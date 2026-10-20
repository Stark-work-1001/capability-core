import { now } from '../core/util.js';

// 模型接入服务（基线 §9）
// 设计约束：
// 1) OpenAI 兼容协议，提供商可增删改，模型角色与具体型号解耦（model_routes 决定 role→(provider, model)）；
// 2) 任何云端调用都必须：超时（默认 15s）→ 失败重试 1 次 → 仍失败则**静默降级到本地算法**，绝不抛错中断调用方；
// 3) 每次调用（成功或失败）都写入 model_calls，供「模型接入」页统计；
// 4) 密钥只在 SQLite 内明文保存一次，任何对外返回一律打码（前 6 + •••• + 后 4）。

export const MODEL_ROLES = ['embedding', 'rerank', 'judge', 'general'];
export const DEFAULT_TIMEOUT_MS = 15000;
export const DEFAULT_RETRIES = 1;

// 默认提供商（硅基流动）。仅作为初始种子写入 model_providers。
// 密钥不写入源码：优先读环境变量 CC_MODEL_API_KEY，其次读 CC_MODEL_BASE_URL 指定的服务。
// 用户可在「模型接入」界面里填写自己的 API Key（只存本地数据库，界面恒为打码显示）。
export const DEFAULT_PROVIDER_ID = 'siliconflow';
export const DEFAULT_PROVIDER = {
  id: DEFAULT_PROVIDER_ID,
  name: '硅基流动 SiliconFlow',
  kind: 'openai-compatible',
  baseUrl: process.env.CC_MODEL_BASE_URL || 'https://api.siliconflow.cn/v1',
  apiKey: process.env.CC_MODEL_API_KEY || '',
  extraHeaders: {},
  config: { timeoutMs: DEFAULT_TIMEOUT_MS, retries: DEFAULT_RETRIES },
  isDefault: true
};

// 四种角色 + 已实测型号（embedding 输出 4096 维）
export const DEFAULT_ROUTES = [
  { role: 'embedding', providerId: DEFAULT_PROVIDER_ID, model: 'Qwen/Qwen3-VL-Embedding-8B', dim: 4096 },
  { role: 'rerank', providerId: DEFAULT_PROVIDER_ID, model: 'Qwen/Qwen3-VL-Reranker-8B', dim: null },
  { role: 'judge', providerId: DEFAULT_PROVIDER_ID, model: 'Qwen/Qwen3.8-27B', dim: null },
  { role: 'general', providerId: DEFAULT_PROVIDER_ID, model: 'Qwen/Qwen3.8-27B', dim: null }
];

// 密钥打码：前 6 + •••• + 后 4。短密钥整体打码，避免泄露。
export function maskKey(key) {
  const s = String(key == null ? '' : key);
  if (!s) return '';
  if (s.length <= 12) return '••••';
  return s.slice(0, 6) + '••••' + s.slice(-4);
}

function withTimeout(promise, ms, label) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error((label || '请求') + '超时（' + ms + 'ms）')), ms);
    if (timer.unref) timer.unref();
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

// 本地兜底向量化：确定性哈希（字符 n-gram → 桶），L2 归一化。
// 语义质量远不如云端模型，但保证「云端不可用时检索链路不中断」。
export function localEmbed(text, dim) {
  const d = Math.max(8, Number(dim) || 4096);
  const vec = new Float64Array(d);
  const s = String(text || '');
  for (let i = 0; i < s.length; i++) {
    const ch = s.charCodeAt(i);
    vec[(ch * 2654435761) % d] += 1;
    if (i + 1 < s.length) {
      const bi = (ch * 131 + s.charCodeAt(i + 1) * 257) % d;
      vec[bi] += 0.5;
    }
  }
  let norm = 0;
  for (let i = 0; i < d; i++) norm += vec[i] * vec[i];
  norm = Math.sqrt(norm) || 1;
  const out = new Array(d);
  for (let i = 0; i < d; i++) out[i] = vec[i] / norm;
  return out;
}

// 本地兜底精排：词面覆盖度打分（等价于 BM25 的简化形式），用于云端 rerank 不可用时保持排序可用。
export function localRerank(query, documents) {
  const q = tokenizeLoose(query);
  const qSet = new Set(q);
  const docs = Array.isArray(documents) ? documents : [];
  const scored = docs.map((doc, index) => {
    const d = tokenizeLoose(doc);
    const dSet = new Set(d);
    let hit = 0;
    for (const t of qSet) if (dSet.has(t)) hit++;
    const coverage = qSet.size ? hit / qSet.size : 0;
    const density = d.length ? hit / Math.sqrt(d.length) : 0;
    // 连续短语命中额外加权，避免「词都在但语义无关」排到前面
    const phrase = String(doc || '').includes(String(query || '').trim()) && String(query || '').trim() ? 0.35 : 0;
    return { index, document: doc, score: Number((coverage * 0.7 + density * 0.3 + phrase).toFixed(6)) };
  });
  scored.sort((a, b) => b.score - a.score || a.index - b.index);
  return scored;
}

function tokenizeLoose(text) {
  const s = String(text || '').toLowerCase();
  const out = [];
  const words = s.match(/[a-z0-9_]+/g) || [];
  out.push(...words);
  const cjk = s.match(/[\u4e00-\u9fff]/g) || [];
  out.push(...cjk);
  for (let i = 0; i + 1 < cjk.length; i++) out.push(cjk[i] + cjk[i + 1]);
  return out;
}

export class ModelService {
  constructor(ctx) {
    this.ctx = ctx;
    this.db = ctx.db;
    this.bus = ctx.bus;
    this.logger = ctx.logger;
    this._defaultsReady = false;
  }

  // ---------- 种子 ----------
  // 幂等：仅当提供商/角色路由缺失时写入，不覆盖用户已改的配置。
  ensureDefaults() {
    if (this._defaultsReady) return;
    const ts = now();
    try {
      for (const p of [DEFAULT_PROVIDER]) {
        const existing = this.db.get('SELECT id FROM model_providers WHERE id = ?', [p.id]);
        if (!existing) {
          this.db.run(
            'INSERT INTO model_providers(id, name, kind, base_url, api_key, enabled, is_default, extra_headers, config, last_check_status, last_check_message, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)',
            [p.id, p.name, p.kind, p.baseUrl, p.apiKey, 1, 1, JSON.stringify(p.extraHeaders || {}), JSON.stringify(p.config || {}), 'never', '', ts, ts]
          );
        }
      }
      for (const r of DEFAULT_ROUTES) {
        const existing = this.db.get('SELECT role FROM model_routes WHERE role = ?', [r.role]);
        if (!existing) {
          this.db.run('INSERT INTO model_routes(role, provider_id, model, dim, params, updated_at) VALUES (?,?,?,?,?,?)',
            [r.role, r.providerId, r.model, r.dim, '{}', ts]);
        }
      }
    } catch (err) {
      this.logger && this.logger.warn('模型接入种子写入失败', { error: String(err && err.message) });
    }
    this._defaultsReady = true;
  }

  // ---------- 提供商 ----------
  rawProviders() {
    this.ensureDefaults();
    return this.db.all('SELECT * FROM model_providers ORDER BY is_default DESC, name');
  }

  // 对外视图：apiKey 一律打码，绝不返回明文
  providers() {
    return this.rawProviders().map((p) => this.publicProvider(p));
  }

  publicProvider(p) {
    const cfg = safeJson(p.config, {});
    return {
      id: p.id,
      name: p.name,
      kind: p.kind,
      baseUrl: p.base_url,
      apiKey: maskKey(p.api_key),
      apiKeyMasked: maskKey(p.api_key),
      hasKey: Boolean(p.api_key),
      enabled: Boolean(p.enabled),
      isDefault: Boolean(p.is_default),
      extraHeaders: safeJson(p.extra_headers, {}),
      config: cfg,
      lastCheckAt: p.last_check_at,
      lastCheckStatus: p.last_check_status,
      lastCheckMessage: p.last_check_message,
      createdAt: p.created_at,
      updatedAt: p.updated_at
    };
  }

  getProviderRow(id) {
    this.ensureDefaults();
    if (id) return this.db.get('SELECT * FROM model_providers WHERE id = ?', [id]) || null;
    return this.db.get('SELECT * FROM model_providers WHERE is_default = 1 LIMIT 1')
      || this.db.get('SELECT * FROM model_providers ORDER BY created_at LIMIT 1') || null;
  }

  // apiKey 为空/未提供时保留原密钥；只有显式传入才覆盖（避免 UI 回传打码串把密钥冲掉）
  saveProvider(payload) {
    this.ensureDefaults();
    const p = payload || {};
    const ts = now();
    const id = p.id || ('prov_' + Date.now().toString(36));
    const existing = this.db.get('SELECT * FROM model_providers WHERE id = ?', [id]);
    const name = String(p.name || (existing && existing.name) || id).trim() || id;
    const baseUrl = String(p.baseUrl || (existing && existing.base_url) || '').trim().replace(/\/+$/, '');
    if (!baseUrl) throw new Error('baseUrl 不能为空');
    const kind = String(p.kind || (existing && existing.kind) || 'openai-compatible');
    const enabled = p.enabled === undefined ? (existing ? existing.enabled : 1) : (p.enabled ? 1 : 0);
    const extraHeaders = p.extraHeaders !== undefined
      ? JSON.stringify(p.extraHeaders || {})
      : (existing ? existing.extra_headers : '{}');
    const config = p.config !== undefined
      ? JSON.stringify(p.config || {})
      : (existing ? existing.config : '{}');
    const incomingKey = p.apiKey === undefined ? undefined : String(p.apiKey || '');
    // 打码串原样回传时视为「不修改」
    const keyIsMasked = incomingKey !== undefined && (incomingKey === '' || incomingKey.includes('••••'));
    let apiKey;
    if (incomingKey === undefined || keyIsMasked) apiKey = existing ? existing.api_key : '';
    else apiKey = incomingKey;

    if (existing) {
      this.db.run('UPDATE model_providers SET name=?, kind=?, base_url=?, api_key=?, enabled=?, extra_headers=?, config=?, updated_at=? WHERE id=?',
        [name, kind, baseUrl, apiKey, enabled, extraHeaders, config, ts, id]);
    } else {
      this.db.run('INSERT INTO model_providers(id, name, kind, base_url, api_key, enabled, is_default, extra_headers, config, last_check_status, last_check_message, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)',
        [id, name, kind, baseUrl, apiKey, enabled, 0, extraHeaders, config, 'never', '', ts, ts]);
    }
    this.bus && this.bus.info('models', '已保存模型提供商：' + name, { providerId: id });
    return this.providers();
  }

  removeProvider(id) {
    this.ensureDefaults();
    this.db.run('DELETE FROM model_providers WHERE id = ?', [id]);
    // 路由指向被删提供商时回落到默认提供商，避免角色悬空
    const def = this.getProviderRow(null);
    if (def) this.db.run('UPDATE model_routes SET provider_id = ?, updated_at = ? WHERE provider_id = ?', [def.id, now(), id]);
    this.bus && this.bus.warn('models', '已删除模型提供商：' + id, { providerId: id });
    return { ok: true };
  }

  // ---------- 角色路由 ----------
  routes() {
    this.ensureDefaults();
    const rows = this.db.all('SELECT * FROM model_routes');
    const byRole = new Map(rows.map((r) => [r.role, r]));
    const routes = MODEL_ROLES.map((role) => {
      const r = byRole.get(role);
      return {
        role,
        providerId: r ? r.provider_id : '',
        model: r ? r.model : '',
        dim: r ? r.dim : null,
        params: r ? safeJson(r.params, {}) : {},
        updatedAt: r ? r.updated_at : null
      };
    });
    const providers = this.providers();
    return {
      roles: MODEL_ROLES.slice(),
      routes,
      providers: providers.map((p) => ({ id: p.id, name: p.name, enabled: p.enabled, isDefault: p.isDefault })),
      models: {} // 可选模型清单由 models.testProvider 拉取后缓存，避免此处阻塞
    };
  }

  getRoute(role) {
    this.ensureDefaults();
    const r = this.db.get('SELECT * FROM model_routes WHERE role = ?', [role]);
    if (!r) return null;
    const provider = this.db.get('SELECT * FROM model_providers WHERE id = ?', [r.provider_id]);
    return { role, providerId: r.provider_id, model: r.model, dim: r.dim, params: safeJson(r.params, {}), provider };
  }

  setRoute(payload) {
    this.ensureDefaults();
    const p = payload || {};
    const role = String(p.role || '');
    if (!MODEL_ROLES.includes(role)) throw new Error('未知模型角色：' + role);
    const providerId = String(p.providerId || '');
    const provider = this.db.get('SELECT id FROM model_providers WHERE id = ?', [providerId]);
    if (!provider) throw new Error('提供商不存在：' + providerId);
    const model = String(p.model || '').trim();
    if (!model) throw new Error('model 不能为空');
    this.db.run('INSERT INTO model_routes(role, provider_id, model, dim, params, updated_at) VALUES (?,?,?,?,?,?) ON CONFLICT(role) DO UPDATE SET provider_id=excluded.provider_id, model=excluded.model, dim=excluded.dim, params=excluded.params, updated_at=excluded.updated_at',
      [role, providerId, model, p.dim === undefined ? null : p.dim, JSON.stringify(p.params || {}), now()]);
    this.bus && this.bus.info('models', '已更新模型角色：' + role + ' → ' + model, { role, providerId, model });
    return this.routes();
  }

  // ---------- 审计 ----------
  recordCall(entry) {
    try {
      this.db.run('INSERT INTO model_calls(ts, provider_id, role, model, action, latency_ms, ok, tokens, error) VALUES (?,?,?,?,?,?,?,?,?)',
        [now(), entry.providerId || null, entry.role || null, entry.model || null, entry.action || 'unknown',
          entry.latencyMs || 0, entry.ok ? 1 : 0, entry.tokens || 0, String(entry.error || '').slice(0, 500)]);
    } catch { /* 统计失败不得影响主流程 */ }
  }

  usage(options) {
    const opts = options || {};
    const limit = Math.min(Number(opts.limit) || 50, 500);
    const calls = this.db.get('SELECT COUNT(*) c FROM model_calls').c;
    const okCalls = this.db.get("SELECT COUNT(*) c FROM model_calls WHERE ok = 1").c;
    const byRole = this.db.all('SELECT role, COUNT(*) c, SUM(ok) ok, AVG(latency_ms) avgLatency FROM model_calls GROUP BY role');
    const byAction = this.db.all('SELECT action, COUNT(*) c FROM model_calls GROUP BY action');
    const recent = this.db.all('SELECT * FROM model_calls ORDER BY ts DESC LIMIT ?', [limit]).map((r) => ({
      id: r.id, ts: r.ts, providerId: r.provider_id, role: r.role, model: r.model, action: r.action,
      latencyMs: r.latency_ms, ok: Boolean(r.ok), tokens: r.tokens, error: r.error
    }));
    return { calls, okCalls, failedCalls: calls - okCalls, byRole, byAction, recent };
  }

  health() {
    this.ensureDefaults();
    const providers = this.rawProviders().map((p) => ({
      id: p.id, name: p.name, enabled: Boolean(p.enabled), hasKey: Boolean(p.api_key),
      lastCheckAt: p.last_check_at, lastCheckStatus: p.last_check_status, lastCheckMessage: p.last_check_message
    }));
    const routes = this.routes().routes;
    const usable = providers.some((p) => p.enabled && p.hasKey);
    return {
      providers,
      routes,
      // 云端不可用时检索/精排/判定一律走本地算法，因此本地兜底恒为可用
      localFallback: true,
      cloudUsable: usable
    };
  }

  // ---------- 评测与读取 ----------
  async listModels(id) {
    const provider = this.getProviderRow(id);
    if (!provider) return { ok: false, latencyMs: 0, message: '提供商不存在', models: [] };
    const started = Date.now();
    try {
      const res = await this.cloudRequest(provider, '/models', { method: 'GET' }, this.timeoutOf(provider));
      if (!res.ok) throw new Error('HTTP ' + res.status + ' ' + shortText(res.text));
      const data = JSON.parse(res.text);
      const models = (data && Array.isArray(data.data) ? data.data : []).map((m) => m.id).filter(Boolean);
      return { ok: true, latencyMs: Date.now() - started, message: '可用模型 ' + models.length + ' 个', models };
    } catch (err) {
      return { ok: false, latencyMs: Date.now() - started, message: String(err && err.message || err), models: [] };
    }
  }

  async testProvider(id) {
    this.ensureDefaults();
    const provider = this.getProviderRow(id);
    if (!provider) return { ok: false, latencyMs: 0, message: '提供商不存在：' + (id || '(默认)'), models: [] };
    const started = Date.now();
    let result;
    try {
      const res = await this.cloudRequest(provider, '/models', { method: 'GET' }, this.timeoutOf(provider));
      if (!res.ok) throw new Error('HTTP ' + res.status + ' ' + shortText(res.text));
      const data = JSON.parse(res.text);
      const models = (data && Array.isArray(data.data) ? data.data : []).map((m) => m.id).filter(Boolean);
      result = { ok: true, latencyMs: Date.now() - started, message: '连接正常，可用模型 ' + models.length + ' 个', models };
    } catch (err) {
      result = { ok: false, latencyMs: Date.now() - started, message: String(err && err.message || err), models: [] };
    }
    try {
      this.db.run('UPDATE model_providers SET last_check_at=?, last_check_status=?, last_check_message=?, updated_at=? WHERE id=?',
        [now(), result.ok ? 'ok' : 'fail', result.message.slice(0, 300), now(), provider.id]);
    } catch { /* ignore */ }
    return result;
  }

  // ---------- 云端调用基础件 ----------
  timeoutOf(provider) {
    const cfg = safeJson(provider && provider.config, {});
    const t = Number(cfg.timeoutMs);
    return Number.isFinite(t) && t > 0 ? t : DEFAULT_TIMEOUT_MS;
  }

  retriesOf(provider) {
    const cfg = safeJson(provider && provider.config, {});
    const r = Number(cfg.retries);
    return Number.isFinite(r) && r >= 0 ? Math.min(r, 3) : DEFAULT_RETRIES;
  }

  endpoint(provider, suffix) {
    const base = String(provider.base_url || '').replace(/\/+$/, '');
    return base + (suffix.startsWith('/') ? suffix : '/' + suffix);
  }

  async cloudRequest(provider, suffix, init, timeoutMs) {
    if (!provider) throw new Error('没有可用的模型提供商');
    if (!provider.api_key) throw new Error('提供商未配置 API Key');
    if (!provider.enabled) throw new Error('提供商已停用');
    const url = this.endpoint(provider, suffix);
    const headers = { ...safeJson(provider.extra_headers, {}), authorization: 'Bearer ' + provider.api_key };
    if (init && init.body) headers['content-type'] = 'application/json';
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), timeoutMs || DEFAULT_TIMEOUT_MS);
    try {
      const res = await withTimeout(
        fetch(url, { ...(init || {}), headers: { ...headers, ...((init && init.headers) || {}) }, signal: ctrl.signal }),
        (timeoutMs || DEFAULT_TIMEOUT_MS) + 1000,
        '模型请求'
      );
      const text = await res.text();
      return { ok: res.ok, status: res.status, text };
    } finally {
      clearTimeout(timer);
    }
  }

  // 统一的「云端优先 + 重试 1 次 + 静默降级」执行器。
  // 任何异常都被吞掉并通过 fallback 返回值兜底，调用方永远拿到可用结果。
  async runWithFallback(role, action, cloudFn, fallbackFn, meta) {
    const route = this.getRoute(role);
    const provider = route ? route.provider : null;
    const started = Date.now();
    let lastError = '';
    const attempts = provider ? this.retriesOf(provider) + 1 : 0;
    for (let i = 0; i < attempts; i++) {
      try {
        const out = await cloudFn(provider, route);
        this.recordCall({
          providerId: provider.id, role, model: route.model, action,
          latencyMs: Date.now() - started, ok: true, tokens: (out && out.tokens) || 0
        });
        return { ...out, local: false, providerId: provider.id, model: route.model, latencyMs: Date.now() - started };
      } catch (err) {
        lastError = String((err && err.message) || err);
        if (i < attempts - 1) {
          this.logger && this.logger.warn('模型调用失败，重试 1 次', { role, action, error: lastError });
        }
      }
    }
    // 降级：本地算法
    const localOut = await fallbackFn();
    this.recordCall({
      providerId: provider ? provider.id : null, role, model: route ? route.model : null, action,
      latencyMs: Date.now() - started, ok: false, error: lastError || '未配置可用提供商'
    });
    if (lastError) this.logger && this.logger.warn('模型调用降级到本地算法', { role, action, error: lastError });
    return { ...localOut, local: true, providerId: provider ? provider.id : null, model: route ? route.model : null, latencyMs: Date.now() - started, fallbackReason: lastError || '未配置可用提供商', ...(meta || {}) };
  }

  // ---------- 四种角色 ----------
  // embedding：返回 4096 维向量（默认路由），云端失败时本地哈希向量兜底
  async embed(texts, options) {
    const opts = options || {};
    const input = Array.isArray(texts) ? texts.map((t) => String(t)) : [String(texts == null ? '' : texts)];
    const out = await this.runWithFallback('embedding', 'embeddings', async (provider, route) => {
      const body = JSON.stringify({ model: route.model, input, encoding_format: 'float' });
      const res = await this.cloudRequest(provider, '/embeddings', { method: 'POST', body }, this.timeoutOf(provider));
      if (!res.ok) throw new Error('HTTP ' + res.status + ' ' + shortText(res.text));
      const data = JSON.parse(res.text);
      const rows = Array.isArray(data && data.data) ? data.data : [];
      if (!rows.length) throw new Error('embeddings 返回为空');
      const vectors = rows.map((r) => (Array.isArray(r.embedding) ? r.embedding : []));
      const dim = vectors[0].length;
      return { vectors, dim, tokens: (data.usage && data.usage.total_tokens) || 0 };
    }, async () => {
      const dim = (this.getRoute('embedding') && this.getRoute('embedding').dim) || 4096;
      const vectors = input.map((t) => localEmbed(t, dim));
      return { vectors, dim };
    });
    // 维度兜底：即使云端返回的维度和路由登记不一致，也以真实返回为准并回写
    if (out && out.dim && !out.local) {
      try {
        const route = this.getRoute('embedding');
        if (route && route.dim !== out.dim) this.db.run('UPDATE model_routes SET dim = ? WHERE role = ?', [out.dim, 'embedding']);
      } catch { /* ignore */ }
    }
    return { vectors: out.vectors, dim: out.dim, local: out.local, providerId: out.providerId, model: out.model, latencyMs: out.latencyMs, fallbackReason: out.fallbackReason };
  }

  async embedOne(text, options) {
    const res = await this.embed([text], options);
    return { vector: res.vectors[0] || [], dim: res.dim, local: res.local, model: res.model, providerId: res.providerId, latencyMs: res.latencyMs };
  }

  // rerank：返回按分数降序的 [{index, document, score}]
  async rerank(query, documents, options) {
    const docs = Array.isArray(documents) ? documents.map((d) => String(d)) : [];
    const out = await this.runWithFallback('rerank', 'rerank', async (provider, route) => {
      const body = JSON.stringify({ model: route.model, query: String(query || ''), documents: docs, top_n: docs.length || undefined });
      const res = await this.cloudRequest(provider, '/rerank', { method: 'POST', body }, this.timeoutOf(provider));
      if (!res.ok) throw new Error('HTTP ' + res.status + ' ' + shortText(res.text));
      const data = JSON.parse(res.text);
      const rows = Array.isArray(data && data.results) ? data.results : [];
      if (!rows.length) throw new Error('rerank 返回为空');
      const items = rows.map((r) => ({
        index: Number(r.index),
        document: r.document == null ? docs[Number(r.index)] : r.document,
        score: Number(r.relevance_score != null ? r.relevance_score : r.score) || 0
      })).sort((a, b) => b.score - a.score);
      return { items, tokens: (data.meta && data.meta.tokens && data.meta.tokens.input_tokens) || 0 };
    }, async () => ({ items: localRerank(query, docs) }));
    return { items: out.items, local: out.local, providerId: out.providerId, model: out.model, latencyMs: out.latencyMs, fallbackReason: out.fallbackReason };
  }

  // judge：返回判定文本；云端不可用时退回规则引擎（本地确定性判定）
  async judge(payload, options) {
    const opts = options || {};
    const p = typeof payload === 'string' ? { prompt: payload } : (payload || {});
    const messages = p.messages || [
      { role: 'system', content: p.system || '你是能力核心的判定层。请只给出简洁、可执行的判定结论。' },
      { role: 'user', content: String(p.prompt || '') }
    ];
    const out = await this.runWithFallback('judge', 'chat', async (provider, route) => {
      const body = JSON.stringify({ model: route.model, messages, temperature: opts.temperature === undefined ? 0.2 : opts.temperature, max_tokens: opts.maxTokens || 512 });
      const res = await this.cloudRequest(provider, '/chat/completions', { method: 'POST', body }, this.timeoutOf(provider));
      if (!res.ok) throw new Error('HTTP ' + res.status + ' ' + shortText(res.text));
      const data = JSON.parse(res.text);
      const text = (data && data.choices && data.choices[0] && data.choices[0].message && data.choices[0].message.content) || '';
      if (!String(text).trim()) throw new Error('judge 返回为空');
      return { text: String(text), tokens: (data.usage && data.usage.total_tokens) || 0 };
    }, async () => ({ text: ruleJudge(p) }));
    return { text: out.text, local: out.local, providerId: out.providerId, model: out.model, latencyMs: out.latencyMs, fallbackReason: out.fallbackReason };
  }

  // general：通用对话/替代模型
  async chat(messages, options) {
    const opts = options || {};
    const out = await this.runWithFallback('general', 'chat', async (provider, route) => {
      const body = JSON.stringify({ model: route.model, messages: messages || [], temperature: opts.temperature === undefined ? 0.3 : opts.temperature, max_tokens: opts.maxTokens || 1024 });
      const res = await this.cloudRequest(provider, '/chat/completions', { method: 'POST', body }, this.timeoutOf(provider));
      if (!res.ok) throw new Error('HTTP ' + res.status + ' ' + shortText(res.text));
      const data = JSON.parse(res.text);
      const text = (data && data.choices && data.choices[0] && data.choices[0].message && data.choices[0].message.content) || '';
      return { text: String(text), tokens: (data.usage && data.usage.total_tokens) || 0 };
    }, async () => ({ text: '（本地兜底）通用模型当前不可用，请检查「模型接入」页的提供商状态。' }));
    return { text: out.text, local: out.local, providerId: out.providerId, model: out.model, latencyMs: out.latencyMs, fallbackReason: out.fallbackReason };
  }

  // ---------- 向量缓存与语义检索 ----------
  encodeVector(vec) {
    const arr = Float32Array.from(vec || []);
    return Buffer.from(arr.buffer, arr.byteOffset, arr.byteLength);
  }

  decodeVector(blob) {
    if (!blob) return [];
    const buf = Buffer.from(blob);
    const arr = new Float32Array(buf.buffer, buf.byteOffset, Math.floor(buf.byteLength / 4));
    return Array.from(arr);
  }

  async indexVectors(capabilityIds, options) {
    const opts = options || {};
    const ids = Array.isArray(capabilityIds) ? capabilityIds : [];
    const rows = ids.length
      ? this.db.all('SELECT id, display_name, summary FROM capabilities WHERE id IN (' + ids.map(() => '?').join(',') + ')', ids)
      : this.db.all("SELECT id, display_name, summary FROM capabilities WHERE scope = 'own' LIMIT ?", [opts.limit || 200]);
    let indexed = 0;
    for (const row of rows) {
      try {
        const text = row.display_name + '\n' + (row.summary || '');
        const res = await this.embedOne(text);
        if (!res.vector || !res.vector.length) continue;
        this.db.run('INSERT INTO asset_vectors(capability_id, provider_id, model, dim, vector, source_hash, updated_at) VALUES (?,?,?,?,?,?,?) ON CONFLICT(capability_id) DO UPDATE SET provider_id=excluded.provider_id, model=excluded.model, dim=excluded.dim, vector=excluded.vector, source_hash=excluded.source_hash, updated_at=excluded.updated_at',
          [row.id, res.providerId || '', res.model || '', res.dim, this.encodeVector(res.vector), String(text).length + ':' + row.id, now()]);
        indexed++;
      } catch { /* 单条失败不影响批量 */ }
    }
    return { indexed, total: rows.length };
  }

  async vectorSearch(query, options) {
    const opts = options || {};
    const limit = Math.min(Number(opts.limit) || 10, 100);
    const res = await this.embedOne(String(query || ''));
    const qv = res.vector || [];
    if (!qv.length) return { items: [], local: res.local, note: '查询向量为空' };
    const rows = this.db.all('SELECT capability_id, dim, vector FROM asset_vectors');
    const scored = [];
    for (const r of rows) {
      const v = this.decodeVector(r.vector);
      if (v.length !== qv.length) continue;
      let dot = 0;
      for (let i = 0; i < v.length; i++) dot += v[i] * qv[i];
      scored.push({ capabilityId: r.capability_id, score: dot });
    }
    scored.sort((a, b) => b.score - a.score);
    const top = scored.slice(0, limit);
    const items = top.map((t) => {
      const cap = this.db.get('SELECT id, kind, display_name, summary, scope FROM capabilities WHERE id = ?', [t.capabilityId]);
      return cap ? { id: cap.id, kind: cap.kind, displayName: cap.display_name, summary: cap.summary, scope: cap.scope, score: Number(t.score.toFixed(6)) } : null;
    }).filter(Boolean);
    return { items, local: res.local, model: res.model, providerId: res.providerId };
  }
}

// 本地判定规则：把结构化输入压缩成一条可执行结论
function ruleJudge(p) {
  const kind = p.kind || 'generic';
  if (kind === 'duplicate') {
    const sim = Number(p.similarity || 0);
    const verdict = sim >= 0.9 ? 'merge' : sim >= 0.7 ? 'keep-latest' : 'coexist';
    return '判定：' + verdict + '（本地规则引擎，相似度 ' + sim.toFixed(3) + '；云端判定不可用时的确定性兜底）';
  }
  if (kind === 'fault') {
    return '判定：' + (p.autoFixable ? '可自动修复，建议生成补丁并校验路径边界' : '需人工介入，记录证据后派发工单') + '（本地规则引擎兜底）';
  }
  const q = String(p.prompt || '').trim();
  return q ? '本地规则引擎无法完成语义判定；已记录请求（' + q.length + ' 字），请检查模型接入配置。' : '本地规则引擎：空请求。';
}

function safeJson(text, fallback) {
  if (!text) return fallback;
  try { return JSON.parse(text); } catch { return fallback; }
}

function shortText(t) {
  return String(t || '').replace(/\s+/g, ' ').slice(0, 200);
}
