import fs from 'node:fs';
import path from 'node:path';
import { newId, now, sha256, jsonParse, oneLine, truncate, normalizeName, copyDir, removeDir, writeFileAtomic, formatBytes } from '../core/util.js';
import { fingerprintOf, identityOf, normalizeManifest } from '../core/manifest.js';
import { ASSET_DIR, isInsideAssetDir, assetEntityDir } from '../core/paths.js';
import { segmentForIndex } from '../core/text-index.js';
import { parseVersion, compareVersions, versionKey } from '../core/util.js';

// 能力注册库：负责能力与安装实例的增删改查、检索文档维护、去重候选、更新检查。
export class Registry {
  constructor(ctx) {
    this.ctx = ctx;
    this.db = ctx.db;
    this.bus = ctx.bus;
    this.logger = ctx.logger;
  }

  // ---------- 写入 ----------
  upsertCapability(item, source) {
    const db = this.db;
    const manifest = normalizeManifest(item.manifest);
    const body = item.body || '';
    const bodyHash = sha256(body);
    const identity = identityOf(manifest);
    const fp = fingerprintOf(manifest, body);
    const ts = now();
    // 安装实例路径与所属范围：scope 由物理位置决定 —— 落在 assets/ 之下 = 能力资产库（自有），
    // 否则 = 宿主能力库（外部扫描登记）。与 migrate() 的回填判据保持一致，避免两种归属口径。
    const instPath = String(item.path || item.entry || '').slice(0, 900);
    const assetDir = this.ctx.options.assetDir || ASSET_DIR;
    const scope = isInsideAssetDir(instPath, assetDir) ? 'own' : 'host';
    // 能力资产库用「资产身份」命名空间（asset:<kind>:<slug>），扫描器用「清单身份」（<kind>:<author>/<name>）。
    // 同一份物理资产若已被资产库登记，这里必须复用其能力记录，否则同一目录会变成两条能力。
    const entityDir = assetEntityDir(instPath, assetDir);
    let ownedExisting = null;
    if (entityDir) {
      for (const r of db.all("SELECT i.path, i.capability_id FROM installations i JOIN capabilities c ON c.id = i.capability_id WHERE c.scope = 'own'")) {
        if (assetEntityDir(r.path, assetDir) === entityDir) {
          ownedExisting = db.get('SELECT * FROM capabilities WHERE id = ?', [r.capability_id]);
          break;
        }
      }
    }
    const existing = ownedExisting || db.get('SELECT * FROM capabilities WHERE identity_key = ?', [identity.key]);
    let capabilityId;
    let created = false;

    if (!existing) {
      capabilityId = newId('cap');
      const mergedTags = manifest.tags || [];
      const mergedHosts = [...new Set([...(manifest.hosts || []), source && source.host].filter(Boolean))];
      db.run(
        `INSERT INTO capabilities(id, kind, canonical_name, display_name, summary, description, author, author_source, homepage, license,
          tags, version, protocols, hosts, identity, identity_key, fingerprint, body_hash, scope, status, health, first_seen, last_seen, updated_at, metadata)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
        [capabilityId, manifest.kind, identity.name, manifest.displayName, manifest.summary, truncate(manifest.description, 20000),
          manifest.author, manifest.authorSource, manifest.homepage, manifest.license, JSON.stringify(mergedTags), manifest.version,
          JSON.stringify(manifest.protocols || []), JSON.stringify(mergedHosts), JSON.stringify(manifest.identity || {}), identity.key,
          fp, bodyHash, scope, 'active', 'unknown', ts, ts, ts, JSON.stringify({ entry: manifest.entry, runtime: manifest.runtime, resources: (manifest.resources || []).slice(0, 60), manifestMeta: manifest.metadata || {} })]
      );
      created = true;
    } else {
      capabilityId = existing.id;
      // 版本更高的写入主体字段；同一版本时保留更完整的描述
      const newer = compareVersions(manifest.version || '0', existing.version || '0') > 0;
      const hosts = new Set([...jsonParse(existing.hosts, []), ...(manifest.hosts || []), source && source.host].filter(Boolean));
      const protocols = new Set([...jsonParse(existing.protocols, []), ...(manifest.protocols || [])]);
      const tags = new Set([...jsonParse(existing.tags, []), ...(manifest.tags || [])]);
      const choose = (next, prev) => (newer && next ? next : (prev || next));
      if (ownedExisting) {
        // 该物理实体属于能力资产库：资产属性（名称/摘要/正文/标签）由 AssetService 拥有，
        // 扫描只做「保活 + 补登记安装实例」，不得用适配器解析出的清单覆盖资产属性。
        db.run("UPDATE capabilities SET scope = 'own', last_seen = ?, updated_at = ? WHERE id = ?", [ts, ts, capabilityId]);
      } else {
        db.run(
          `UPDATE capabilities SET display_name=?, summary=?, description=?, author=?, author_source=?, homepage=?, license=?, version=?, tags=?, protocols=?, hosts=?, fingerprint=?, body_hash=?, last_seen=?, updated_at=?, metadata=?
           WHERE id=?`,
          [
            manifest.displayName || existing.display_name,
            choose(manifest.summary, existing.summary),
            choose(truncate(manifest.description, 20000), existing.description),
            manifest.author || existing.author,
            manifest.author || existing.author_source,
            manifest.homepage || existing.homepage,
            manifest.license || existing.license,
            newer ? manifest.version : existing.version,
            JSON.stringify([...tags]), JSON.stringify([...protocols]), JSON.stringify([...hosts]),
            fp, bodyHash, ts, ts,
            JSON.stringify({ entry: manifest.entry, runtime: manifest.runtime, resources: (manifest.resources || []).slice(0, 60), manifestMeta: manifest.metadata || {} }),
            capabilityId
          ]
        );
        if (scope === 'own' && existing.scope !== 'own') {
          db.run("UPDATE capabilities SET scope = 'own' WHERE id = ?", [capabilityId]);
        }
      }
    }

    // 安装实例
    let inst = db.get('SELECT * FROM installations WHERE capability_id = ? AND path = ?', [capabilityId, instPath]);
    const size = item.dir ? dirSize(item.dir) : (item.entry ? fileSize(item.entry) : 0);
    if (!inst) {
      db.run(
        `INSERT INTO installations(id, capability_id, source_id, host, origin_kind, path, entry, glob, version, format, fingerprint, body_hash, size, mtime, content, manifest, visibility, created_at, updated_at)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
        [newId('ins'), capabilityId, source ? source.id : null, source ? source.host : '', 'discovered', instPath, String(item.entry || '').slice(0, 900), '',
          manifest.version, source ? source.format : '', fp, bodyHash, size, ts, truncate(body, 60000), JSON.stringify(manifest), 'visible', ts, ts]
      );
    } else {
      db.run('UPDATE installations SET fingerprint=?, body_hash=?, version=?, size=?, mtime=?, content=?, manifest=?, updated_at=?, source_id=? WHERE id=?',
        [fp, bodyHash, manifest.version, size, ts, truncate(body, 60000), JSON.stringify(manifest), ts, source ? source.id : null, inst.id]);
    }

    this.updateSearchDoc(capabilityId);
    const fault = this.detectFault(manifest, item);
    if (fault) this.bus.warn('capability', '能力异常：' + manifest.name + ' · ' + fault.title, { capabilityId, code: fault.code });
    return { capabilityId, created, fault };
  }

  updateSearchDoc(capabilityId) {
    const db = this.db;
    const cap = db.get('SELECT * FROM capabilities WHERE id = ?', [capabilityId]);
    if (!cap) return;
    const inst = db.get('SELECT content FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [capabilityId]);
    const body = inst ? inst.content : '';
    const name = segmentForIndex(cap.display_name + ' ' + cap.canonical_name);
    const summary = segmentForIndex(cap.summary);
    const bodySeg = segmentForIndex(body.slice(0, 60000));
    const keywords = segmentForIndex(jsonParse(cap.tags, []).join(' ') + ' ' + jsonParse(cap.protocols, []).join(' '));
    const hosts = segmentForIndex(jsonParse(cap.hosts, []).join(' '));
    const author = segmentForIndex(cap.author || '');
    const existing = db.get('SELECT rowid FROM search_docs WHERE capability_id = ?', [capabilityId]);
    if (existing) {
      db.run('UPDATE search_docs SET kind=?, name=?, summary=?, body=?, keywords=?, author=?, hosts=?, updated_at=? WHERE capability_id=?',
        [cap.kind, name, summary, bodySeg, keywords, author, hosts, now(), capabilityId]);
    } else {
      db.run('INSERT INTO search_docs(capability_id, kind, name, summary, body, keywords, author, hosts, updated_at) VALUES (?,?,?,?,?,?,?,?,?)',
        [capabilityId, cap.kind, name, summary, bodySeg, keywords, author, hosts, now()]);
    }
  }

  reindexSearch() {
    const db = this.db;
    db.tx(() => {
      db.exec('DELETE FROM search_docs');
      const caps = db.all('SELECT id FROM capabilities');
      for (const c of caps) this.updateSearchDoc(c.id);
    });
    this.bus.info('search', '检索索引重建完成');
  }

  detectFault(manifest, item) {
    if (item.parseError) return { code: 'parse-error', title: '清单解析失败' };
    if (!manifest.name) return { code: 'missing-name', title: '缺少名称' };
    if (manifest.kind === 'mcp') {
      const rt = manifest.runtime || {};
      if (rt.transport === 'stdio' && rt.command && !isExecutableLike(rt.command)) {
        return { code: 'mcp-command-missing', title: 'MCP 启动命令不存在：' + rt.command };
      }
    }
    return null;
  }

  // ---------- 读取 ----------
  listCapabilities(filter) {
    const f = filter || {};
    const where = [];
    const params = [];
    if (f.scope) { where.push('scope = ?'); params.push(f.scope); }
    if (f.kind) { where.push('kind = ?'); params.push(f.kind); }
    if (f.status) { where.push('status = ?'); params.push(f.status); }
    if (f.health) { where.push('health = ?'); params.push(f.health); }
    if (f.host) { where.push("hosts LIKE ?"); params.push('%' + f.host + '%'); }
    if (f.q) {
      where.push('(canonical_name LIKE ? OR display_name LIKE ? OR summary LIKE ? OR author LIKE ?)');
      const like = '%' + f.q + '%';
      params.push(like, like, like, like);
    }
    const sql = 'SELECT * FROM capabilities ' + (where.length ? 'WHERE ' + where.join(' AND ') : '') +
      ' ORDER BY ' + (f.sort === 'usage' ? 'usage_count DESC, updated_at DESC' : 'updated_at DESC') + ' LIMIT ? OFFSET ?';
    params.push(f.limit || 200, f.offset || 0);
    const rows = this.db.all(sql, params);
    const upd = new Map();
    try {
      for (const u of this.db.all("SELECT capability_id, status, latest_version FROM updates WHERE status IN ('outdated','ahead','current')")) {
        upd.set(u.capability_id, { status: u.status, latestVersion: u.latest_version });
      }
    } catch { /* updates 表可能尚未写入 */ }
    return rows.map((r) => {
      const cap = this.hydrate(r);
      const u = upd.get(r.id);
      if (u) { cap.updateStatus = u.status; cap.latestVersion = u.latestVersion; }
      return cap;
    });
  }

  hydrate(row) {
    if (!row) return null;
    return {
      id: row.id,
      kind: row.kind,
      scope: row.scope,
      name: row.canonical_name,
      displayName: row.display_name,
      summary: row.summary,
      description: row.description,
      author: row.author,
      authorSource: row.author_source,
      homepage: row.homepage,
      license: row.license,
      tags: jsonParse(row.tags, []),
      version: row.version,
      protocols: jsonParse(row.protocols, []),
      hosts: jsonParse(row.hosts, []),
      status: row.status,
      health: row.health,
      verifyState: row.verify_state,
      verifyMessage: row.verify_message,
      repairState: row.repair_state,
      archivePath: row.archive_path,
      managed: Boolean(row.managed),
      pinned: Boolean(row.pinned),
      usageCount: row.usage_count,
      lastUsedAt: row.last_used_at,
      firstSeen: row.first_seen,
      lastSeen: row.last_seen,
      updatedAt: row.updated_at,
      metadata: jsonParse(row.metadata, {})
    };
  }

  getCapability(id) {
    const row = this.db.get('SELECT * FROM capabilities WHERE id = ?', [id]);
    if (!row) return null;
    const cap = this.hydrate(row);
    cap.installations = this.db.all('SELECT * FROM installations WHERE capability_id = ?', [id]).map((i) => ({
      id: i.id, path: i.path, entry: i.entry, host: i.host, originKind: i.origin_kind, version: i.version,
      size: i.size, visibility: i.visibility, fingerprint: i.fingerprint, updatedAt: i.updated_at, content: i.content,
      manifest: jsonParse(i.manifest, {})
    }));
    cap.faults = this.db.all("SELECT * FROM faults WHERE capability_id = ? AND status != 'resolved' ORDER BY detected_at DESC", [id]);
    cap.updates = this.db.get('SELECT * FROM updates WHERE capability_id = ?', [id]);
    cap.usage = this.db.all('SELECT * FROM capability_usage WHERE capability_id = ? ORDER BY ts DESC LIMIT 20', [id]).map((u) => ({
      id: u.id, agentId: u.agent_id, action: u.action, ts: u.ts, detail: jsonParse(u.detail, {})
    }));
    return cap;
  }

  stats() {
    const db = this.db;
    const byKind = db.all('SELECT kind, COUNT(*) AS c FROM capabilities GROUP BY kind');
    // 按宿主统计「能力」数量（从 hosts JSON 展开），而不是来源数量
    const hostCounts = new Map();
    for (const row of db.all('SELECT hosts FROM capabilities WHERE status != \'archived\'')) {
      let list = [];
      try { list = JSON.parse(row.hosts || '[]'); } catch { list = []; }
      for (const h of list) hostCounts.set(h, (hostCounts.get(h) || 0) + 1);
    }
    const byHost = [...hostCounts.entries()].map(([host, c]) => ({ host, c })).sort((a, b) => b.c - a.c);
    const byHostSources = db.all('SELECT host, COUNT(*) AS c FROM sources GROUP BY host');
    const status = db.all('SELECT status, COUNT(*) AS c FROM capabilities GROUP BY status');
    const health = db.all('SELECT health, COUNT(*) AS c FROM capabilities GROUP BY health');
    return {
      total: db.get('SELECT COUNT(*) AS c FROM capabilities').c,
      installations: db.get('SELECT COUNT(*) AS c FROM installations').c,
      sources: db.get('SELECT COUNT(*) AS c FROM sources').c,
      byKind, byHost, byHostSources, status, health,
      faultsOpen: db.get("SELECT COUNT(*) AS c FROM faults WHERE status != 'resolved' AND status != 'ignored'").c,
      duplicatesPending: db.get("SELECT COUNT(*) AS c FROM duplicate_groups WHERE decision = 'pending'").c,
      updatesAvailable: db.get("SELECT COUNT(*) AS c FROM updates WHERE status = 'outdated'").c,
      agentsRunning: db.get("SELECT COUNT(*) AS c FROM agents WHERE status = 'running'").c,
      byScope: safeAll(db, 'SELECT scope, COUNT(*) AS c FROM capabilities GROUP BY scope')
    };
  }
}

function safeAll(db, sql) {
  try { return db.all(sql); } catch { return []; }
}

function dirSize(dir) {
  let total = 0;
  const stack = [dir];
  while (stack.length) {
    const cur = stack.pop();
    let entries = [];
    try { entries = fs.readdirSync(cur, { withFileTypes: true }); } catch { continue; }
    for (const ent of entries) {
      const full = path.join(cur, ent.name);
      if (ent.isDirectory()) { if (stack.length < 5000) stack.push(full); }
      else if (ent.isFile()) { try { total += fs.statSync(full).size; } catch { /* ignore */ } }
    }
  }
  return total;
}

function fileSize(file) {
  try { return fs.statSync(file).size; } catch { return 0; }
}

function isExecutableLike(cmd) {
  if (!cmd) return true;
  if (path.isAbsolute(cmd)) return fs.existsSync(cmd);
  return true; // PATH 中的命令无法在此断言
}
