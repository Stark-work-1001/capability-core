import fs from 'node:fs';
import path from 'node:path';
import { newId, now, sha256, copyDir, readTextSafe, jsonParse, oneLine } from '../core/util.js';
import { skillAdapter } from '../adapters/skill.js';
import { pluginAdapter } from '../adapters/plugin.js';
import { mcpAdapter } from '../adapters/mcp.js';
import { ASSET_DIR } from '../core/paths.js';


// 能力入库：把外部文件/目录复制进能力资产库（assets/<kind>/<slug>/），
// 再登记为受管来源。原始位置保持不变（除非用户指定 move）。
export class ImportService {
  constructor(ctx, services) {
    this.ctx = ctx;
    this.svc = services;
    this.db = ctx.db;
  }

  detectKind(p) {
    const st = fs.statSync(p);
    if (st.isDirectory()) {
      if (skillAdapter.detect(p)) return 'skill';
      if (pluginAdapter.detect(p)) return 'plugin';
      if (mcpAdapter.detectPackage(p)) return 'mcp';
      return 'skill';
    }
    const base = path.basename(p).toLowerCase();
    if (/^(skill\.md|.*\.skill)$/.test(base)) return 'skill';
    if (/mcp.*\.json$/.test(base) || base === '.mcp.json') return 'mcp';
    if (base === 'plugin.json') return 'plugin';
    return 'skill';
  }

  importPath(sourcePath, options) {
    const opts = options || {};
    const src = path.resolve(sourcePath || '');
    if (!fs.existsSync(src)) throw new Error('路径不存在：' + src);
    const kind = opts.kind || this.detectKind(src);
    const slugBase = path.basename(src).replace(/[^a-zA-Z0-9._\u4e00-\u9fff-]/g, '_') || ('import-' + Date.now());
    let slug = slugBase;
    const assetDir = this.ctx.options.assetDir || ASSET_DIR;
    let dest = path.join(assetDir, kind, slug);
    let n = 1;
    while (fs.existsSync(dest)) { slug = slugBase + '-' + (++n); dest = path.join(assetDir, kind, slug); }
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    const st = fs.statSync(src);
    if (st.isDirectory()) copyDir(src, dest);
    else {
      fs.mkdirSync(dest, { recursive: true });
      fs.copyFileSync(src, path.join(dest, path.basename(src)));
    }
    const sourceId = this.ensureSource(kind, dest);
    this.ctx.bus.info('import', '能力已入库：' + slug + '（' + kind + '）', { path: dest, from: src });
    return { ok: true, kind, slug, dest, sourceId, name: slug };
  }

  ensureSource(kind, dir) {
    const host = 'asset-center';
    const assetDir = this.ctx.options.assetDir || ASSET_DIR;
    const root = path.join(assetDir, kind);
    const existing = this.db.get('SELECT * FROM sources WHERE root_path = ? AND kind = ?', [root, kind]);
    if (existing) return existing.id;
    const id = newId('src');
    const ts = now();
    this.db.run('INSERT INTO sources(id, kind, host, label, root_path, format, enabled, builtin, scan_interval, created_at, updated_at, config) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)',
      [id, kind, host, '能力资产库-' + kind, root, kind + '-dir', 1, 1, 900, ts, ts, JSON.stringify({ depth: 2 })]);
    return id;
  }

  importMany(paths, options) {
    const out = [];
    for (const p of paths) {
      try { out.push(this.importPath(p, options)); }
      catch (err) { out.push({ ok: false, path: p, error: String(err && err.message) }); }
    }
    return out;
  }

  // 预检：不落盘，只报告将导入什么、有什么问题
  preview(sourcePath) {
    const src = path.resolve(sourcePath || '');
    if (!fs.existsSync(src)) return { ok: false, error: '路径不存在' };
    const kind = this.detectKind(src);
    const st = fs.statSync(src);
    const issues = [];
    let manifest = null;
    if (kind === 'skill' && st.isDirectory()) {
      const parsed = skillAdapter.parse(src);
      if (parsed) manifest = parsed.manifest;
      if (!parsed) issues.push('目录中缺少 SKILL.md');
    } else if (kind === 'plugin' && st.isDirectory()) {
      const parsed = pluginAdapter.parse(src);
      if (parsed) manifest = parsed.manifest;
    } else if (kind === 'mcp' && st.isDirectory()) {
      const parsed = mcpAdapter.parsePackage(src);
      if (parsed) manifest = parsed.manifest;
    }
    return { ok: issues.length === 0, kind, path: src, name: manifest ? manifest.displayName : path.basename(src),
      manifest, issues, size: st.isDirectory() ? null : st.size };
  }
}
