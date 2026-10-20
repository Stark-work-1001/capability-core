import { app, BrowserWindow, ipcMain, dialog, shell, Menu } from 'electron';
import path from 'node:path';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import { createContext, closeContext } from './services/context.js';
import { createHttpServer } from './server/http.js';
import { seedSources } from './services/discovery.js';
import { DB_PATH, HOME, ASSET_DIR, ensureDirs } from './core/paths.js';
import { now, jsonParse } from './core/util.js';
import { PRODUCT_TITLE, APP_ID } from './core/identity.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const DEV = process.env.CC_DEV === '1';
const PORT = Number(process.env.CC_PORT || 18297);
const EVENT_CHANNEL = 'capability-core:event';
let mainWindow = null;
let ctx = null;
let server = null;

app.disableHardwareAcceleration();
app.setAppUserModelId(APP_ID);

const gotLock = app.requestSingleInstanceLock();
if (!gotLock) {
  app.quit();
} else {
  app.on('second-instance', () => { if (mainWindow) { mainWindow.show(); mainWindow.focus(); } });
}

async function boot() {
  ensureDirs();
  ctx = createContext({ dbPath: DB_PATH });
  // 能力资产库：种子 + 物理目录 → 数据库补登记（目录是事实来源）
  try { ctx.services.assets.ensureSeed(); ctx.services.assets.syncFromDisk(); } catch (err) { ctx.logger.warn('能力资产登记失败', { error: String(err && err.message) }); }
  ctx.logger.info('应用启动', { home: HOME, db: DB_PATH, assets: ASSET_DIR, dev: DEV });
  seedSources(ctx.db, ctx.bus, ctx.options.assetDir);

  // HTTP + MCP 服务
  try {
    server = createHttpServer(ctx, ctx.services, {});
    const addr = await server.listen(PORT, '127.0.0.1');
    ctx.logger.info('中枢服务已启动', { port: addr.port });
    ctx.bus.info('system', '中枢 HTTP/MCP 服务已启动：http://127.0.0.1:' + addr.port + '/mcp');
  } catch (err) {
    ctx.logger.error('中枢服务启动失败', { error: String(err && err.message) });
    ctx.bus.error('system', '中枢服务启动失败：' + String(err && err.message));
  }

  ctx.bus.on((evt) => { if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send(EVENT_CHANNEL, evt); });

  scheduleBackground();

  ipcMain.handle('scan.busy', async () => ({ busy: scanning }));

  try { ctx.services.updates.importCatalog(); } catch (err) { ctx.logger.warn('目录导入失败', { error: String(err && err.message) }); }

  try { await ctx.services.agents.scan(); } catch (err) { ctx.logger.warn('智能体识别失败', { error: String(err && err.message) }); }

  const flags = settingsAll();
  if (flags.scanOnStart !== false) {
    setTimeout(() => { runInitialScan(); }, 1200);
  }
}

let scanning = false;
async function runInitialScan() {
  if (scanning) return;
  scanning = true;
  try {
    await ctx.services.scanner.scanAll({});
    ctx.services.faults.diagnose();
    ctx.services.dedupe.scan();
    try { await ctx.services.updates.checkAll({ useNpm: false }); } catch (err) { ctx.logger.warn('版本比对失败', { error: String(err && err.message) }); }
    if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send(EVENT_CHANNEL, { topic: 'scan', message: '首次扫描完成', level: 'info', ts: now(), data: {} });
  } catch (err) {
    ctx.logger.error('启动扫描失败', { error: String(err && err.message) });
  } finally { scanning = false; }
}

// 每 1 分钟刷新智能体状态；每 15 分钟可选增量扫描
function scheduleBackground() {
  setInterval(async () => {
    try { await ctx.services.agents.scan(); } catch { /* ignore */ }
  }, 60 * 1000);
  setInterval(async () => {
    const s = settingsAll();
    if (s.autoScan === false) return;
    try {
      if (!scanning) {
        scanning = true;
        await ctx.services.scanner.scanAll({});
        ctx.services.faults.diagnose();
        scanning = false;
      }
    } catch { scanning = false; }
  }, 15 * 60 * 1000);
}

function settingsAll() {
  const rows = ctx ? ctx.db.all('SELECT key, value FROM settings') : [];
  return Object.fromEntries(rows.map((r) => [r.key, jsonParse(r.value, r.value)]));
}

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1480,
    height: 940,
    minWidth: 1080,
    minHeight: 680,
    show: false,
    backgroundColor: '#0b0f14',
    title: PRODUCT_TITLE,
    icon: path.join(here, '..', '..', 'build', 'icon.png'),
    webPreferences: {
      preload: path.join(here, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false,
      spellcheck: false
    }
  });
  mainWindow.once('ready-to-show', () => { mainWindow.show(); if (process.env.CC_SHOT) captureAndQuit(); });
  mainWindow.on('closed', () => { mainWindow = null; });

  if (DEV) {
    mainWindow.loadURL('http://127.0.0.1:5173');
    mainWindow.webContents.openDevTools({ mode: 'detach' });
  } else {
    const index = path.join(here, '..', '..', 'dist-renderer', 'index.html');
    mainWindow.loadFile(index);
  }
  mainWindow.webContents.setWindowOpenHandler(({ url }) => { shell.openExternal(url); return { action: 'deny' }; });
}

async function captureAndQuit() {
  const target = process.env.CC_SHOT;
  if (!target) return;
  const views = (process.env.CC_SHOT_VIEWS || 'overview').split(',');
  if (process.env.CC_SHOT_WAIT === '1') {
    // 等待「扫描真正结束」，而不是等第一行入库——否则截到的是半成品状态
    for (let i = 0; i < 240; i++) {
      const total = ctx.db.get('SELECT COUNT(*) AS c FROM capabilities').c;
      if (!scanning && total > 0) break;
      await new Promise((r) => setTimeout(r, 1000));
    }
    await new Promise((r) => setTimeout(r, 1200));
  }
  try {
    for (let i = 0; i < views.length; i++) {
      const view = views[i];
      await mainWindow.webContents.executeJavaScript('window.__ccGoto && window.__ccGoto(' + JSON.stringify(view) + ')');
      await new Promise((r) => setTimeout(r, 1100));
      const img = await mainWindow.webContents.capturePage();
      const file = path.join(target, view + '.png');
      fs.mkdirSync(target, { recursive: true });
      fs.writeFileSync(file, img.toPNG());
      console.log('captured', file);
    }
  } catch (err) {
    console.error('capture failed', err);
  }
  setTimeout(() => { app.quit(); }, 300);
}

// ---------------- IPC ----------------
function ok(data) { return { ok: true, data }; }
function fail(err) { return { ok: false, error: String(err && err.message || err) }; }
function register(channel, fn) {
  ipcMain.handle(channel, async (_e, payload) => {
    try { return ok(await fn(payload)); } catch (err) { ctx && ctx.logger.error('IPC 失败 ' + channel, { error: String(err && err.message) }); return fail(err); }
  });
}

function registerIpc() {
  const s = () => ctx.services;
  register('status', async () => ({
    version: '1.0.0', home: HOME, dbPath: DB_PATH, assetDir: ASSET_DIR, port: PORT,
    stats: s().registry.stats(), db: ctx.db.stats(),
    assets: s().assets.stats(),
    serverRunning: Boolean(server), dev: DEV,
    recent: ctx.bus.recent(12)
  }));
  register('db.stats', async () => {
    const tables = ctx.db.all("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'search_%' ORDER BY name");
    const info = tables.map((t) => {
      try { return { name: t.name, rows: ctx.db.get('SELECT COUNT(*) c FROM "' + t.name + '"').c }; }
      catch { return { name: t.name, rows: 0 }; }
    });
    return { db: ctx.db.stats(), tables: info, file: DB_PATH, wal: DB_PATH + '-wal' };
  });

  register('capabilities.list', async (filter) => s().registry.listCapabilities(filter || {}));
  register('capabilities.get', async (id) => s().registry.getCapability(id));
  register('capabilities.read', async (id) => {
    const cap = s().registry.getCapability(id);
    if (!cap) throw new Error('能力不存在');
    const inst = cap.installations.sort((a, b) => b.updatedAt - a.updatedAt)[0];
    return { id, content: inst ? inst.content : '', path: inst ? inst.path : '', title: cap.displayName };
  });
  register('capabilities.action', async ({ id, action, value }) => s().lifecycle.action(id, action, value));
  register('capabilities.export', async (id) => s().lifecycle.exportCapability(id));
  register('capabilities.import', async ({ sourcePath, options }) => {
    const res = s().imports.importPath(sourcePath, options || {});
    await s().scanner.scanAll({});
    s().faults.diagnose();
    return res;
  });
  register('capabilities.importPreview', async (sourcePath) => s().imports.preview(sourcePath));

  // ---- 能力资产库（基线 §10 新增）----
  register('assets.list', async (filter) => s().assets.list(filter || {}));
  register('assets.get', async (id) => s().assets.get(id));
  register('assets.stats', async () => s().assets.stats());
  register('assets.promote', async ({ capabilityId, kind }) => s().assets.promote({ capabilityId, kind }));
  register('assets.create', async ({ kind, name, content, summary }) => s().assets.create({ kind, name, content, summary }));
  register('assets.remove', async ({ id }) => s().assets.remove({ id }));

  register('search.query', async ({ query, options }) => s().search.search(query, options || {}));
  register('search.reindex', async () => { s().registry.reindexSearch(); return { ok: true }; });

  register('sources.list', async () => s().scanner.listSources());
  register('scan.run', async (options) => {
    // 顺序同 CLI：资产库先登记，扫描器随后复用其记录
    try { s().assets.syncFromDisk(); } catch { /* ignore */ }
    const res = await s().scanner.scanAll(options || {});
    s().faults.diagnose();
    return res;
  });
  register('sources.add', async (payload) => {
    const id = 'src_' + Date.now().toString(36);
    const ts = now();
    ctx.db.run('INSERT INTO sources(id, kind, host, label, root_path, format, enabled, builtin, scan_interval, created_at, updated_at, config) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)',
      [id, payload.kind || 'skill', payload.host || 'custom', payload.label || payload.rootPath, payload.rootPath, payload.format || 'auto', 1, 0, 900, ts, ts, JSON.stringify({ depth: payload.depth ?? 2 })]);
    return { id };
  });
  register('sources.toggle', async ({ id, enabled }) => {
    ctx.db.run('UPDATE sources SET enabled=?, updated_at=? WHERE id=?', [enabled ? 1 : 0, now(), id]);
    return { ok: true };
  });
  register('sources.remove', async (id) => {
    const row = ctx.db.get('SELECT * FROM sources WHERE id = ?', [id]);
    if (row && row.builtin) throw new Error('内置来源不可删除，可停用');
    ctx.db.run('DELETE FROM sources WHERE id = ?', [id]);
    return { ok: true };
  });

  register('agents.list', async () => s().agents.list());
  register('agents.scan', async () => s().agents.scan());
  register('agents.sessions', async () => s().agents.sessions());
  // 智能体连通检测 / 主动连通（基线 §10 新增）
  register('agents.probe', async ({ host }) => s().agents.probe(host));
  register('agents.connect', async ({ host }) => s().agents.connect(host, { port: PORT }));

  register('faults.list', async (filter) => s().faults.list(filter || {}));
  register('faults.diagnose', async () => s().faults.diagnose());
  register('faults.autofix', async (id) => s().faults.autoFix(id));
  register('faults.dispatch', async ({ id, agentId, note }) => s().faults.dispatch(id, agentId, note));
  register('faults.repairs', async (faultId) => s().faults.repairs(faultId));
  register('faults.stats', async () => s().faults.stats());

  register('duplicates.list', async (decision) => s().dedupe.list({ decision }));
  register('duplicates.scan', async () => s().dedupe.scan());
  register('duplicates.decide', async ({ id, action, chosenId }) => s().dedupe.decide(id, action, chosenId));

  register('updates.list', async (status) => s().updates.list({ status }));
  register('updates.catalog', async (payload) => {
    const { query, options } = payload || {};
    return s().updates.catalog(query, options || {});
  });
  register('updates.importCatalog', async () => s().updates.importCatalog());
  register('updates.check', async (options) => s().updates.checkAll(options || {}));
  register('updates.apply', async (capabilityId) => s().lifecycle.snapshot(capabilityId, 'pre-update'));

  register('workflows.list', async () => s().workflows.list());
  register('workflows.run', async (id) => s().workflows.run(id, 'manual'));
  register('workflows.runs', async () => s().workflows.runs(50));

  register('events.recent', async (limit) => ctx.bus.recent(limit || 200));

  // ---- 模型接入（基线 §10 新增）----
  register('models.providers', async () => s().models.providers());
  register('models.saveProvider', async (payload) => s().models.saveProvider(payload || {}));
  register('models.removeProvider', async (id) => s().models.removeProvider(id));
  register('models.testProvider', async (id) => s().models.testProvider(id));
  register('models.routes', async () => s().models.routes());
  register('models.setRoute', async (payload) => s().models.setRoute(payload || {}));
  register('models.usage', async (payload) => s().models.usage({ limit: (payload && payload.limit) || 50 }));
  register('models.health', async () => s().models.health());

  register('ecosystem.targets', async () => s().ecosystem.targets());
  register('ecosystem.preview', async (host) => s().ecosystem.previewMcp(host, { port: PORT }));
  register('ecosystem.apply', async (host) => s().ecosystem.applyMcp(host, { port: PORT }));
  register('ecosystem.backups', async () => s().ecosystem.backups());
  register('ecosystem.restore', async (id) => s().ecosystem.restore(id));

  register('settings.get', async () => settingsAll());
  register('settings.set', async (patch) => {
    for (const [k, v] of Object.entries(patch || {})) {
      ctx.db.run('INSERT INTO settings(key, value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value', [k, JSON.stringify(v)]);
    }
    return settingsAll();
  });

  register('lifecycle.trash', async () => s().lifecycle.trash());
  register('server.info', async () => ({ port: PORT, running: Boolean(server), mcp: 'http://127.0.0.1:' + PORT + '/mcp' }));
  register('server.restart', async () => {
    if (server) await server.close();
    server = createHttpServer(ctx, ctx.services, {});
    const addr = await server.listen(PORT, '127.0.0.1');
    return { port: addr.port };
  });

  ipcMain.handle('dialog.pickDirectory', async () => {
    const r = await dialog.showOpenDialog(mainWindow, { properties: ['openDirectory', 'multiSelections'] });
    return r.canceled ? [] : r.filePaths;
  });
  ipcMain.handle('dialog.pickFiles', async () => {
    const r = await dialog.showOpenDialog(mainWindow, { properties: ['openFile', 'multiSelections'] });
    return r.canceled ? [] : r.filePaths;
  });
  ipcMain.handle('shell.openExternal', async (_e, url) => { await shell.openExternal(String(url)); return { ok: true }; });
  ipcMain.handle('shell.openPath', async (_e, p) => { await shell.openPath(String(p)); return { ok: true }; });
}

app.whenReady().then(async () => {
  Menu.setApplicationMenu(null);
  await boot();
  registerIpc();
  createWindow();
  app.on('activate', () => { if (BrowserWindow.getAllWindows().length === 0) createWindow(); });
});

app.on('window-all-closed', async () => {
  if (process.platform !== 'darwin') {
    try { if (server) await server.close(); } catch { /* ignore */ }
    closeContext(ctx);
    app.quit();
  }
});

process.on('uncaughtException', (err) => {
  try { ctx && ctx.logger.error('未捕获异常', { error: String(err && err.stack || err) }); } catch { /* ignore */ }
});
process.on('unhandledRejection', (err) => {
  try { ctx && ctx.logger.error('未处理 Promise 拒绝', { error: String(err && err.stack || err) }); } catch { /* ignore */ }
});
