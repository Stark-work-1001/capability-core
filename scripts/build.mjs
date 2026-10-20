import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// 打包：Vite 构建渲染层 → 组装 Electron 绿色运行时 → 产出 release/runtime/CapabilityCore.exe
// 产物形态（基线 §4）：
//   release/runtime/CapabilityCore.exe     双击即启动
//   release/runtime/resources/app/         应用本体（Electron 默认从 resources/app 读取 package.json）
//   release/capability-core-data/          打包态数据目录
//   release/assets/<kind>/                 打包态能力资产库
//   release/启动-能力核心.cmd
//   release/VERSION.json
// 不引入 electron-builder / electron-packager：直接复用本地 node_modules/electron 的 dist。
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const out = path.join(root, 'release');
const runtimeDir = path.join(out, 'runtime');
const resourcesDir = path.join(runtimeDir, 'resources');
const appDir = path.join(resourcesDir, 'app');
const EXE_NAME = 'CapabilityCore.exe';        // 用户双击的入口（原生启动器）
const GUI_EXE_NAME = 'CapabilityCore-GUI.exe'; // Electron 本体（由启动器拉起）
const ASSET_KINDS = ['skill', 'mcp', 'plugin', 'prompt'];

function run(cmd, args, cwd) {
  // 不使用 shell：避免 Windows 上含空格的程序路径（C:\Program Files\...）被拆词
  const r = spawnSync(cmd, args, { cwd: cwd || root, stdio: 'inherit', shell: false });
  if (r.error) throw r.error;
  if (r.status !== 0) throw new Error(cmd + ' 失败，退出码 ' + r.status);
}

function step(n, total, label) {
  console.log('[' + n + '/' + total + '] ' + label);
}

function must(cond, message) {
  if (!cond) throw new Error('打包校验失败：' + message);
}

// Electron 本体改名后不再自带 launch 能力解析，需要确保 resources 布局不变（已满足）。

// .NET Framework 的 C# 编译器位置（Windows 自带，无需任何 npm 依赖）
const CSC_CANDIDATES = [
  'C:\\Windows\\Microsoft.NET\\Framework64\\v4.0.30319\\csc.exe',
  'C:\\Windows\\Microsoft.NET\\Framework\\v4.0.30319\\csc.exe'
];

// 编译原生启动器 → release/runtime/CapabilityCore.exe
// 为什么不用 JS：ELECTRON_RUN_AS_NODE 的判定发生在 Electron 原生层，
// 命中时主进程 main.js 根本不执行（实测零日志、零目录），JS 侧无补救机会。
function buildLauncher() {
  const src = path.join(root, 'scripts', 'launcher.cs');
  must(fs.existsSync(src), '缺少启动器源码：' + src);
  const csc = CSC_CANDIDATES.find((p) => fs.existsSync(p));
  must(csc, '未找到 C# 编译器（csc.exe）。原生启动器是打包必需项：它负责清除 ELECTRON_RUN_AS_NODE 并设置 CC_PACKAGED。');
  const target = path.join(runtimeDir, EXE_NAME);
  const r = spawnSync(csc, [
    '/nologo', '/target:winexe', '/optimize+', '/utf8output',
    '/out:' + target,
    '/reference:System.Windows.Forms.dll',
    src
  ], { cwd: root, encoding: 'utf8' });
  if (r.error) throw r.error;
  if (r.status !== 0) throw new Error('启动器编译失败（退出码 ' + r.status + '）：' + (r.stdout || '') + (r.stderr || ''));
  must(fs.existsSync(target), '启动器未生成：' + target);
  const nowDate = new Date();
  fs.utimesSync(target, nowDate, nowDate);
}

const TOTAL = 7;

// ── 1. 渲染层 ────────────────────────────────────────────────
step(1, TOTAL, '构建渲染层…');
run(process.execPath, [path.join(root, 'node_modules', 'vite', 'bin', 'vite.js'), 'build', '--config', path.join(root, 'apps', 'desktop', 'vite.config.mjs')]);
const rendererIndex = path.join(root, 'apps', 'desktop', 'dist-renderer', 'index.html');
must(fs.existsSync(rendererIndex), '渲染层产物缺失：' + rendererIndex);

// ── 2. 清空 release（幂等：重复执行结果一致）─────────────────
step(2, TOTAL, '准备 release 目录（幂等清理）…');
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });

// ── 3. 复制 Electron 运行时，并把 electron.exe 改名为 GUI 本体 ──
step(3, TOTAL, '复制 Electron 运行时…');
const electronDist = path.join(root, 'node_modules', 'electron', 'dist');
must(fs.existsSync(path.join(electronDist, 'electron.exe')), '未找到 ' + path.join(electronDist, 'electron.exe'));
fs.cpSync(electronDist, runtimeDir, { recursive: true });
// Electron 本体改名后不再是入口：CapabilityCore.exe 由下面的原生启动器承担，
// 因为有两件事必须在任何 JS 执行之前完成（见 scripts/launcher.cs 顶部说明）。
fs.renameSync(path.join(runtimeDir, 'electron.exe'), path.join(runtimeDir, GUI_EXE_NAME));
{
  const exePath = path.join(runtimeDir, GUI_EXE_NAME);
  const nowDate = new Date();
  fs.utimesSync(exePath, nowDate, nowDate);
}

// Electron 自带 default_app.asar：不清掉会打开默认页而不是我们的应用
for (const stale of ['default_app.asar', 'default_app.asar.unpacked']) {
  const p = path.join(resourcesDir, stale);
  if (fs.existsSync(p)) fs.rmSync(p, { recursive: true, force: true });
}

// ── 4. 编译原生启动器为 CapabilityCore.exe ──────────────────
step(4, TOTAL, '编译原生启动器 CapabilityCore.exe…');
buildLauncher();

// ── 5. 组装应用本体到 resources/app ──────────────────────────
step(5, TOTAL, '组装应用本体到 resources/app…');
fs.mkdirSync(appDir, { recursive: true });
// 只带运行期需要的文件：主进程源码、渲染层产物、桌面壳 package.json
fs.cpSync(path.join(root, 'apps', 'desktop', 'src'), path.join(appDir, 'apps', 'desktop', 'src'), { recursive: true });
fs.cpSync(path.join(root, 'apps', 'desktop', 'dist-renderer'), path.join(appDir, 'apps', 'desktop', 'dist-renderer'), { recursive: true });
// 图标：主进程按 apps/desktop/build/icon.png 相对路径查找，必须一并带上
const iconSrc = path.join(root, 'apps', 'desktop', 'build', 'icon.png');
if (fs.existsSync(iconSrc)) {
  fs.mkdirSync(path.join(appDir, 'apps', 'desktop', 'build'), { recursive: true });
  fs.copyFileSync(iconSrc, path.join(appDir, 'apps', 'desktop', 'build', 'icon.png'));
}
fs.writeFileSync(
  path.join(appDir, 'package.json'),
  JSON.stringify({ name: 'capability-core-desktop', version: JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8')).version, private: true, type: 'module', main: 'apps/desktop/src/main/main.js' }, null, 2) + '\n'
);

// ── 6. 数据目录与资产目录 ────────────────────────────────────
step(6, TOTAL, '创建数据目录与能力资产库目录…');
const dataDir = path.join(out, 'capability-core-data');
fs.mkdirSync(dataDir, { recursive: true });
fs.writeFileSync(path.join(dataDir, '.gitkeep'), '');
for (const kind of ASSET_KINDS) fs.mkdirSync(path.join(out, 'assets', kind), { recursive: true });

// ── 7. 启动器与版本信息 ──────────────────────────────────────
step(7, TOTAL, '写启动器与版本信息…');
const CRLF = String.fromCharCode(13, 10);
const BS = String.fromCharCode(92); // 反斜杠：避免在 JS 字符串里写行尾 \ 造成转义歧义
// .cmd 只是双击入口的备选；环境清理与打包态标记已由原生启动器接管，
// 这里不重复设置，避免「.cmd 和 exe 行为不一致」再次出现。
const launcher = [
  '@echo off',
  'setlocal',
  'cd /d "%~dp0"',
  'start "" "%~dp0runtime' + BS + EXE_NAME + '"',
  'endlocal',
  ''
].join(CRLF);
fs.writeFileSync(path.join(out, '启动-能力核心.cmd'), launcher, 'utf8');

const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
const electronVer = JSON.parse(fs.readFileSync(path.join(root, 'node_modules', 'electron', 'package.json'), 'utf8')).version;
fs.writeFileSync(path.join(out, 'VERSION.json'), JSON.stringify({
  name: pkg.name,
  productName: 'Capability Core',
  productNameZh: '能力核心',
  version: pkg.version,
  builtAt: new Date().toISOString(),
  electron: electronVer,
  entry: 'runtime/' + EXE_NAME
}, null, 2) + '\n');

// ── 7. 产物自检：缺任何一项即失败，避免「看着成功其实打不开」───
step(7, TOTAL, '校验产物完整性…');
must(fs.existsSync(path.join(runtimeDir, EXE_NAME)), '缺少 ' + EXE_NAME);
must(fs.existsSync(path.join(runtimeDir, GUI_EXE_NAME)), '缺少 Electron 本体 ' + GUI_EXE_NAME);
// 启动器必须比 Electron 本体小得多，否则说明 exe 名字改错、双击会绕过环境清理直接进 Electron
{
  const stubSize = fs.statSync(path.join(runtimeDir, EXE_NAME)).size;
  const guiSize = fs.statSync(path.join(runtimeDir, GUI_EXE_NAME)).size;
  must(stubSize < guiSize / 10, EXE_NAME + ' 应为原生启动器（' + stubSize + ' B），实际大小接近 Electron 本体（' + guiSize + ' B），疑似改名错误');
}
must(fs.existsSync(path.join(appDir, 'package.json')), '缺少 resources/app/package.json');
must(fs.existsSync(path.join(appDir, 'apps', 'desktop', 'dist-renderer', 'index.html')), '缺少渲染层 index.html');
must(!fs.existsSync(path.join(resourcesDir, 'default_app.asar')), 'resources/default_app.asar 未清除');
const appMain = JSON.parse(fs.readFileSync(path.join(appDir, 'package.json'), 'utf8')).main;
must(fs.existsSync(path.join(appDir, appMain)), 'resources/app 内主进程入口不存在：' + appMain);

console.log('');
console.log('完成：' + out);
console.log('可执行文件：' + path.join(runtimeDir, EXE_NAME));
console.log('双击 release/runtime/' + EXE_NAME + ' 即可运行（启动器负责清除 ELECTRON_RUN_AS_NODE 并设置打包态）；');
console.log('数据保存于 release/capability-core-data，能力资产库为 release/assets。');
