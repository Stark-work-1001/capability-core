import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// 开发态启动器：剔除 ELECTRON_RUN_AS_NODE（本机环境变量会强制 electron 以 node 模式运行，
// 表现为「启动后没有窗口、只当 node 用」），再启动桌面应用。
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const electron = path.join(root, 'node_modules', 'electron', 'dist', 'electron.exe');
const env = { ...process.env };
delete env.ELECTRON_RUN_AS_NODE;
env.CC_PACKAGED = env.CC_PACKAGED || '0';
const child = spawn(electron, [path.join(root, 'apps', 'desktop'), ...process.argv.slice(2)], { stdio: 'inherit', env, windowsHide: false });
child.on('exit', (code) => process.exit(code ?? 0));
