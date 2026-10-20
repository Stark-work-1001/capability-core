const { contextBridge, ipcRenderer } = require('electron');

// 预加载桥：渲染进程只能通过白名单调用主进程方法（无 nodeIntegration）。
// 暴露名固定为 window.capabilityCore —— 渲染层 lib/api.ts 直接按方法名取属性，
// 因此下面的方法名属于对外契约，改名必须与渲染层同步。
const invoke = (channel, ...args) => ipcRenderer.invoke(channel, ...args);

contextBridge.exposeInMainWorld('capabilityCore', {
  // ---- 总览 / 数据库 ----
  status: () => invoke('status'),
  dbStats: () => invoke('db.stats'),

  // ---- 宿主能力库（原「能力库」）----
  capabilities: (filter) => invoke('capabilities.list', filter),
  capability: (id) => invoke('capabilities.get', id),
  capabilityAction: (id, action, value) => invoke('capabilities.action', { id, action, value }),
  readCapability: (id) => invoke('capabilities.read', id),
  importCapability: (sourcePath, options) => invoke('capabilities.import', { sourcePath, options }),
  previewImport: (sourcePath) => invoke('capabilities.importPreview', sourcePath),
  pickDirectory: () => invoke('dialog.pickDirectory'),
  pickFiles: () => invoke('dialog.pickFiles'),

  // ---- 能力资产库（自有资产）----
  assetsList: (filter) => invoke('assets.list', filter),
  assetsGet: (id) => invoke('assets.get', id),
  assetsStats: () => invoke('assets.stats'),
  assetsPromote: (payload) => invoke('assets.promote', payload || {}),
  assetsCreate: (payload) => invoke('assets.create', payload || {}),
  assetsRemove: (payload) => invoke('assets.remove', payload || {}),

  // ---- 检索 ----
  search: (query, options) => invoke('search.query', { query, options }),
  reindex: () => invoke('search.reindex'),

  // ---- 来源与扫描 ----
  sources: () => invoke('sources.list'),
  scan: (options) => invoke('scan.run', options),
  addSource: (payload) => invoke('sources.add', payload),
  toggleSource: (id, enabled) => invoke('sources.toggle', { id, enabled }),
  removeSource: (id) => invoke('sources.remove', id),

  // ---- 智能体接入（含连通检测 / 主动连通）----
  agents: () => invoke('agents.list'),
  scanAgents: () => invoke('agents.scan'),
  sessions: () => invoke('agents.sessions'),
  probeAgent: (payload) => invoke('agents.probe', payload || {}),
  connectAgent: (payload) => invoke('agents.connect', payload || {}),

  // ---- 维修站 ----
  faults: (filter) => invoke('faults.list', filter),
  diagnose: () => invoke('faults.diagnose'),
  autoFix: (id) => invoke('faults.autofix', id),
  dispatchFault: (id, agentId, note) => invoke('faults.dispatch', { id, agentId, note }),
  repairs: (faultId) => invoke('faults.repairs', faultId),
  faultStats: () => invoke('faults.stats'),

  // ---- 去重裁决 ----
  duplicates: (decision) => invoke('duplicates.list', decision),
  scanDuplicates: () => invoke('duplicates.scan'),
  decideDuplicate: (id, action, chosenId) => invoke('duplicates.decide', { id, action, chosenId }),

  // ---- 更新管理 ----
  updates: (status) => invoke('updates.list', status),
  catalog: (query, options) => invoke('updates.catalog', { query, options }),
  importCatalog: () => invoke('updates.importCatalog'),
  checkUpdates: (options) => invoke('updates.check', options),
  applyUpdate: (capabilityId) => invoke('updates.apply', capabilityId),

  // ---- 工作流 ----
  workflows: () => invoke('workflows.list'),
  runWorkflow: (id) => invoke('workflows.run', id),
  workflowRuns: () => invoke('workflows.runs'),

  // ---- 模型接入 ----
  modelProviders: () => invoke('models.providers'),
  saveModelProvider: (payload) => invoke('models.saveProvider', payload || {}),
  removeModelProvider: (id) => invoke('models.removeProvider', id),
  testModelProvider: (id) => invoke('models.testProvider', id),
  modelRoutes: () => invoke('models.routes'),
  setModelRoute: (payload) => invoke('models.setRoute', payload || {}),
  modelUsage: (payload) => invoke('models.usage', payload || {}),
  modelHealth: () => invoke('models.health'),

  // ---- 事件流 / 扫描状态 ----
  events: (limit) => invoke('events.recent', limit),
  scanBusy: () => invoke('scan.busy'),
  onEvent: (cb) => {
    const listener = (_e, evt) => cb(evt);
    ipcRenderer.on('capability-core:event', listener);
    return () => ipcRenderer.removeListener('capability-core:event', listener);
  },

  // ---- 生态同步 ----
  ecosystemTargets: () => invoke('ecosystem.targets'),
  previewSync: (host) => invoke('ecosystem.preview', host),
  applySync: (host) => invoke('ecosystem.apply', host),
  backups: () => invoke('ecosystem.backups'),
  restoreBackup: (id) => invoke('ecosystem.restore', id),

  // ---- 设置 / 系统 ----
  settings: () => invoke('settings.get'),
  saveSettings: (patch) => invoke('settings.set', patch),
  openExternal: (url) => invoke('shell.openExternal', url),
  openPath: (p) => invoke('shell.openPath', p),
  exportCapability: (id) => invoke('capabilities.export', id),
  trash: () => invoke('lifecycle.trash'),
  serverInfo: () => invoke('server.info'),
  restartServer: () => invoke('server.restart')
});
