import fs from 'node:fs';
import path from 'node:path';
import { HOST_HOME_DIRS, USER_HOME, ASSET_DIR } from '../core/paths.js';
import { now, newId } from '../core/util.js';

// 内置发现规则：把本机各智能体生态的能力目录登记为来源。
// 用户可在界面新增/停用来源；内置来源不可删除，只能停用。
export function hostHome(host) {
  const dirs = HOST_HOME_DIRS[host] || [];
  return dirs.find((d) => fs.existsSync(d)) || dirs[0] || '';
}

export function discoveryCandidates(assetDir) {
  const out = [];
  const ASSETS = assetDir || ASSET_DIR;
  const add = (kind, host, label, root, opts) => {
    if (!root || !fs.existsSync(root)) return;
    out.push({ kind, host, label, rootPath: root, builtin: true, ...(opts || {}) });
  };

  // ---- DSH（DeepSeek Harness）----
  const dsh = hostHome('dsh');
  if (dsh) {
    add('plugin', 'dsh', 'DSH 插件（profile node_modules）', path.join(dsh, 'profiles', 'desktop', 'node_modules'), { format: 'plugin-package', depth: 1 });
    add('plugin', 'dsh', 'DSH 本地插件', path.join(dsh, 'plugins'), { format: 'plugin-package', depth: 2 });
    add('mcp', 'dsh', 'DSH 配置', dsh, { format: 'mcp-config', depth: 0 });
  }
  // 通用能力仓库：用户自己维护的跨宿主资产目录。
  // 位置由 CC_EXTRA_LIBRARY 环境变量指定；未指定时回退到用户主目录下的默认位置。
  // 不硬编码盘符或用户名，便于在不同机器上直接使用。
  const extraLibrary = process.env.CC_EXTRA_LIBRARY
    ? path.resolve(process.env.CC_EXTRA_LIBRARY)
    : path.join(USER_HOME, '.capability-assets');
  if (fs.existsSync(extraLibrary)) {
    add('skill', 'dsh', '本机技能仓库 (通用资产目录 / .skill)', path.join(extraLibrary, '.skill'), { format: 'skill-dir', depth: 1 });
    add('mcp', 'dsh', '本机 MCP 工程 (通用资产目录 / .mcp)', path.join(extraLibrary, '.mcp'), { format: 'mcp-project', depth: 1 });
    add('plugin', 'dsh', '本机插件仓库 (通用资产目录 / .plugin)', path.join(extraLibrary, '.plugin'), { format: 'plugin-package', depth: 1 });
  }

  // ---- Codex ----
  const codex = hostHome('codex');
  if (codex) {
    add('skill', 'codex', 'Codex Skills', path.join(codex, 'skills'), { format: 'skill-dir', depth: 1 });
    add('plugin', 'codex', 'Codex 插件缓存', path.join(codex, 'plugins', 'cache'), { format: 'plugin-package', depth: 3 });
    add('plugin', 'codex', 'Codex 插件', path.join(codex, 'plugins'), { format: 'plugin-package', depth: 2 });
    add('mcp', 'codex', 'Codex 配置', codex, { format: 'mcp-config', depth: 0 });
  }

  // ---- Claude Code ----
  const claude = hostHome('claude');
  if (claude) {
    add('skill', 'claude', 'Claude Skills', path.join(claude, 'skills'), { format: 'skill-dir', depth: 1 });
    add('plugin', 'claude', 'Claude 插件', path.join(claude, 'plugins'), { format: 'plugin-package', depth: 2 });
    add('mcp', 'claude', 'Claude 配置', claude, { format: 'mcp-config', depth: 0 });
    add('prompt', 'claude', 'Claude 命令', path.join(claude, 'commands'), { format: 'prompt-dir', depth: 2 });
  }

  // ---- MiniMax Code ----
  const minimax = hostHome('minimax');
  if (minimax) {
    add('skill', 'minimax', 'MiniMax 内置技能', path.join(minimax, '.builtin-skills'), { format: 'skill-dir', depth: 1 });
    add('plugin', 'minimax', 'MiniMax 插件', path.join(minimax, 'plugins'), { format: 'plugin-package', depth: 2 });
    add('agent', 'minimax', 'MiniMax 智能体', path.join(minimax, 'agents'), { format: 'agent-dir', depth: 2 });
    add('mcp', 'minimax', 'MiniMax 配置', minimax, { format: 'mcp-config', depth: 0 });
  }

  // ---- Gemini / Antigravity ----
  const gemini = hostHome('gemini');
  if (gemini) {
    add('skill', 'gemini', 'Gemini 技能', path.join(gemini, 'skills'), { format: 'skill-dir', depth: 1 });
    add('mcp', 'gemini', 'Gemini 配置', path.join(gemini, 'config'), { format: 'mcp-config', depth: 0 });
  }
  const antigravity = path.join(USER_HOME, '.gemini', 'antigravity');
  if (fs.existsSync(antigravity)) {
    add('plugin', 'gemini', 'Antigravity 插件', antigravity, { format: 'plugin-package', depth: 2 });
  }

  // ---- WorkBuddy ----
  const wb = hostHome('workbuddy');
  if (wb) {
    add('skill', 'workbuddy', 'WorkBuddy 技能', path.join(wb, 'buddy-skill-store'), { format: 'skill-dir', depth: 2 });
    add('mcp', 'workbuddy', 'WorkBuddy 配置', wb, { format: 'mcp-config', depth: 0 });
  }

  // ---- 其他常见生态 ----
  const cursor = hostHome('cursor');
  if (cursor) {
    add('mcp', 'cursor', 'Cursor 配置', cursor, { format: 'mcp-config', depth: 1 });
    add('skill', 'cursor', 'Cursor 技能', path.join(cursor, 'skills'), { format: 'skill-dir', depth: 1 });
  }
  const qoder = hostHome('qoder');
  if (qoder) add('mcp', 'qoder', 'Qoder 配置', qoder, { format: 'mcp-config', depth: 1 });

  // ---- 中枢自身能力资产库 ----
  add('skill', 'asset-center', '能力资产库技能', path.join(ASSETS, 'skill'), { format: 'skill-dir', depth: 1 });
  add('mcp', 'asset-center', '能力资产库 MCP', path.join(ASSETS, 'mcp'), { format: 'mcp-project', depth: 1 });
  add('plugin', 'asset-center', '能力资产库插件', path.join(ASSETS, 'plugin'), { format: 'plugin-package', depth: 1 });
  add('prompt', 'asset-center', '能力资产库提示词', path.join(ASSETS, 'prompt'), { format: 'prompt-dir', depth: 1 });

  return dedupeByRoot(out);
}

function dedupeByRoot(list) {
  const seen = new Set();
  const out = [];
  for (const item of list) {
    const key = item.kind + '::' + path.resolve(item.rootPath).toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(item);
  }
  return out;
}

export function seedSources(db, bus, assetDir) {
  const existing = db.all('SELECT * FROM sources');
  const byKey = new Map(existing.map((s) => [s.kind + '::' + path.resolve(s.root_path).toLowerCase(), s]));
  const candidates = discoveryCandidates(assetDir);
  const ts = now();
  let added = 0;
  for (const c of candidates) {
    const key = c.kind + '::' + path.resolve(c.rootPath).toLowerCase();
    if (byKey.has(key)) continue;
    db.run(
      `INSERT INTO sources(id, kind, host, label, root_path, format, enabled, builtin, scan_interval, created_at, updated_at, config)
       VALUES (?,?,?,?,?,?,?,?,?,?,?,?)`,
      [newId('src'), c.kind, c.host, c.label, c.rootPath, c.format || 'auto', 1, c.builtin ? 1 : 0, 900, ts, ts, JSON.stringify({ depth: c.depth ?? 2 })]
    );
    added++;
  }
  if (added && bus) bus.info('sources', '自动发现 ' + added + ' 个能力来源', { count: added });
  return { added, total: candidates.length };
}

// 标记已失效来源（路径不存在）
export function markMissingSources(db) {
  const rows = db.all('SELECT id, root_path FROM sources WHERE enabled = 1');
  const missing = [];
  for (const r of rows) {
    if (!fs.existsSync(r.root_path)) {
      db.run("UPDATE sources SET last_scan_status = 'error', last_scan_message = ? WHERE id = ?", ['路径不存在：' + r.root_path, r.id]);
      missing.push(r.id);
    }
  }
  return missing;
}
