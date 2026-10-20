import { execFile } from 'node:child_process';
import path from 'node:path';
import { now, newId, jsonParse, compareVersions, readTextSafe, writeFileAtomic } from '../core/util.js';
import { USER_HOME } from '../core/paths.js';

// 更新识别：三种证据
// 1) DSH Market 的 discovery 兼容性缓存（本机已有第三方插件事实，速度最快）；
// 2) node_modules 内已安装包自带的 version（本地事实）；
// 3) npm registry（可选联网，带并发上限与 6 小时缓存）。
const NPM_TTL_MS = 6 * 60 * 60 * 1000;

export class UpdateService {
  constructor(ctx) {
    this.ctx = ctx;
    this.db = ctx.db;
    this.bus = ctx.bus;
    this.npmCache = new Map();
  }

  dshMarketCache() {
    // 默认位置按当前用户主目录推导，不硬编码任何具体用户名
    const defaultCache = path.join(USER_HOME, '.dsh', 'profiles', 'desktop', '.dsh-market', 'discovery-compatibility-v1.json');
    const file = path.join(this.ctx.options.marketCache || defaultCache);
    const text = readTextSafe(file);
    if (!text) return null;
    try { return JSON.parse(text).entries || null; } catch { return null; }
  }

  // 把 DSH Market 兼容性缓存导入 registry_catalog，作为「可安装/可更新」目录
  importCatalog() {
    const entries = this.dshMarketCache();
    if (!entries) return { imported: 0 };
    const ts = now();
    let imported = 0;
    for (const [pkg, entry] of Object.entries(entries)) {
      const facts = entry.facts || {};
      const installed = this.db.get('SELECT id FROM capabilities WHERE canonical_name = ? OR canonical_name LIKE ?', [pkg, '%/' + pkg]);
      this.db.run(
        `INSERT INTO registry_catalog(id, registry, package_name, version, engines_dsh, peer_count, checked_at, installed, capability_id, metadata)
         VALUES (?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(registry, package_name) DO UPDATE SET version=excluded.version, engines_dsh=excluded.engines_dsh,
           peer_count=excluded.peer_count, checked_at=excluded.checked_at, installed=excluded.installed, capability_id=excluded.capability_id, metadata=excluded.metadata`,
        [newId('cat'), 'dsh-market', pkg, String(facts.version || ''), String(facts.enginesDsh || ''),
          Object.keys(facts.peerDependencies || {}).length, entry.checkedAt || ts, installed ? 1 : 0, installed ? installed.id : null, JSON.stringify(facts)]);
      imported++;
    }
    this.bus.info('update', '已导入上游目录 ' + imported + ' 条（DSH Market）');
    return { imported };
  }

  catalog(query, options) {
    const opts = options || {};
    const where = [];
    const params = [];
    if (query) { where.push('(package_name LIKE ? OR metadata LIKE ?)'); params.push('%' + query + '%', '%' + query + '%'); }
    if (opts.onlyInstalled) where.push('installed = 1');
    const sql = 'SELECT * FROM registry_catalog ' + (where.length ? 'WHERE ' + where.join(' AND ') : '') +
      ' ORDER BY installed DESC, package_name LIMIT ? OFFSET ?';
    params.push(opts.limit || 100, opts.offset || 0);
    return this.db.all(sql, params).map((r) => ({
      id: r.id, registry: r.registry, packageName: r.package_name, version: r.version, enginesDsh: r.engines_dsh,
      peerCount: r.peer_count, checkedAt: r.checked_at, installed: Boolean(r.installed), capabilityId: r.capability_id,
      metadata: jsonParse(r.metadata, {})
    }));
  }

  async checkAll(options) {
    const opts = options || {};
    const caps = this.db.all("SELECT * FROM capabilities WHERE status != 'archived'");
    const entries = this.dshMarketCache();
    let checked = 0; let outdated = 0; let failed = 0; let skipped = 0;
    const concurrency = Math.max(1, Math.min(8, opts.concurrency || 6));
    const queue = [...caps];
    const workers = Array.from({ length: concurrency }, async () => {
      while (queue.length) {
        const cap = queue.shift();
        if (!cap) break;
        const res = await this.checkOne(cap, entries, opts);
        checked++;
        if (res.status === 'outdated') outdated++;
        if (res.status === 'check-failed') failed++;
        if (res.status === 'unknown') skipped++;
      }
    });
    await Promise.all(workers);
    this.bus.info('update', '更新检查完成：' + checked + ' 项，' + outdated + ' 个可更新，' + skipped + ' 个无上游信息，' + failed + ' 个失败');
    return { checked, outdated, failed, skipped };
  }

  async checkOne(cap, marketEntries, opts) {
    const ts = now();
    let latest = '';
    let sourceKind = 'local';
    let detail = '';
    let status = 'unknown';
    // 1) DSH Market 缓存
    if (marketEntries && marketEntries[cap.canonical_name]) {
      const entry = marketEntries[cap.canonical_name];
      latest = String((entry.facts && entry.facts.version) || '');
      if (latest) {
        sourceKind = 'dsh-market';
        detail = 'DSH Market 兼容性缓存（检查于 ' + new Date(entry.checkedAt).toISOString().slice(0, 10) + '）';
      }
    }
    // 2) npm registry（可选）
    if (!latest && opts.useNpm && isRegistryName(cap.canonical_name)) {
      const localVersion = this.readLocalInstalledVersion(cap);
      if (localVersion && !cap.version) {
        this.db.run('UPDATE capabilities SET version=?, updated_at=? WHERE id=?', [localVersion, now(), cap.id]);
        cap.version = localVersion;
        sourceKind = 'local';
        detail = '本机 node_modules 中记录的版本';
      }
      const npmVersion = await this.npmLatest(cap.canonical_name, opts.timeoutMs || 8000);
      if (npmVersion) { latest = npmVersion; sourceKind = 'npm'; detail = 'npm registry 最新版'; }
      else if (localVersion) { detail = '仅有本地版本信息，上游查询失败或超时'; }
    }
    if (latest) {
      const cmp = compareVersions(latest, cap.version || '0');
      if (cmp > 0) status = 'outdated';
      else if (cmp === 0) status = 'current';
      else status = 'ahead';
    }
    const existing = this.db.get('SELECT id FROM updates WHERE capability_id = ?', [cap.id]);
    if (existing) {
      this.db.run('UPDATE updates SET source_kind=?, current_version=?, latest_version=?, status=?, detail=?, checked_at=? WHERE id=?',
        [sourceKind, cap.version, latest, status, detail, ts, existing.id]);
    } else {
      this.db.run('INSERT INTO updates(id, capability_id, source_kind, current_version, latest_version, status, detail, checked_at) VALUES (?,?,?,?,?,?,?,?)',
        [newId('upd'), cap.id, sourceKind, cap.version, latest, status, detail, ts]);
    }
    return { capabilityId: cap.id, status, current: cap.version, latest, sourceKind, detail };
  }

  // 从安装路径的 node_modules/<name>/package.json 读版本（离线事实）
  readLocalInstalledVersion(cap) {
    const inst = this.db.get('SELECT path FROM installations WHERE capability_id = ? AND path LIKE ? LIMIT 1', [cap.id, '%node_modules%']);
    if (!inst) return '';
    const pkgPath = path.join(String(inst.path).split('#')[0], 'package.json');
    try { return JSON.parse(readTextSafe(pkgPath) || '{}').version || ''; } catch { return ''; }
  }

  npmLatest(name, timeoutMs) {
    const cached = this.npmCache.get(name);
    if (cached && Date.now() - cached.at < NPM_TTL_MS) return Promise.resolve(cached.version);
    return new Promise((resolve) => {
      execFile(process.platform === 'win32' ? 'npm.cmd' : 'npm', ['view', name, 'version', '--json'], { timeout: timeoutMs, windowsHide: true }, (err, stdout) => {
        if (err) return resolve(null);
        let version = null;
        try { version = String(JSON.parse(stdout.trim())); } catch { version = stdout.trim() || null; }
        if (version) this.npmCache.set(name, { version, at: Date.now() });
        resolve(version);
      }).on('error', () => resolve(null));
    });
  }

  list(filter) {
    const rows = this.db.all(
      `SELECT u.*, c.display_name, c.canonical_name, c.kind, c.status AS cap_status
       FROM updates u JOIN capabilities c ON c.id = u.capability_id
       ${filter && filter.status ? 'WHERE u.status = ?' : ''} ORDER BY CASE u.status WHEN 'outdated' THEN 0 WHEN 'check-failed' THEN 1 WHEN 'current' THEN 2 ELSE 3 END, u.checked_at DESC LIMIT 400`,
      filter && filter.status ? [filter.status] : []);
    return rows.map((r) => ({
      id: r.id, capabilityId: r.capability_id, name: r.display_name, canonicalName: r.canonical_name, kind: r.kind,
      sourceKind: r.source_kind, currentVersion: r.current_version, latestVersion: r.latest_version,
      status: r.status, detail: r.detail, checkedAt: r.checked_at, appliedAt: r.applied_at
    }));
  }
}

function isRegistryName(name) {
  return /^(@[a-z0-9][a-z0-9-_.]*\/)?[a-z0-9][a-z0-9-_.]*$/i.test(String(name || ''));
}
