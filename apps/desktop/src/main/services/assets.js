import fs from 'node:fs';
import path from 'node:path';
import { newId, now, readTextSafe, writeFileAtomic, copyDir, removeDir, jsonParse, oneLine, sha256 } from '../core/util.js';
import { ASSET_DIR, TRASH_DIR, ASSET_KINDS } from '../core/paths.js';
import { ensureAssetSeed } from './seed.js';

// 能力资产库服务（基线 §5 / §10）
// 能力资产 = 能力核心自己持有、可维修、可派发、可直接提供给接入智能体的能力实体。
// 双重登记：物理目录 assets/<kind>/<id>/ + 数据库 capabilities.scope='own' + installations 记录。
// 与「宿主能力库」（scope='host'，从各宿主扫描登记）严格区分；转化方向 host --promote--> own。

const ENTRY_BY_KIND = {
  skill: 'SKILL.md',
  mcp: 'mcp.json',
  plugin: 'plugin.json',
  prompt: 'PROMPT.md'
};

export function entryFileFor(kind) {
  return ENTRY_BY_KIND[kind] || 'README.md';
}

function slugOf(name) {
  return String(name || '').trim().replace(/[^a-zA-Z0-9._\u4e00-\u9fff-]/g, '-').replace(/-+/g, '-').replace(/^-|-$/g, '') || 'asset';
}

// skill 资产必须带 frontmatter，否则宿主无法识别；缺省时按命名自动补
function ensureSkillFrontmatter(name, content, summary) {
  const text = String(content || '');
  if (/^\s*---\s*\n/.test(text)) return text;
  const lines = ['---', 'name: ' + slugOf(name), 'description: ' + oneLine(summary || name, 200), 'version: 1.0.0', 'author: 能力核心', '---', ''];
  return lines.join('\n') + text;
}

export class AssetService {
  constructor(ctx, services) {
    this.ctx = ctx;
    this.svc = services;
    this.db = ctx.db;
    this.bus = ctx.bus;
  }

  assetDir() {
    return this.ctx.options.assetDir || ASSET_DIR;
  }

  trashDir() {
    return path.join(this.assetDir(), '.trash');
  }

  // 首次运行放置元能力资产（幂等）
  ensureSeed() {
    try { ensureAssetSeed(this.assetDir()); } catch { /* 种子失败不应阻止启动 */ }
  }

  // 启动后把物理目录里的资产补登记进数据库（目录是事实来源，数据库可重建）
  syncFromDisk() {
    this.ensureSeed();
    let registered = 0;
    for (const kind of ASSET_KINDS) {
      const root = path.join(this.assetDir(), kind);
      let entries = [];
      try { entries = fs.readdirSync(root, { withFileTypes: true }); } catch { continue; }
      for (const ent of entries) {
        if (!ent.isDirectory()) continue;
        const dir = path.join(root, ent.name);
        const entry = path.join(dir, entryFileFor(kind));
        if (!fs.existsSync(entry)) continue;
        const existing = this.db.get('SELECT id FROM installations WHERE path = ?', [dir]);
        if (existing) continue;
        try {
          this.registerOwn({ kind, name: ent.name, dir, entry, content: readTextSafe(entry) || '', summary: '' });
          registered++;
        } catch { /* 单条失败不影响其余 */ }
      }
    }
    return { registered };
  }

  // ---------- 读取 ----------
  list(filter) {
    const f = filter || {};
    const where = ["scope = 'own'", "status != 'archived'"];
    const params = [];
    if (f.kind) { where.push('kind = ?'); params.push(f.kind); }
    if (f.query) {
      where.push('(display_name LIKE ? OR canonical_name LIKE ? OR summary LIKE ?)');
      const like = '%' + f.query + '%';
      params.push(like, like, like);
    }
    const limit = Math.min(Number(f.limit) || 200, 1000);
    const total = this.db.get('SELECT COUNT(*) c FROM capabilities WHERE ' + where.join(' AND '), params).c;
    const rows = this.db.all('SELECT * FROM capabilities WHERE ' + where.join(' AND ') + ' ORDER BY updated_at DESC LIMIT ?', [...params, limit]);
    const items = rows.map((r) => {
      const inst = this.db.get('SELECT path, size FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [r.id]);
      return {
        id: r.id,
        kind: r.kind,
        scope: r.scope,
        name: r.canonical_name,
        displayName: r.display_name,
        summary: r.summary,
        version: r.version,
        tags: jsonParse(r.tags, []),
        status: r.status,
        health: r.health,
        path: inst ? inst.path : '',
        size: inst ? inst.size : 0,
        updatedAt: r.updated_at
      };
    });
    return { items, total };
  }

  // 读取单个能力资产内容。id 可以是能力 id，也可以是资产目录名。
  get(id) {
    const key = String(id || '');
    if (!key) throw new Error('缺少 id');
    let cap = this.db.get("SELECT * FROM capabilities WHERE id = ? AND scope = 'own'", [key]);
    if (!cap) {
      const inst = this.db.get('SELECT capability_id FROM installations WHERE path LIKE ?', ['%' + path.sep + key + '%']);
      if (inst) cap = this.db.get("SELECT * FROM capabilities WHERE id = ? AND scope = 'own'", [inst.capability_id]);
    }
    if (!cap) throw new Error('能力资产不存在：' + key);
    const inst = this.db.get('SELECT * FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [cap.id]);
    let content = '';
    let dirPath = inst ? inst.path : '';
    let filePath = '';
    if (dirPath && fs.existsSync(dirPath)) {
      const entry = path.join(dirPath, entryFileFor(cap.kind));
      if (fs.existsSync(entry)) { filePath = entry; content = readTextSafe(entry) || ''; }
      else {
        // 非标准资产：回退到目录下第一个文本文件
        const first = fs.readdirSync(dirPath).find((f) => /\.(md|json|txt|yaml|yml)$/i.test(f));
        if (first) { filePath = path.join(dirPath, first); content = readTextSafe(filePath) || ''; }
      }
    }
    if (!content && inst) content = inst.content || '';
    return {
      id: cap.id,
      kind: cap.kind,
      title: cap.display_name,
      name: cap.canonical_name,
      summary: cap.summary,
      path: filePath || dirPath,
      dir: dirPath,
      entry: filePath,
      content,
      version: cap.version,
      updatedAt: cap.updated_at
    };
  }

  stats() {
    const dir = this.assetDir();
    const total = this.db.get("SELECT COUNT(*) c FROM capabilities WHERE scope = 'own' AND status != 'archived'").c;
    let byKind = [];
    try {
      byKind = this.db.all("SELECT kind, COUNT(*) c FROM capabilities WHERE scope = 'own' AND status != 'archived' GROUP BY kind");
    } catch { byKind = []; }
    let fileCount = 0;
    let sizeBytes = 0;
    const scan = (dirPath) => {
      let entries = [];
      try { entries = fs.readdirSync(dirPath, { withFileTypes: true }); } catch { return; }
      for (const ent of entries) {
        if (ent.name === '.trash') continue;
        const full = path.join(dirPath, ent.name);
        if (ent.isDirectory()) scan(full);
        else if (ent.isFile()) { fileCount++; try { sizeBytes += fs.statSync(full).size; } catch { /* ignore */ } }
      }
    };
    scan(dir);
    return { total, byKind, assetDir: dir, sizeBytes, fileCount };
  }

  // ---------- 写入 ----------
  // 新建能力资产：分配 id → 建目录 → 写入口文件 → 登记能力与安装实例
  create(payload) {
    const p = payload || {};
    const kind = ASSET_KINDS.includes(p.kind) ? p.kind : 'skill';
    const name = String(p.name || '').trim() || ('asset-' + Date.now().toString(36));
    const id = newId('ast');
    const dir = path.join(this.assetDir(), kind, id);
    fs.mkdirSync(dir, { recursive: true });
    let content = String(p.content == null ? '' : p.content);
    if (kind === 'skill') content = ensureSkillFrontmatter(name, content, p.summary);
    if (kind === 'mcp' && !content.trim()) content = JSON.stringify({ mcpServers: {} }, null, 2);
    if (kind === 'plugin' && !content.trim()) content = JSON.stringify({ name: slugOf(name), version: '1.0.0' }, null, 2);
    const entry = path.join(dir, entryFileFor(kind));
    writeFileAtomic(entry, content);
    const registered = this.registerOwn({ id, kind, name, dir, entry, content, summary: p.summary || '' });
    this.bus && this.bus.info('assets', '已创建能力资产：' + name, { assetId: registered.id, path: dir });
    return { id: registered.id, path: dir, entry };
  }

  // 宿主能力 → 能力资产（复制实体到 assets/，登记来源，保留追溯）
  promote(payload) {
    const p = payload || {};
    const sourceId = p.capabilityId;
    if (!sourceId) throw new Error('缺少 capabilityId');
    const src = this.db.get('SELECT * FROM capabilities WHERE id = ?', [sourceId]);
    if (!src) throw new Error('能力不存在：' + sourceId);

    // 幂等：同一宿主能力已转化过就直接返回既有资产，不重复创建（除非显式 overwrite）。
    const already = this.findPromoted(sourceId);
    if (already && !p.overwrite) {
      const inst = this.db.get('SELECT path FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [already.id]);
      this.bus && this.bus.info('assets', '能力资产已存在，复用：' + already.display_name, { assetId: already.id, from: sourceId });
      return { assetId: already.id, path: inst ? inst.path : '', from: sourceId, existed: true };
    }

    // 类型对齐：assets/ 物理布局只有 skill|mcp|plugin|prompt 四种实体形态。
    // 宿主侧的 agent 等类型无法直接物化，必须显式降级并如实回报，不允许静默伪装。
    const requestedKind = p.kind || src.kind;
    const kind = ASSET_KINDS.includes(requestedKind) ? requestedKind : 'skill';
    const kindCoerced = kind !== requestedKind;

    const srcInst = this.db.get('SELECT * FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [sourceId]);
    // overwrite 复用既有记录与目录，保持「一个来源能力 ↔ 一个资产」的对应关系
    const id = already ? already.id : newId('ast');
    const existingInst = already ? this.db.get('SELECT path FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [already.id]) : null;
    const dir = existingInst && existingInst.path ? existingInst.path : path.join(this.assetDir(), kind, id);
    fs.mkdirSync(dir, { recursive: true });

    // 复制实体：目录整体复制；单文件则落到标准入口文件名
    let content = (srcInst && srcInst.content) || '';
    if (srcInst && srcInst.path) {
      const raw = String(srcInst.path).split('#')[0];
      try {
        if (fs.existsSync(raw)) {
          if (fs.statSync(raw).isDirectory()) {
            copyDir(raw, dir);
            const preferred = path.join(raw, entryFileFor(kind));
            if (fs.existsSync(preferred)) content = readTextSafe(preferred) || content;
          } else {
            const dest = path.join(dir, entryFileFor(kind));
            fs.copyFileSync(raw, dest);
            content = readTextSafe(dest) || content;
          }
        }
      } catch { /* 复制失败时回退为数据库内的正文快照 */ }
    }
    const entry = path.join(dir, entryFileFor(kind));
    if (!fs.existsSync(entry)) {
      writeFileAtomic(entry, kind === 'skill' ? ensureSkillFrontmatter(src.display_name, content, src.summary) : content);
    }
    const registered = this.registerOwn({
      id,
      kind,
      name: src.display_name || src.canonical_name,
      dir,
      entry,
      content: readTextSafe(entry) || content,
      summary: src.summary,
      tags: jsonParse(src.tags, []),
      version: src.version,
      author: src.author,
      metadata: { promotedFrom: sourceId, promotedAt: now(), sourceKind: src.kind, kindCoerced }
    });
    this.bus && this.bus.info('assets', '已转为能力资产：' + (src.display_name || src.canonical_name), { assetId: registered.id, from: sourceId, kindCoerced });
    return {
      assetId: registered.id,
      path: dir,
      from: sourceId,
      existed: false,
      kind,
      kindCoerced,
      // 降级时明确告知调用方原始类型，便于 UI 提示而不是静默伪装
      ...(kindCoerced ? { fromKind: src.kind, toKind: kind, note: '能力资产库只支持 skill/mcp/plugin/prompt 四种实体形态，' + src.kind + ' 已按 ' + kind + ' 转化' } : {})
    };
  }

  // 按来源追溯查找已转化的自有资产
  findPromoted(sourceId) {
    const rows = this.db.all("SELECT * FROM capabilities WHERE scope = 'own' AND status != 'archived'");
    for (const r of rows) {
      const meta = jsonParse(r.metadata, {});
      if (meta && meta.promotedFrom === sourceId) return r;
    }
    return null;
  }

  // 删除能力资产：物理目录移入 assets/.trash，数据库记录保留为 archived（可追溯）
  remove(payload) {
    const id = typeof payload === 'string' ? payload : (payload && payload.id);
    const cap = this.db.get("SELECT * FROM capabilities WHERE id = ? AND scope = 'own'", [id]);
    if (!cap) throw new Error('能力资产不存在：' + id);
    const insts = this.db.all('SELECT * FROM installations WHERE capability_id = ?', [cap.id]);
    const moved = [];
    for (const inst of insts) {
      const raw = String(inst.path || '').split('#')[0];
      if (!raw || !fs.existsSync(raw)) continue;
      const dest = path.join(this.trashDir(), newId('trash') + '-' + path.basename(raw));
      fs.mkdirSync(this.trashDir(), { recursive: true });
      try {
        fs.renameSync(raw, dest);
      } catch {
        try { copyDir(raw, dest); removeDir(raw); } catch { continue; }
      }
      moved.push({ from: raw, to: dest });
    }
    this.db.run("UPDATE capabilities SET status = 'archived', archive_path = ?, updated_at = ? WHERE id = ?", [JSON.stringify(moved), now(), cap.id]);
    this.bus && this.bus.warn('assets', '已删除能力资产（移入回收站）：' + cap.display_name, { assetId: cap.id, moved });
    return { ok: true, id: cap.id, moved };
  }

  // 覆盖写入能力资产正文
  writeContent(id, content) {
    const cap = this.db.get("SELECT * FROM capabilities WHERE id = ? AND scope = 'own'", [id]);
    if (!cap) throw new Error('能力资产不存在：' + id);
    const inst = this.db.get('SELECT * FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [cap.id]);
    if (!inst) throw new Error('能力资产没有物理目录');
    const entry = path.join(inst.path, entryFileFor(cap.kind));
    writeFileAtomic(entry, String(content == null ? '' : content));
    const size = fs.existsSync(entry) ? fs.statSync(entry).size : 0;
    this.db.run('UPDATE installations SET content = ?, size = ?, body_hash = ?, updated_at = ? WHERE id = ?',
      [String(content || '').slice(0, 60000), size, sha256(String(content || '')), now(), inst.id]);
    this.db.run('UPDATE capabilities SET updated_at = ? WHERE id = ?', [now(), cap.id]);
    this.bus && this.bus.info('assets', '已更新能力资产正文：' + cap.display_name, { assetId: cap.id });
    return { ok: true, id: cap.id, entry };
  }

  // ---------- 内部：双重登记 ----------
  registerOwn(input) {
    const { kind, name, dir, entry, content, summary, tags, version, author, metadata } = input;
    const ts = now();
    const slug = slugOf(name);
    // 自有资产使用独立身份命名空间，避免与宿主能力（kind:author/name）撞库
    let identityKey = 'asset:' + kind + ':' + slug;
    let n = 1;
    while (this.db.get("SELECT id FROM capabilities WHERE identity_key = ? AND scope = 'own'", [identityKey])) {
      identityKey = 'asset:' + kind + ':' + slug + '-' + (++n);
    }
    // id 必须由调用方生成后传入：目录名（assets/<kind>/<id>/）与数据库主键必须同一个值，
    // 否则磁盘目录无法反查数据库，「资产 = 目录 + 记录」的对应关系断裂。
    const id = input.id || newId('ast');
    const bodyHash = sha256(String(content || ''));
    const displayName = String(name || slug);
    const existing = this.db.get('SELECT id FROM capabilities WHERE id = ?', [id]);
    this.db.tx(() => {
      if (existing) {
        // overwrite：复用同一条资产记录，只刷新可变属性，身份键保持不变
        this.db.run(
          `UPDATE capabilities SET kind=?, canonical_name=?, display_name=?, summary=?, description=?, author=?, author_source=?,
            tags=?, version=?, fingerprint=?, body_hash=?, status='active', health='healthy', last_seen=?, updated_at=?, metadata=?
           WHERE id=?`,
          [kind, slug, displayName, oneLine(summary || '', 600), String(summary || ''), String(author || '能力核心'), author ? 'declared' : 'builtin',
            JSON.stringify(tags || []), String(version || '1.0.0'), bodyHash, bodyHash, ts, ts, JSON.stringify(metadata || {}), id]
        );
        this.db.run('UPDATE installations SET path=?, entry=?, version=?, fingerprint=?, body_hash=?, size=?, mtime=?, content=?, manifest=?, updated_at=? WHERE capability_id=?',
          [dir, entry, String(version || '1.0.0'), bodyHash, bodyHash, fs.existsSync(entry) ? fs.statSync(entry).size : 0, ts,
            String(content || '').slice(0, 60000), JSON.stringify(input.manifest || { kind, name: slug, displayName }), ts, id]);
        if (!this.db.get('SELECT id FROM installations WHERE capability_id = ?', [id])) {
          this.db.run(
            `INSERT INTO installations(id, capability_id, source_id, host, origin_kind, path, entry, glob, version, format, fingerprint, body_hash, size, mtime, content, manifest, visibility, created_at, updated_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
            [newId('ins'), id, null, 'asset-center', 'managed', dir, entry, '', String(version || '1.0.0'), kind + '-dir', bodyHash, bodyHash,
              fs.existsSync(entry) ? fs.statSync(entry).size : 0, ts, String(content || '').slice(0, 60000), JSON.stringify(input.manifest || { kind, name: slug, displayName }), 'visible', ts, ts]
          );
        }
      } else {
        this.db.run(
          `INSERT INTO capabilities(id, kind, canonical_name, display_name, summary, description, author, author_source, homepage, license,
            tags, version, protocols, hosts, identity, identity_key, fingerprint, body_hash, scope, status, health, first_seen, last_seen, updated_at, metadata)
           VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
          [id, kind, slug, displayName, oneLine(summary || '', 600), String(summary || ''), String(author || '能力核心'), author ? 'declared' : 'builtin',
            '', '', JSON.stringify(tags || []), String(version || '1.0.0'), '[]', JSON.stringify(['asset-center']),
            JSON.stringify({ asset: true }), identityKey, bodyHash, bodyHash, 'own', 'active', 'healthy', ts, ts, ts, JSON.stringify(metadata || {})]
        );
        this.db.run(
          `INSERT INTO installations(id, capability_id, source_id, host, origin_kind, path, entry, glob, version, format, fingerprint, body_hash, size, mtime, content, manifest, visibility, created_at, updated_at)
           VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
          [newId('ins'), id, null, 'asset-center', 'managed', dir, entry, '', String(version || '1.0.0'), kind + '-dir', bodyHash, bodyHash,
            fs.existsSync(entry) ? fs.statSync(entry).size : 0, ts, String(content || '').slice(0, 60000), JSON.stringify(input.manifest || { kind, name: slug, displayName }), 'visible', ts, ts]
        );
      }
    });
    try { this.svc && this.svc.registry && this.svc.registry.updateSearchDoc(id); } catch { /* 索引失败不影响登记 */ }
    return { id, identityKey, path: dir, entry };
  }

  // 保证自有资产来源在 sources 中可见（供「来源与扫描」页展示）
  ensureSource(kind, root) {
    const existing = this.db.get('SELECT id FROM sources WHERE root_path = ? AND kind = ?', [root, kind]);
    if (existing) return existing.id;
    const id = newId('src');
    const ts = now();
    this.db.run('INSERT INTO sources(id, kind, host, label, root_path, format, enabled, builtin, scan_interval, created_at, updated_at, config) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)',
      [id, kind, 'asset-center', '能力资产库-' + kind, root, kind + '-dir', 1, 1, 900, ts, ts, JSON.stringify({ depth: 1 })]);
    return id;
  }
}
