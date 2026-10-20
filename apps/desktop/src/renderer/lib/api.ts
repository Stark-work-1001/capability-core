// 与主进程约定的类型化桥接层。所有界面数据都从这里来。
//
// 桥接对象：`window.capabilityCore`（由 preload 通过 contextBridge 暴露）。
// 通道命名约定：主进程侧为 `<域>.<动作>`（见基线 §10），preload 暴露为 camelCase 方法名。
// 新增通道的桥接方法名候选以 '|' 分隔，取第一个真实存在的实现，后端定稿后任选其一即可。
export type Result<T> = { ok: true; data: T } | { ok: false; error: string };

export interface CapabilitySummary {
  id: string;
  kind: 'skill' | 'mcp' | 'plugin' | 'prompt' | 'agent' | string;
  name: string;
  displayName: string;
  summary: string;
  author: string;
  version: string;
  status: string;
  health: string;
  tags: string[];
  protocols: string[];
  hosts: string[];
  usageCount: number;
  updatedAt: number;
  /** 归属：own = 能力资产库（自有），host = 宿主能力库（扫描登记） */
  scope?: 'own' | 'host' | string;
  updateStatus?: string;
  latestVersion?: string;
  metadata?: Record<string, any>;
}

export interface CapabilityDetail extends CapabilitySummary {
  description: string;
  authorSource: string;
  homepage: string;
  license: string;
  verifyState: string;
  verifyMessage: string;
  installs?: any[];
  installations: { id: string; path: string; entry: string; host: string; size: number; version: string; content: string; manifest: any; fingerprint?: string }[];
  faults: any[];
  updates: any;
  usage: any[];
  /** 首次被扫描发现的时间戳 */
  firstSeen?: number;
  /** 是否被固定（固定后不参与自动归档） */
  pinned?: boolean;
}

export interface SearchHit {
  id: string; kind: string; name: string; displayName: string; summary: string; author: string; version: string;
  status: string; health: string; tags: string[]; hosts: { host: string; label: string }[];
  protocols: string[]; score: number; explanation: Record<string, any>; snippet: string;
}

export interface SearchResult { query: string; terms: string[]; took: number; total: number; mode: string; items: SearchHit[] }
export interface SourceRow { id: string; kind: string; host: string; label: string; root_path: string; enabled: number; builtin: number; last_scan_at: number; last_scan_status: string; last_scan_message: string; last_scan_count: number; last_scan_ms: number }
export interface AgentRow { id: string; host: string; label: string; kind: string; pid?: number; status: string; protocol: string; endpoint: string; lastSeen: number; capabilityCount: number; metadata: any }
export interface FaultRow { id: string; capabilityId: string; capabilityName: string; kind: string; severity: string; code: string; title: string; detail: string; evidence: any; status: string; autoFixable: boolean; dispatchAgent?: string; detectedAt: number }
export interface DuplicateGroup { id: string; kind: string; canonicalName: string; similarity: number; reason: string; recommended: string; decision: string; members: { id: string; name: string; displayName: string; version: string; author: string; status: string; path: string; host: string; updatedAt: number }[] }
export interface UpdateRow { id: string; capabilityId: string; name: string; kind: string; sourceKind: string; currentVersion: string; latestVersion: string; status: string; detail: string; checkedAt: number; canonicalName?: string }
export interface WorkflowRow { id: string; name: string; kind: string; description: string; steps: { type: string; label: string }[]; lastRun: { id: string; status: string; startedAt: number; endedAt: number; summary: any; steps: any[] } | null }
export interface EventRow { id: number; ts: number; level: string; topic: string; message: string; capabilityId?: string; agentId?: string; data: any }
export interface StatusInfo { version: string; home: string; dbPath: string; port: number; stats: any; db: any; serverRunning: boolean; dev: boolean; recent: EventRow[] }

/* ---------------- 能力资产库（基线 §5 / §10） ---------------- */

/** assets.list —— 只返回 scope='own' 的能力资产 */
export interface AssetList { items: CapabilitySummary[]; total: number }
/** assets.get —— 单个能力资产的正文与落盘路径 */
export interface AssetContent { id: string; content: string; path: string; title: string; kind: string }
/** assets.stats —— 资产库总览 */
export interface AssetStats {
  total: number;
  /** 兼容两种形态：{ kind, c }[] 或 { [kind]: count } */
  byKind: { kind: string; c: number }[] | Record<string, number>;
  assetDir: string;
  sizeBytes: number;
}
/** assets.promote —— 宿主能力 → 能力资产 */
export interface PromoteResult { assetId: string; path: string }
/** assets.create —— 新建能力资产 */
export interface CreateAssetResult { id: string; path: string }

/* ---------------- 智能体连通（基线 §10） ---------------- */

export interface ProbeCheck { name: string; ok: boolean; detail: string }
/** agents.probe —— 连通检测：只读探测，不写任何文件 */
export interface ProbeResult { ok: boolean; latencyMs: number; checks: ProbeCheck[] }
/** agents.connect —— 主动连通：写入宿主原生配置并返回备份号 */
export interface ConnectResult { ok: boolean; endpoint: string; written: string[]; backupId?: string }

/* ---------------- 模型接入（基线 §9 / §10） ---------------- */

/** 提供商。apiKey 恒为打码值（前 6 位 + •••• + 后 4 位），任何通道都不得回传完整密钥。 */
export interface ModelProviderRow {
  id: string;
  name: string;
  kind?: string;
  baseUrl?: string;
  base_url?: string;
  apiKey?: string;
  api_key?: string;
  enabled?: boolean | number;
  isDefault?: boolean | number;
  is_default?: boolean | number;
  extraHeaders?: string;
  lastCheckAt?: number;
  lastCheck_at?: number;
  lastCheckStatus?: string;
  last_check_status?: string;
  lastCheckMessage?: string;
  last_check_message?: string;
  createdAt?: number;
  updatedAt?: number;
  [key: string]: any;
}

/** 角色 → (提供商, 模型) 路由。角色：embedding | rerank | judge | general */
export interface ModelRouteRow {
  role: string;
  providerId?: string;
  provider_id?: string;
  model: string;
  dim?: number | null;
  params?: string | Record<string, any>;
  updatedAt?: number;
  [key: string]: any;
}

/** models.routes —— 路由表 + 可选模型清单 */
export interface ModelRoutesPayload {
  routes: ModelRouteRow[];
  /** 提供商可用模型清单：{ [providerId]: string[] } 或扁平数组 */
  models?: Record<string, string[]> | string[];
  [key: string]: any;
}

/** models.testProvider —— 单提供商连通测试 */
export interface ProviderTestResult { ok: boolean; latencyMs: number; message: string; models?: string[] }

/** models.usage —— 调用统计 */
export interface ModelUsagePayload {
  calls: number;
  /** 兼容两种形态：{ role, c }[] 或 { [role]: count } */
  byRole?: { role: string; c: number }[] | Record<string, number>;
  recent?: {
    id?: number; ts: number; providerId?: string; provider_id?: string; role?: string; model?: string;
    action?: string; latencyMs?: number; latency_ms?: number; ok?: boolean | number; tokens?: number; error?: string;
  }[];
  [key: string]: any;
}

/** models.health —— 提供商 / 路由健康度 + 本地兜底状态 */
export interface ModelHealth {
  providers: any[];
  routes: any[];
  localFallback: boolean;
  [key: string]: any;
}

declare global {
  interface Window {
    capabilityCore: any;
  }
}

async function call<T>(channel: string, ...args: any[]): Promise<T> {
  const bridge = window.capabilityCore;
  if (!bridge) throw new Error('能力核心桥接层未就绪：preload 未加载');
  // '|' 分隔的候选方法名，取第一个已实现者
  const key = channel.split('|').find((k) => typeof bridge[k] === 'function');
  if (!key) throw new Error('主进程尚未实现通道：' + channel.split('|')[0]);
  const res = await bridge[key](...args);
  if (res && res.ok === true) return res.data as T;
  if (res && res.ok === false) throw new Error(res.error || '调用失败');
  return res as T;
}

export const api = {
  status: () => call<StatusInfo>('status'),
  dbStats: () => call<any>('dbStats'),
  capabilities: (filter?: any) => call<CapabilitySummary[]>('capabilities', filter),
  capability: (id: string) => call<CapabilityDetail>('capability', id),
  capabilityAction: (id: string, action: string, value?: any) => call<any>('capabilityAction', id, action, value),
  readCapability: (id: string) => call<{ content: string; path: string; title: string }>('readCapability', id),
  importCapability: (p: string, options?: any) => call<any>('importCapability', p, options),
  previewImport: (p: string) => call<any>('previewImport', p),
  pickDirectory: () => call<string[]>('pickDirectory'),
  pickFiles: () => call<string[]>('pickFiles'),
  search: (q: string, options?: any) => call<SearchResult>('search', q, options),
  reindex: () => call<any>('reindex'),
  sources: () => call<SourceRow[]>('sources'),
  scan: (options?: any) => call<any>('scan', options),
  addSource: (p: any) => call<any>('addSource', p),
  toggleSource: (id: string, enabled: boolean) => call<any>('toggleSource', id, enabled),
  removeSource: (id: string) => call<any>('removeSource', id),
  agents: () => call<AgentRow[]>('agents'),
  scanAgents: () => call<AgentRow[]>('scanAgents'),
  sessions: () => call<any[]>('sessions'),
  probeAgent: (host: string) => call<ProbeResult>('probeAgent|agentsProbe|probe|agentProbe', { host }),
  connectAgent: (host: string) => call<ConnectResult>('connectAgent|agentsConnect|connect|agentConnect', { host }),
  faults: (filter?: any) => call<FaultRow[]>('faults', filter),
  diagnose: () => call<any>('diagnose'),
  autoFix: (id: string) => call<any>('autoFix', id),
  dispatchFault: (id: string, agentId: string, note: string) => call<any>('dispatchFault', id, agentId, note),
  repairs: (faultId?: string) => call<any[]>('repairs', faultId),
  faultStats: () => call<any>('faultStats'),
  duplicates: (decision?: string) => call<DuplicateGroup[]>('duplicates', decision),
  scanDuplicates: () => call<any>('scanDuplicates'),
  decideDuplicate: (id: string, action: string, chosenId?: string) => call<any>('decideDuplicate', id, action, chosenId),
  updates: (status?: string) => call<UpdateRow[]>('updates', status),
  checkUpdates: (options?: any) => call<any>('checkUpdates', options),
  catalog: (query?: string, options?: any) => call<any[]>('catalog', query, options),
  importCatalog: () => call<any>('importCatalog'),
  applyUpdate: (id: string) => call<any>('applyUpdate', id),
  workflows: () => call<WorkflowRow[]>('workflows'),
  runWorkflow: (id: string) => call<any>('runWorkflow', id),
  workflowRuns: () => call<any[]>('workflowRuns'),
  events: (limit?: number) => call<EventRow[]>('events', limit),
  scanBusy: () => call<{ busy: boolean }>('scanBusy'),
  // 桥接层缺失时不让订阅拖垮整个 App：退化为空订阅
  onEvent: (cb: (e: EventRow) => void) => {
    try {
      const bridge = window.capabilityCore;
      if (!bridge || typeof bridge.onEvent !== 'function') return () => {};
      return bridge.onEvent(cb);
    } catch { return () => {}; }
  },
  ecosystemTargets: () => call<any[]>('ecosystemTargets'),
  previewSync: (host: string) => call<any>('previewSync', host),
  applySync: (host: string) => call<any>('applySync', host),
  backups: () => call<any[]>('backups'),
  restoreBackup: (id: string) => call<any>('restoreBackup', id),
  settings: () => call<Record<string, any>>('settings'),
  saveSettings: (patch: any) => call<Record<string, any>>('saveSettings', patch),
  openExternal: (url: string) => call<any>('openExternal', url),
  openPath: (p: string) => call<any>('openPath', p),
  exportCapability: (id: string) => call<any>('exportCapability', id),
  trash: () => call<any[]>('trash'),
  serverInfo: () => call<any>('serverInfo'),
  restartServer: () => call<any>('restartServer'),

  /* ---- 能力资产库（自有资产，基线 §5 / §10） ---- */
  assets: (filter?: { kind?: string; query?: string; limit?: number }) => call<AssetList>('assetsList|listAssets|assets.list', filter || {}),
  asset: (id: string) => call<AssetContent>('assetsGet|getAsset|readAsset|assets.get', id),
  assetStats: () => call<AssetStats>('assetsStats|assetStats|assets.stats'),
  promoteAsset: (capabilityId: string, kind?: string) => call<PromoteResult>('assetsPromote|promoteAsset|assets.promote', { capabilityId, kind }),
  createAsset: (payload: { kind: string; name: string; content: string; summary?: string }) => call<CreateAssetResult>('assetsCreate|createAsset|assets.create', payload),
  removeAsset: (id: string) => call<{ ok: boolean }>('assetsRemove|removeAsset|assets.remove', { id }),

  /* ---- 模型接入（基线 §9 / §10） ---- */
  modelProviders: () => call<ModelProviderRow[]>('modelProviders|modelsProviders|providers|models.providers'),
  saveModelProvider: (payload: { id?: string; name: string; baseUrl: string; apiKey?: string; kind?: string; enabled?: boolean }) =>
    call<ModelProviderRow[]>('saveModelProvider|modelsSaveProvider|saveProvider|models.saveProvider', payload),
  removeModelProvider: (id: string) => call<{ ok: boolean }>('removeModelProvider|modelsRemoveProvider|removeProvider|models.removeProvider', id),
  testModelProvider: (id: string) => call<ProviderTestResult>('testModelProvider|modelsTestProvider|testProvider|models.testProvider', id),
  modelRoutes: () => call<ModelRoutesPayload>('modelRoutes|modelsRoutes|routes|models.routes'),
  setModelRoute: (payload: { role: string; providerId: string; model: string; params?: any }) =>
    call<ModelRouteRow[] | ModelRoutesPayload>('setModelRoute|modelsSetRoute|setRoute|models.setRoute', payload),
  modelUsage: (limit?: number) => call<ModelUsagePayload>('modelUsage|modelsUsage|usage|models.usage', { limit }),
  modelHealth: () => call<ModelHealth>('modelHealth|modelsHealth|health|models.health')
};

export function fmtTime(ts?: number | null) {
  if (!ts) return '—';
  const d = new Date(ts);
  const now = Date.now();
  const diff = now - ts;
  if (diff < 60_000) return '刚刚';
  if (diff < 3_600_000) return Math.floor(diff / 60_000) + ' 分钟前';
  if (diff < 86_400_000) return Math.floor(diff / 3_600_000) + ' 小时前';
  if (diff < 7 * 86_400_000) return Math.floor(diff / 86_400_000) + ' 天前';
  return d.toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit' }) + ' ' + d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
}

export function fmtBytes(n?: number) {
  if (!n) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let i = 0; let v = n;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return (v >= 10 || i === 0 ? Math.round(v) : v.toFixed(1)) + ' ' + units[i];
}

export const KIND_LABEL: Record<string, string> = {
  skill: '技能', mcp: 'MCP', plugin: '插件', prompt: '提示词', agent: '智能体', hook: '钩子'
};
export const KIND_ICON: Record<string, string> = { skill: '◆', mcp: '⬡', plugin: '❖', prompt: '¶', agent: '◉', hook: '⇥' };
export const HOST_LABEL: Record<string, string> = {
  dsh: 'DeepSeek Harness', codex: 'Codex', claude: 'Claude Code', minimax: 'MiniMax Code',
  gemini: 'Gemini / Antigravity', workbuddy: 'WorkBuddy', cursor: 'Cursor', qoder: 'Qoder', custom: '自定义'
};
export const STATUS_LABEL: Record<string, string> = {
  active: '已启用', disabled: '已停用', quarantined: '已隔离', archived: '已归档',
  healthy: '健康', warning: '警告', broken: '故障', unknown: '未检测', untriaged: '待诊断'
};
/** 能力三分：自有资产 / 宿主索引（基线 §5） */
export const SCOPE_LABEL: Record<string, string> = {
  own: '能力资产', host: '宿主能力'
};
/** 模型角色（基线 §9） */
export const MODEL_ROLE_LABEL: Record<string, string> = {
  embedding: '向量嵌入', rerank: '精排', judge: '判定', general: '通用兜底'
};
export const MODEL_ROLE_ICON: Record<string, string> = {
  embedding: '⁙', rerank: '⇅', judge: '⚖', general: '✦'
};
