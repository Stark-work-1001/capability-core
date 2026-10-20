import fs from 'node:fs';
import path from 'node:path';
import { skillAdapter, skillHash } from '../adapters/skill.js';
import { mcpAdapter } from '../adapters/mcp.js';
import { pluginAdapter } from '../adapters/plugin.js';
import { normalizeManifest } from '../core/manifest.js';
import { now, sha256, readTextSafe, oneLine, truncate, jsonParse, copyDir, writeFileAtomic, sleep } from '../core/util.js';
import { newId } from '../core/util.js';
import { fingerprintOf, identityOf } from '../core/manifest.js';
import { segmentForIndex } from '../core/text-index.js';

const CONFIG_FILES = ['mcp.json', '.mcp.json', 'mcp_config.json', 'claude_desktop_config.json', 'mcp-servers.json', 'config.toml', 'config.yaml', 'config.yml', 'mcp.yaml'];

export class Scanner {
  constructor(ctx) {
    this.ctx = ctx;              // { db, bus, logger, registry }
    this.running = false;
    this.current = null;
    this.queue = [];
  }

  listSources() {
    return this.ctx.db.all('SELECT * FROM sources ORDER BY builtin DESC, host, kind');
  }

  async scanAll(options) {
    if (this.running) return { started: false, reason: '扫描进行中', current: this.current };
    this.running = true;
    const opts = options || {};
    const startedAt = now();
    const sources = this.listSources().filter((s) => opts.sourceId ? s.id === opts.sourceId : s.enabled);
    const result = { started: true, startedAt, sources: [], found: 0, added: 0, updated: 0, removed: 0, faults: 0, errors: [] };
    this.ctx.bus.info('scan', '开始扫描 ' + sources.length + ' 个来源');
    const seenPaths = new Set();
    for (const src of sources) {
      this.current = src.label;
      try {
        const res = await this.scanSource(src, seenPaths);
        result.sources.push(res);
        result.found += res.found;
        result.added += res.added;
        result.updated += res.updated;
        result.removed += res.removed;
        result.faults += res.faults;
      } catch (err) {
        this.ctx.logger.error('扫描来源失败 ' + src.label, { error: String(err && err.message) });
        result.errors.push({ source: src.label, error: String(err && err.message) });
        this.ctx.db.run("UPDATE sources SET last_scan_status='error', last_scan_message=?, updated_at=? WHERE id=?", [String(err && err.message), now(), src.id]);
        this.ctx.bus.error('scan', '来源扫描失败：' + src.label, { error: String(err && err.message) });
      }
      await sleep(0);
    }
    // 清理：本次未出现的「发现型」安装（受管与生成的不清理）
    const removed = this.pruneMissing(seenPaths);
    result.removed += removed;
    this.current = null;
    this.running = false;
    result.endedAt = now();
    result.durationMs = result.endedAt - startedAt;
    this.ctx.bus.info('scan', '扫描完成：发现 ' + result.found + ' 项，新增 ' + result.added + '，更新 ' + result.updated + '，清理 ' + result.removed + '，故障 ' + result.faults, { summary: result });
    const registry = this.ctx.services ? this.ctx.services.registry : this.ctx.registry;
    if (registry) registry.reindexSearch();
    return result;
  }

  async scanSource(src, seenPaths) {
    const t0 = now();
    const depth = Number(jsonParse(src.config, {}).depth ?? 2);
    const found = [];
    const root = src.root_path;
    if (!fs.existsSync(root)) throw new Error('路径不存在：' + root);
    const stat = fs.statSync(root);

    if (src.kind === 'skill') {
      for (const dir of collectSkillDirs(root, depth)) {
        const item = safeParse(() => skillAdapter.parse(dir), src, dir);
        if (item) found.push({ ...item, dir, path: dir });
      }
    } else if (src.kind === 'mcp') {
      // 配置形态
      const files = depth === 0 ? CONFIG_FILES.map((f) => path.join(root, f)).filter((f) => fs.existsSync(f))
        : walkFiles(root, depth, (f) => CONFIG_FILES.includes(path.basename(f).toLowerCase()));
      for (const file of files) {
        const items = mcpAdapter.parseConfigFile(file, src.host) || [];
        for (const item of items) found.push({ ...item, path: file + '#' + item.manifest.name });
      }
      // 工程形态
      const projectDirs = collectProjectDirs(root, depth);
      for (const dir of projectDirs) {
        const item = safeParse(() => mcpAdapter.parsePackage(dir, src.host), src, dir);
        if (item) found.push({ ...item, dir, path: dir });
      }
    } else if (src.kind === 'plugin') {
      const dirs = collectPluginDirs(root, depth);
      for (const dir of dirs) {
        const item = safeParse(() => pluginAdapter.parse(dir, src.host), src, dir);
        if (item) found.push({ ...item, dir, path: dir });
      }
    } else if (src.kind === 'prompt') {
      for (const file of walkFiles(root, depth, (f) => /\.(md|txt)$/i.test(f))) {
        const text = readTextSafe(file) || '';
        if (!text.trim()) continue;
        const name = path.basename(file).replace(/\.(md|txt)$/i, '');
        const manifest = normalizeManifest({
          kind: 'prompt', name, displayName: name,
          summary: oneLine(text.replace(/^#+\s*/m, ''), 240),
          description: truncate(text, 12000), hosts: [src.host], entry: path.basename(file),
          protocols: ['prompt'], tags: ['prompt']
        });
        found.push({ manifest, body: text, entry: file, path: file });
      }
    } else if (src.kind === 'agent') {
      for (const file of walkFiles(root, depth, (f) => /\.(md|json|yaml|yml)$/i.test(f))) {
        const text = readTextSafe(file) || '';
        const name = path.basename(file).replace(/\.\w+$/, '');
        const manifest = normalizeManifest({
          kind: 'agent', name, displayName: name,
          summary: oneLine(text.replace(/^#+\s*/m, ''), 240), description: truncate(text, 12000),
          hosts: [src.host], entry: path.basename(file), protocols: ['agent-dir'], tags: ['agent']
        });
        found.push({ manifest, body: text, entry: file, path: file });
      }
    }

    // 写入数据库
    let added = 0; let updated = 0; let faults = 0;
    const store = this.ctx.services ? this.ctx.services.registry : this.ctx.registry;
    for (const item of found) {
      if (!item.manifest || !item.manifest.name) continue;
      seenPaths.add(path.resolve(item.path).toLowerCase());
      const r = store.upsertCapability(item, src);
      if (r.created) added++; else updated++;
      if (r.fault) faults++;
    }
    const status = faults > 0 ? 'warn' : 'ok';
    this.ctx.db.run(
      "UPDATE sources SET last_scan_at=?, last_scan_status=?, last_scan_message=?, last_scan_count=?, last_scan_ms=?, updated_at=? WHERE id=?",
      [now(), status, faults ? faults + ' 项能力存在故障' : '正常', found.length, now() - t0, now(), src.id]
    );
    return { id: src.id, label: src.label, host: src.host, kind: src.kind, found: found.length, added, updated, removed: 0, faults, ms: now() - t0, status };
  }

  pruneMissing(seenPaths) {
    const db = this.ctx.db;
    const rows = db.all("SELECT i.id, i.path, i.origin_kind, c.id AS cap_id FROM installations i JOIN capabilities c ON c.id = i.capability_id WHERE i.origin_kind = 'discovered'");
    let removed = 0;
    for (const r of rows) {
      const raw = String(r.path).split('#')[0];
      if (seenPaths.has(path.resolve(raw).toLowerCase())) continue;
      if (fs.existsSync(raw)) {
        // 文件仍在但未被扫描到（例如来源停用）：保留，标记为 shadowed 由来源状态解释
        continue;
      }
      db.run('DELETE FROM installations WHERE id = ?', [r.id]);
      const left = db.get('SELECT COUNT(*) AS c FROM installations WHERE capability_id = ?', [r.cap_id]).c;
      if (!left) {
        db.run('DELETE FROM capabilities WHERE id = ?', [r.cap_id]);
        removed++;
      }
    }
    if (removed) this.ctx.bus.warn('scan', '清理 ' + removed + ' 项已消失的能力');
    return removed;
  }
}

function safeParse(fn, src, where) {
  try { return fn(); } catch (err) {
    return { manifest: normalizeManifest({
      kind: src.kind, name: path.basename(where), displayName: path.basename(where),
      summary: '解析失败：' + String(err && err.message).slice(0, 200), hosts: [src.host],
      tags: ['parse-error'], metadata: { parseError: String(err && err.message) }
    }), body: '', entry: where, parseError: String(err && err.message) };
  }
}

export function collectSkillDirs(root, depth) {
  const out = [];
  if (skillAdapter.detect(root)) out.push(root);
  const maxDepth = Math.max(1, depth || 1);
  const stack = [{ d: root, depth: 0 }];
  while (stack.length) {
    const cur = stack.pop();
    let entries = [];
    try { entries = fs.readdirSync(cur.d, { withFileTypes: true }); } catch { continue; }
    for (const ent of entries) {
      if (!ent.isDirectory()) continue;
      if (ent.name === '.git' || ent.name === 'node_modules' || ent.name.startsWith('__')) continue;
      const full = path.join(cur.d, ent.name);
      if (skillAdapter.detect(full)) out.push(full);
      if (cur.depth + 1 < maxDepth) stack.push({ d: full, depth: cur.depth + 1 });
    }
  }
  return [...new Set(out)];
}

export function collectProjectDirs(root, depth) {
  const out = [];
  const maxDepth = Math.max(1, depth || 1);
  const stack = [{ d: root, depth: 0 }];
  while (stack.length) {
    const cur = stack.pop();
    let entries = [];
    try { entries = fs.readdirSync(cur.d, { withFileTypes: true }); } catch { continue; }
    for (const ent of entries) {
      if (!ent.isDirectory()) continue;
      if (['node_modules', '.git', '.venv', '__pycache__'].includes(ent.name)) continue;
      const full = path.join(cur.d, ent.name);
      if (cur.depth > 0 || root !== full) {
        // 目录本身可能是工程根
      }
      const marks = ['pyproject.toml', 'package.json', 'setup.py', 'go.mod'];
      if (marks.some((m) => fs.existsSync(path.join(full, m)))) out.push(full);
      if (cur.depth + 1 < maxDepth) stack.push({ d: full, depth: cur.depth + 1 });
    }
  }
  return [...new Set(out)];
}

export function collectPluginDirs(root, depth) {
  const out = [];
  const maxDepth = Math.max(1, depth || 1);
  const stack = [{ d: root, depth: 0 }];
  while (stack.length) {
    const cur = stack.pop();
    let entries = [];
    try { entries = fs.readdirSync(cur.d, { withFileTypes: true }); } catch { continue; }
    for (const ent of entries) {
      if (!ent.isDirectory()) continue;
      if (['.git', '__pycache__'].includes(ent.name)) continue;
      const full = path.join(cur.d, ent.name);
      if (pluginAdapter.detect(full)) out.push(full);
      if (cur.depth + 1 < maxDepth) stack.push({ d: full, depth: cur.depth + 1 });
    }
  }
  return [...new Set(out)];
}

export function walkFiles(root, depth, filter, limit) {
  const out = [];
  const maxDepth = Math.max(0, depth || 0);
  const stack = [{ d: root, depth: 0 }];
  while (stack.length && out.length < (limit || 500)) {
    const cur = stack.pop();
    let entries = [];
    try { entries = fs.readdirSync(cur.d, { withFileTypes: true }); } catch { continue; }
    for (const ent of entries) {
      const full = path.join(cur.d, ent.name);
      if (ent.isDirectory()) {
        if (['node_modules', '.git', '__pycache__', '.venv'].includes(ent.name)) continue;
        if (cur.depth < maxDepth) stack.push({ d: full, depth: cur.depth + 1 });
      } else if (ent.isFile() && filter(full)) {
        out.push(full);
      }
    }
  }
  return out;
}
