import { DatabaseSync } from 'node:sqlite';
import fs from 'node:fs';
import path from 'node:path';
import { SCHEMA, SCHEMA_VERSION } from './schema.js';
import { jsonParse, now } from '../core/util.js';
import { ASSET_DIR, isInsideAssetDir } from '../core/paths.js';

let instance = null;

export class Database {
  constructor(file, options) {
    this.file = file;
    this.options = options || {};
    fs.mkdirSync(path.dirname(file), { recursive: true });
    this.conn = new DatabaseSync(file, { timeout: 8000 });
    this.conn.exec('PRAGMA busy_timeout = 8000;');
    this._txDepth = 0;
  }

  static open(file, options) {
    if (instance && instance.file === file && !instance.closed) return instance;
    const db = new Database(file, options);
    db.migrate();
    instance = db;
    return db;
  }

  static current() { return instance; }

  // 幂等迁移：先建缺失的表/索引，再做「列级」增量迁移。
  // 老库（schema_version=12，已有 313 条能力）升级到 13 时不得丢数据。
  migrate() {
    this.conn.exec(SCHEMA);
    const row = this.get('SELECT value FROM meta WHERE key = ?', ['schema_version']);
    const prev = row ? Number(row.value) : 0;
    const applied = [];
    if (!this.hasColumn('capabilities', 'scope')) {
      this.exec("ALTER TABLE capabilities ADD COLUMN scope TEXT NOT NULL DEFAULT 'host'");
      applied.push('capabilities.scope');
    }
    // 已有列也要确保索引存在（老库的 CREATE INDEX IF NOT EXISTS 在 SCHEMA 中已覆盖）；
    // scope 列可能是「本进程刚加上」的，此时需要按物理位置回填一次。
    if (applied.includes('capabilities.scope')) {
      const n = this.backfillScope();
      if (n) applied.push('capabilities.scope:backfilled=' + n);
    }
    // 索引必须在 scope 列确定存在之后再建（老库首次升级时列刚被 ALTER 出来）
    this.exec('CREATE INDEX IF NOT EXISTS idx_cap_scope ON capabilities(scope, kind, status)');
    if (prev === 0) {
      this.setMeta('schema_version', String(SCHEMA_VERSION));
      this.setMeta('created_at', String(now()));
    } else if (prev !== SCHEMA_VERSION) {
      this.setMeta('schema_version', String(SCHEMA_VERSION));
      this.setMeta('migrated_from', String(prev));
    }
    if (applied.length) this.setMeta('last_migration', JSON.stringify({ at: now(), from: prev, to: SCHEMA_VERSION, steps: applied }));
    this.setMeta('app_version', '1.0.0');
    this.conn.exec('PRAGMA optimize;');
    return { from: prev, to: SCHEMA_VERSION, steps: applied };
  }

  hasColumn(table, column) {
    try {
      const rows = this.conn.prepare('SELECT name FROM pragma_table_info(?)').all(table);
      return rows.some((r) => r.name === column);
    } catch {
      return false;
    }
  }

  // 按安装路径是否位于 ASSET_DIR 之下回填 scope：own = 能力资产库（自有），host = 宿主能力库（外部）。
  backfillScope(assetDir) {
    let own = new Set();
    try {
      for (const r of this.all('SELECT capability_id, path FROM installations')) {
        if (isInsideAssetDir(r.path, assetDir || ASSET_DIR)) own.add(r.capability_id);
      }
    } catch { own = new Set(); }
    let changed = 0;
    for (const id of own) {
      try { this.run("UPDATE capabilities SET scope = 'own' WHERE id = ? AND scope != 'own'", [id]); changed++; } catch { /* ignore */ }
    }
    return changed;
  }

  setMeta(key, value) {
    this.run('INSERT INTO meta(key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value', [key, String(value)]);
  }

  getMeta(key, fallback) {
    const row = this.get('SELECT value FROM meta WHERE key = ?', [key]);
    return row ? row.value : fallback;
  }

  prepare(sql) { return this.conn.prepare(sql); }

  all(sql, params) {
    const stmt = this.conn.prepare(sql);
    return params ? stmt.all(...params) : stmt.all();
  }

  get(sql, params) {
    const stmt = this.conn.prepare(sql);
    return params ? stmt.get(...params) : stmt.get();
  }

  run(sql, params) {
    const stmt = this.conn.prepare(sql);
    const res = params ? stmt.run(...params) : stmt.run();
    return res;
  }

  exec(sql) { this.conn.exec(sql); }

  tx(fn) {
    if (this._txDepth > 0) return fn();
    this.conn.exec('BEGIN IMMEDIATE');
    this._txDepth++;
    try {
      const result = fn();
      this.conn.exec('COMMIT');
      this._txDepth--;
      return result;
    } catch (err) {
      this._txDepth--;
      try { this.conn.exec('ROLLBACK'); } catch { /* ignore */ }
      throw err;
    }
  }

  jget(sql, params, key) {
    const row = this.get(sql, params);
    return row ? jsonParse(row[key || 'data'], {}) : null;
  }

  backupTo(target) {
    // 使用 VACUUM INTO 做一致性快照
    fs.mkdirSync(path.dirname(target), { recursive: true });
    if (fs.existsSync(target)) fs.rmSync(target);
    this.conn.exec("VACUUM INTO '" + target.replace(/'/g, "''") + "'");
    return target;
  }

  stats() {
    const tables = ['capabilities', 'installations', 'sources', 'faults', 'duplicate_groups', 'agents', 'updates', 'events', 'workflow_runs'];
    const out = {};
    for (const t of tables) {
      try { out[t] = this.get('SELECT COUNT(*) AS c FROM ' + t).c; } catch { out[t] = 0; }
    }
    try {
      const st = fs.statSync(this.file);
      out.dbBytes = st.size;
      out.walBytes = fs.existsSync(this.file + '-wal') ? fs.statSync(this.file + '-wal').size : 0;
    } catch { out.dbBytes = 0; out.walBytes = 0; }
    out.sqliteVersion = this.get('SELECT sqlite_version() AS v').v;
    return out;
  }

  close() {
    if (this.closed) return;
    try { this.conn.exec('PRAGMA wal_checkpoint(TRUNCATE);'); } catch { /* ignore */ }
    try { this.conn.close(); } catch { /* ignore */ }
    this.closed = true;
    if (instance === this) instance = null;
  }
}

// 老库就地接管：产品改名后默认数据库文件名也变了。若目标库不存在，而数据目录下
// 还留着一份「旧版本模式的库」，就把它整体改名为新库，避免升级后看起来数据全没了。
//
// 判据刻意不依赖文件名（文件名属于历史遗留，会随产品改名而变），而是认数据库自身的特征：
// 有 capabilities 表、schema_version 低于当前版本。这样任何来源的老库都能被识别。
export function findLegacyDatabase(targetFile) {
  const dir = path.dirname(targetFile);
  const target = path.resolve(targetFile);
  let best = null;
  let entries = [];
  try { entries = fs.readdirSync(dir); } catch { return null; }
  for (const name of entries) {
    if (!name.toLowerCase().endsWith('.db')) continue;
    const full = path.join(dir, name);
    if (path.resolve(full) === target) continue;
    let conn = null;
    try {
      conn = new DatabaseSync(full, { readOnly: true });
      const hasCaps = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='capabilities'").get();
      if (!hasCaps) continue;
      const verRow = conn.prepare("SELECT value FROM meta WHERE key='schema_version'").get();
      const version = verRow ? Number(verRow.value) : 0;
      if (version >= SCHEMA_VERSION) continue;
      const count = conn.prepare('SELECT COUNT(*) AS c FROM capabilities').get().c;
      if (!best || count > best.capabilities) best = { path: full, version, capabilities: count };
    } catch {
      /* 不是本产品的库，跳过 */
    } finally {
      try { if (conn) conn.close(); } catch { /* ignore */ }
    }
  }
  return best;
}

export function adoptLegacyDatabase(file) {
  try {
    if (fs.existsSync(file)) return { adopted: false };
    const legacy = findLegacyDatabase(file);
    if (!legacy) return { adopted: false };
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.renameSync(legacy.path, file);
    for (const suffix of ['-wal', '-shm']) {
      const oldSide = legacy.path + suffix;
      if (fs.existsSync(oldSide)) {
        try { fs.renameSync(oldSide, file + suffix); } catch { /* 侧车文件改名失败不致命 */ }
      }
    }
    return { adopted: true, from: legacy.path, to: file, capabilities: legacy.capabilities, fromVersion: legacy.version };
  } catch (err) {
    return { adopted: false, error: String(err && err.message) };
  }
}

export function openDatabase(file, options) {
  adoptLegacyDatabase(file);
  return Database.open(file, options);
}
