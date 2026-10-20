import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// 开发模式：Vite dev server + Electron（渲染进程连 5173，主进程走 IPC）。
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const env = { ...process.env };
delete env.ELECTRON_RUN_AS_NODE;
env.CC_DEV = '1';

const vite = spawn(process.execPath, [path.join(root, 'node_modules', 'vite', 'bin', 'vite.js'), '--config', path.join(root, 'apps', 'desktop', 'vite.config.mjs')], { stdio: 'inherit', env });
await waitFor('http://127.0.0.1:5173', 30000);

const electronBin = path.join(root, 'node_modules', 'electron', 'dist', 'electron.exe');
const app = spawn(electronBin, [path.join(root, 'apps', 'desktop')], { stdio: 'inherit', env });
app.on('exit', (code) => { try { vite.kill(); } catch {} process.exit(code ?? 0); });
process.on('SIGINT', () => { try { app.kill(); vite.kill(); } catch {} process.exit(0); });

async function waitFor(url, timeoutMs) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const r = await fetch(url);
      if (r.ok || r.status === 200) return;
    } catch { /* retry */ }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error('Vite 启动超时');
}
