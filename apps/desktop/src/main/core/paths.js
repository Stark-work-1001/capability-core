import os from 'node:os';
import path from 'node:path';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const here = path.dirname(fileURLToPath(import.meta.url));
// 本文件是 ESM，没有全局 require；用 createRequire 才能「按需」探测 electron，
// 从而既不破坏纯 node 的 CLI，也能在主进程内拿到 app.isPackaged。
const nodeRequire = createRequire(import.meta.url);
export const APP_ROOT = path.resolve(here, '..', '..', '..', '..', '..');
export const DESKTOP_ROOT = path.resolve(here, '..', '..', '..');
// 打包态判定：必须不依赖外部环境变量。
// 早期实现只认 CC_PACKAGED，而该变量仅由启动器写入 —— 用户直接双击 exe 时它不存在，
// 于是误判为开发态，数据被写进应用包内部的 resources/app/data（备份/升级都会出事）。
// 这里改为三条互相独立的物理证据，任一命中即为打包态：
//   1) 环境变量 CC_PACKAGED=1（启动器写入，最快路径）；
//   2) Electron 原生 app.isPackaged（打包后恒为 true；用延迟 require 以免纯 node 的 CLI 挂掉）；
//   3) 存在 resources/app/package.json —— 这是 Electron 打包布局的铁证。
function detectPackaged() {
  if (process.env.CC_PACKAGED === '1') return true;

  // res/ 结构：<...>/resources/app/package.json（本项目应用本体就放在这里）
  try {
    const resourcesDir = process.resourcesPath
      || path.join(path.dirname(process.execPath), 'resources');
    if (resourcesDir && fs.existsSync(path.join(resourcesDir, 'app', 'package.json'))) return true;
  } catch { /* 忽略：继续下一条证据 */ }

  // Electron 原生标记。放在最后：纯 node 环境（CLI）没有 electron 模块，
  // 且不能静态 import —— 否则 cli.js 直接起不来。
  try {
    // eslint-disable-next-line global-require
    const { app } = nodeRequire('electron');
    if (app && typeof app.isPackaged === 'boolean' && app.isPackaged) return true;
  } catch { /* 纯 node 环境：无 electron 模块，属正常情况 */ }

  return false;
}

export const IS_PACKAGED = detectPackaged();

// 数据根：默认在程序目录旁；可通过 CC_HOME 覆盖（测试/多实例隔离）。
export function resolveHome() {
  if (process.env.CC_HOME) return path.resolve(process.env.CC_HOME);
  // 打包后 exe 位于 release/runtime/，数据目录放在 release/capability-core-data（与程序同级、便于备份）
  if (IS_PACKAGED) {
    const exeDir = path.dirname(process.execPath);
    const rootDir = path.basename(exeDir).toLowerCase() === 'runtime' ? path.dirname(exeDir) : exeDir;
    return path.join(rootDir, 'capability-core-data');
  }
  return path.join(APP_ROOT, 'data');
}

export const HOME = resolveHome();
export const DB_PATH = process.env.CC_DB || path.join(HOME, 'capability-core.db');
export const RUN_DIR = path.join(HOME, 'run');
export const LOG_DIR = path.join(HOME, 'logs');
export const SNAPSHOT_DIR = path.join(HOME, 'snapshots');
export const BACKUP_DIR = path.join(HOME, 'backups');
export const SEARCH_INDEX_DIR = path.join(HOME, 'search');
// 能力资产库（自有资产）与数据目录同级：开发态是项目根/assets，打包态是 release/assets
export const ASSET_DIR = process.env.CC_ASSET_DIR || path.join(path.dirname(HOME), 'assets');
export const TRASH_DIR = path.join(ASSET_DIR, '.trash');
export const EXPORT_DIR = path.join(HOME, 'exports');
export const USER_HOME = os.homedir();

// 能力资产的四种物化形态（与 capabilities.kind 对齐）
export const ASSET_KINDS = ['skill', 'mcp', 'plugin', 'prompt'];

export function ensureDirs() {
  const dirs = [HOME, RUN_DIR, LOG_DIR, SNAPSHOT_DIR, BACKUP_DIR, SEARCH_INDEX_DIR, ASSET_DIR, TRASH_DIR, EXPORT_DIR];
  for (const kind of ASSET_KINDS) dirs.push(path.join(ASSET_DIR, kind));
  for (const dir of dirs) {
    try { fs.mkdirSync(dir, { recursive: true }); } catch { /* 目录不可写不应阻止启动 */ }
  }
}

export const HOST_HOME_DIRS = {
  dsh: [path.join(USER_HOME, '.dsh'), path.join(USER_HOME, 'AppData', 'Roaming', '@deepseek-ai', 'dsh-desktop')],
  codex: [path.join(USER_HOME, '.codex')],
  claude: [path.join(USER_HOME, '.claude'), path.join(USER_HOME, '.config', 'claude')],
  minimax: [path.join(USER_HOME, '.minimax')],
  gemini: [path.join(USER_HOME, '.gemini')],
  workbuddy: [path.join(USER_HOME, '.workbuddy')],
  cursor: [path.join(USER_HOME, '.cursor'), path.join(USER_HOME, 'AppData', 'Roaming', 'Cursor')],
  windsurf: [path.join(USER_HOME, '.codeium', 'windsurf')],
  qoder: [path.join(USER_HOME, '.qoder')],
  generic: []
};

export function safeJoin(root, rel) {
  const target = path.resolve(root, rel || '.');
  const normRoot = path.resolve(root);
  if (target !== normRoot && !target.startsWith(normRoot + path.sep)) {
    throw new Error('路径越界: ' + rel);
  }
  return target;
}

// 把任意安装路径归一成「资产实体目录」assets/<kind>/<id>/（基线 §4 的物理布局）。
// 必要性：同一份资产，资产库用目录路径登记，扫描器对 MCP 用 "<file>#<name>" 登记，
// 两者字符串不等但指向同一实体，必须按实体目录而不是按字符串比较。
export function assetEntityDir(target, assetDir) {
  if (!target) return '';
  try {
    const base = path.resolve(assetDir || ASSET_DIR);
    const abs = path.resolve(String(target).split('#')[0]);
    if (abs !== base && !abs.startsWith(base + path.sep)) return '';
    const parts = path.relative(base, abs).split(path.sep).filter(Boolean);
    if (parts.length < 2) return '';
    return path.join(base, parts[0], parts[1]);
  } catch {
    return '';
  }
}

// 判断某个安装路径是否落在能力资产库内（用于回填 capabilities.scope）
export function isInsideAssetDir(target, assetDir) {
  if (!target) return false;
  try {
    const base = path.resolve(assetDir || ASSET_DIR);
    const abs = path.resolve(String(target).split('#')[0]);
    return abs === base || abs.startsWith(base + path.sep);
  } catch {
    return false;
  }
}
