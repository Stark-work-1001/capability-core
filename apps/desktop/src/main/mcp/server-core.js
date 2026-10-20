import { newId, now, jsonParse, truncate, oneLine } from '../core/util.js';
import { hostLabel } from '../services/search.js';
import { PRODUCT_NAME, PRODUCT_LABEL, PROTOCOL_VERSION } from '../core/identity.js';

// MCP 服务端实现：stdio（JSON-RPC 行分隔）与 Streamable HTTP（POST 单请求）共用同一套工具定义。
// 工具设计目标：让智能体能「主动在内部找能力」——搜索、读取全文、读取安装信息、上报使用、提交补丁。
export const CAPABILITY_TOOLS = [
  {
    name: 'capability.search',
    title: '检索能力',
    description: '在能力中枢中检索可用的技能 / MCP / 插件。返回匹配项、分数、作者、所在智能体与摘要。',
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string', description: '自然语言或关键词，中英文均可' },
        kind: { type: 'string', enum: ['skill', 'mcp', 'plugin', 'prompt', 'agent'], description: '限定能力类型' },
        host: { type: 'string', description: '限定来源智能体，例如 dsh / codex / claude' },
        limit: { type: 'number', description: '返回条数，默认 10' }
      },
      required: ['query']
    }
  },
  {
    name: 'capability.list',
    title: '列出能力',
    description: '按类型/状态分页列出中枢能力，用于浏览而非检索。',
    inputSchema: {
      type: 'object',
      properties: {
        kind: { type: 'string' }, status: { type: 'string' }, limit: { type: 'number' }, offset: { type: 'number' }
      }
    }
  },
  {
    name: 'capability.get',
    title: '读取能力详情',
    description: '读取单个能力的完整信息：清单、安装路径、正文内容、健康状态、更新与故障。',
    inputSchema: { type: 'object', properties: { id: { type: 'string' } }, required: ['id'] }
  },
  {
    name: 'capability.read_skill',
    title: '读取技能正文',
    description: '读取技能（skill）或提示词的正文全文，用于按需加载执行指令。',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' }, maxChars: { type: 'number' } },
      required: ['id']
    }
  },
  {
    name: 'capability.report_usage',
    title: '上报能力使用',
    description: '智能体调用能力后上报一次使用记录，用于中枢统计热度与检索排序。',
    inputSchema: {
      type: 'object',
      properties: { capabilityId: { type: 'string' }, action: { type: 'string' }, detail: { type: 'object' } },
      required: ['capabilityId']
    }
  },
  {
    name: 'capability.list_faults',
    title: '查询故障与维修任务',
    description: '列出中枢识别到的能力故障，可据此修复并回传补丁。',
    inputSchema: { type: 'object', properties: { status: { type: 'string' }, severity: { type: 'string' } } }
  },
  {
    name: 'capability.submit_patch',
    title: '提交维修补丁',
    description: '提交针对某个故障的完整文件内容。中枢校验路径边界并备份后写入，记录到维修历史。',
    inputSchema: {
      type: 'object',
      properties: {
        faultId: { type: 'string' }, file: { type: 'string' }, content: { type: 'string' }, message: { type: 'string' }
      },
      required: ['faultId', 'file', 'content']
    }
  },
  {
    name: 'capability.register_agent',
    title: '登记智能体',
    description: '智能体启动时登记自身（宿主、名称、版本），中枢据此更新在线状态。',
    inputSchema: {
      type: 'object',
      properties: { host: { type: 'string' }, name: { type: 'string' }, version: { type: 'string' }, capabilities: { type: 'array', items: { type: 'string' } } },
      required: ['host']
    }
  },
  {
    name: 'capability.registry_stats',
    title: '中枢统计',
    description: '返回能力总数、按类型/宿主分布、故障数、重复数、在线智能体数。',
    inputSchema: { type: 'object', properties: {} }
  },
  {
    name: 'capability.sync_status',
    title: '同步状态',
    description: '返回最近一次扫描、诊断、更新检查的时间与结果。',
    inputSchema: { type: 'object', properties: {} }
  },
  {
    name: 'capability.assets_list',
    title: '列出能力资产',
    description: '列出能力资产库（能力核心自有的、可维修可派发的能力实体）。与宿主能力库（扫描登记的外部能力）相互独立。',
    inputSchema: {
      type: 'object',
      properties: {
        kind: { type: 'string', enum: ['skill', 'mcp', 'plugin', 'prompt'], description: '限定资产类型' },
        limit: { type: 'number', description: '返回条数，默认 50' }
      }
    }
  },
  {
    name: 'capability.assets_read',
    title: '读取能力资产',
    description: '读取单个能力资产的完整正文与物理路径。',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string', description: '能力资产 id' } },
      required: ['id']
    }
  },
  {
    name: 'capability.model_status',
    title: '模型接入状态',
    description: '返回模型接入的可用性：提供商连接状态、四种角色（embedding/rerank/judge/general）的路由与最近调用统计。不返回任何密钥。',
    inputSchema: { type: 'object', properties: {} }
  }
];

export class McpServerCore {
  constructor(ctx, services) {
    this.ctx = ctx;
    this.svc = services;
    this.sessions = new Map();
  }

  describeCapability(cap, opts) {
    const o = opts || {};
    return {
      id: cap.id,
      kind: cap.kind,
      name: cap.canonical_name,
      displayName: cap.display_name,
      summary: cap.summary,
      author: cap.author || '未声明',
      version: cap.version || '未声明',
      hosts: jsonParse(cap.hosts, []).map((h) => ({ host: h, label: hostLabel(h) })),
      protocols: jsonParse(cap.protocols, []),
      tags: jsonParse(cap.tags, []),
      status: cap.status,
      health: cap.health,
      score: cap.score,
      snippet: cap.snippet,
      explanation: o.explanation === false ? undefined : cap.explanation,
      path: o.path ? (cap.installations && cap.installations[0] ? cap.installations[0].path : '') : undefined
    };
  }

  async callTool(name, args, auth) {
    const a = args || {};
    switch (name) {
      case 'capability.search': {
        if (!a.query) throw new Error('缺少 query');
        const res = this.svc.search.search(a.query, { kind: a.kind, host: a.host, limit: a.limit || 10 });
        this.recordUsage(a.query, auth, 'search', { query: a.query });
        return {
          content: [{ type: 'text', text: this.renderSearch(res) }],
          structuredContent: { query: res.query, took: res.took, total: res.total, items: res.items.map((i) => this.describeCapability(i, {})) }
        };
      }
      case 'capability.list': {
        const rows = this.ctx.db.all(
          'SELECT * FROM capabilities ' + (a.kind || a.status ? 'WHERE ' + [a.kind ? 'kind = ?' : '', a.status ? 'status = ?' : ''].filter(Boolean).join(' AND ') : '') + ' ORDER BY updated_at DESC LIMIT ? OFFSET ?',
          [a.kind, a.status, a.limit || 50, a.offset || 0].filter((v) => v !== undefined));
        const items = rows.map((r) => this.describeCapability(r, {}));
        return { content: [{ type: 'text', text: items.map((i) => '- [' + i.kind + '] ' + i.displayName + ' (v' + i.version + ') — ' + oneLine(i.summary, 120)).join('\n') || '（空）' }], structuredContent: { items } };
      }
      case 'capability.get': {
        const cap = this.svc.registry.getCapability(a.id);
        if (!cap) throw new Error('能力不存在：' + a.id);
        return { content: [{ type: 'text', text: JSON.stringify({ ...cap, description: truncate(cap.description, 4000), installations: cap.installations.map((i) => ({ ...i, content: undefined })) }, null, 2) }], structuredContent: cap };
      }
      case 'capability.read_skill': {
        const cap = this.svc.registry.getCapability(a.id);
        if (!cap) throw new Error('能力不存在：' + a.id);
        const inst = cap.installations.sort((x, y) => y.updatedAt - x.updatedAt)[0];
        const content = inst ? inst.content : '';
        this.recordUsage(cap.id, auth, 'read', { chars: content.length });
        return { content: [{ type: 'text', text: truncate(content || '（无正文）', a.maxChars || 30000) }], structuredContent: { id: cap.id, path: inst ? inst.path : '', length: content.length } };
      }
      case 'capability.report_usage': {
        this.recordUsage(a.capabilityId, auth, a.action || 'use', a.detail || {});
        return { content: [{ type: 'text', text: '已记录使用：' + a.capabilityId }] };
      }
      case 'capability.list_faults': {
        const items = this.svc.faults.list({ status: a.status || 'open', severity: a.severity });
        return {
          content: [{ type: 'text', text: items.length ? items.map((f) => '- [' + f.severity + '] ' + f.title + '\n  工单: ' + f.id + ' · 路径: ' + (f.evidence.path || '')).join('\n') : '没有未处理故障' }],
          structuredContent: { items }
        };
      }
      case 'capability.submit_patch': {
        const res = this.svc.faults.submitPatch({ faultId: a.faultId, file: a.file, content: a.content, message: a.message, agentId: auth && auth.agentId });
        return { content: [{ type: 'text', text: '补丁已应用：' + res.file + '（备份 ' + res.backupId + '）' }], structuredContent: res };
      }
      case 'capability.register_agent': {
        const id = 'mcp:' + (a.host || 'unknown');
        this.ctx.db.run('INSERT INTO agents(id, host, label, kind, status, protocol, first_seen, last_seen, metadata) VALUES (?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET status=\'running\', last_seen=excluded.last_seen, metadata=excluded.metadata',
          [id, a.host || 'unknown', a.name || a.host || 'unknown', 'mcp-session', 'running', 'mcp', now(), now(), JSON.stringify({ version: a.version, capabilities: a.capabilities || [] })]);
        return { content: [{ type: 'text', text: '已登记：' + (a.name || a.host) }], structuredContent: { agentId: id } };
      }
      case 'capability.registry_stats': {
        const stats = this.svc.registry.stats();
        return { content: [{ type: 'text', text: JSON.stringify(stats, null, 2) }], structuredContent: stats };
      }
      case 'capability.sync_status': {
        const sources = this.ctx.db.all('SELECT label, host, last_scan_at, last_scan_status, last_scan_count FROM sources ORDER BY last_scan_at DESC LIMIT 30');
        const faults = this.svc.faults.stats();
        return { content: [{ type: 'text', text: JSON.stringify({ sources, faults }, null, 2) }], structuredContent: { sources, faults } };
      }
      case 'capability.assets_list': {
        const res = this.svc.assets.list({ kind: a.kind, limit: a.limit || 50 });
        const lines = res.items.length
          ? res.items.map((i) => '- [' + i.kind + '] ' + i.displayName + (i.version ? ' v' + i.version : '') + ' — ' + oneLine(i.summary || '', 100) + '\n  id ' + i.id + ' · 路径 ' + i.path)
          : ['（能力资产库为空）'];
        return {
          content: [{ type: 'text', text: '能力资产 ' + res.total + ' 项：\n' + lines.join('\n') }],
          structuredContent: res
        };
      }
      case 'capability.assets_read': {
        if (!a.id) throw new Error('缺少 id');
        const asset = this.svc.assets.get(a.id);
        this.recordUsage(asset.id, auth, 'read', { chars: String(asset.content || '').length });
        return {
          content: [{ type: 'text', text: truncate(asset.content || '（无正文）', 30000) }],
          structuredContent: { id: asset.id, kind: asset.kind, title: asset.title, path: asset.path, length: String(asset.content || '').length }
        };
      }
      case 'capability.model_status': {
        // 只返回可用性，不返回任何密钥；provider 里只有打码串
        const health = this.svc.models.health();
        const usage = this.svc.models.usage({ limit: 10 });
        const payload = { ...health, usage: { calls: usage.calls, okCalls: usage.okCalls, failedCalls: usage.failedCalls, byRole: usage.byRole } };
        return { content: [{ type: 'text', text: JSON.stringify(payload, null, 2) }], structuredContent: payload };
      }
      default:
        throw new Error('未知工具：' + name);
    }
  }

  renderSearch(res) {
    if (!res.items.length) return '没有匹配的能力。可尝试更短的关键词，或使用 capability.list 浏览。';
    const lines = ['检索「' + res.query + '」命中 ' + res.items.length + ' 项（候选 ' + res.total + '，用时 ' + res.took + 'ms）：'];
    res.items.forEach((i, idx) => {
      lines.push((idx + 1) + '. [' + i.kind + '] ' + i.displayName + (i.version ? ' v' + i.version : '') + ' · 作者 ' + (i.author || '未声明'));
      if (i.summary) lines.push('   ' + oneLine(i.summary, 160));
      lines.push('   宿主：' + ((i.hosts || []).map((h) => h.label).join(' / ') || '未知') + ' · 状态 ' + i.status + '/' + i.health + ' · id ' + i.id);
    });
    lines.push('', '下一步：capability.read_skill 读取正文，或 capability.get 查看安装路径。');
    return lines.join('\n');
  }

  recordUsage(capabilityId, auth, action, detail) {
    try {
      this.ctx.db.run('UPDATE capabilities SET usage_count = usage_count + 1, last_used_at = ? WHERE id = ?', [now(), capabilityId]);
      this.ctx.db.run('INSERT INTO capability_usage(capability_id, agent_id, session_id, action, detail, ts) VALUES (?,?,?,?,?,?)',
        [capabilityId, auth ? auth.agentId : null, auth ? auth.sessionId : null, action, JSON.stringify(detail || {}), now()]);
    } catch { /* ignore */ }
  }

  // JSON-RPC 分发（stdio 与 HTTP 共用）
  async handle(msg, auth) {
    if (!msg || typeof msg !== 'object') return rpcError(null, -32600, 'Invalid Request');
    const { id, method, params } = msg;
    const isNotification = id === undefined || id === null;
    try {
      let result;
      switch (method) {
        case 'initialize': {
          const clientInfo = (params && params.clientInfo) || {};
          const sessionId = (auth && auth.sessionId) || newId('ses');
          if (auth) auth.sessionId = sessionId;
          const hostGuess = guessHost(clientInfo.name || '');
          if (hostGuess) {
            this.ctx.db.run('INSERT INTO agents(id, host, label, kind, status, protocol, first_seen, last_seen, metadata) VALUES (?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET status=\'running\', last_seen=excluded.last_seen, metadata=excluded.metadata',
              ['mcp:' + hostGuess, hostGuess, clientInfo.name || hostGuess, 'mcp-session', 'running', 'mcp', now(), now(), JSON.stringify({ clientInfo })]);
          }
          this.svc.agents.registerSession({ id: sessionId, host: hostGuess || 'unknown', client: clientInfo, protocol: 'mcp', ip: auth && auth.ip });
          this.sessions.set(sessionId, { createdAt: now(), client: clientInfo });
          result = {
            protocolVersion: PROTOCOL_VERSION,
            capabilities: { tools: { listChanged: false }, resources: { subscribe: false, listChanged: false }, prompts: { listChanged: false } },
            serverInfo: { name: PRODUCT_NAME, title: PRODUCT_LABEL, version: '1.0.0' },
            instructions: PRODUCT_LABEL + '：调用 capability.search 检索能力，capability.read_skill 读取正文，capability.list_faults 与 capability.submit_patch 参与维修；capability.assets_list 查看能力资产库。'
          };
          break;
        }
        case 'ping': result = {}; break;
        case 'tools/list': result = { tools: CAPABILITY_TOOLS }; break;
        case 'tools/call': {
          const name = params && params.name;
          const args = (params && params.arguments) || {};
          const out = await this.callTool(name, args, auth || {});
          if (auth && auth.sessionId) {
            this.svc.agents.touchSession(auth.sessionId, 1);
            const row = this.ctx.db.get('SELECT tool_calls FROM agent_sessions WHERE id = ?', [auth.sessionId]);
            if (row) {
              const counts = jsonParse(row.tool_calls, {});
              counts[name] = (counts[name] || 0) + 1;
              this.ctx.db.run('UPDATE agent_sessions SET tool_calls = ? WHERE id = ?', [JSON.stringify(counts), auth.sessionId]);
            }
          }
          result = out;
          break;
        }
        case 'resources/list': {
          const caps = this.ctx.db.all("SELECT id, display_name, summary FROM capabilities WHERE status = 'active' LIMIT 200");
          result = { resources: caps.map((c) => ({ uri: 'capability://asset/' + c.id, name: c.display_name, description: c.summary, mimeType: 'text/markdown' })) };
          break;
        }
        case 'resources/read': {
          const uri = params && params.uri || '';
          const id = String(uri).replace('capability://asset/', '').replace('capability://capability/', '');
          const cap = this.svc.registry.getCapability(id);
          if (!cap) throw new Error('资源不存在');
          const inst = cap.installations[0];
          result = { contents: [{ uri, mimeType: 'text/markdown', text: inst ? inst.content : '' }] };
          break;
        }
        case 'prompts/list': {
          result = { prompts: [{ name: 'capability-hub', description: '在能力核心中检索并使用能力的工作流说明' }] };
          break;
        }
        case 'prompts/get': {
          result = { description: '能力中枢使用说明', messages: [{ role: 'user', content: { type: 'text', text: '使用 capability.search 检索能力，capability.read_skill 读取正文。' } }] };
          break;
        }
        default:
          if (isNotification) return null;
          return rpcError(id, -32601, 'Method not found: ' + method);
      }
      if (isNotification) return null;
      return { jsonrpc: '2.0', id, result };
    } catch (err) {
      if (isNotification) return null;
      return rpcError(id, -32000, String(err && err.message || err));
    }
  }
}

function rpcError(id, code, message) {
  return { jsonrpc: '2.0', id: id === undefined ? null : id, error: { code, message } };
}

function guessHost(name) {
  const s = String(name || '').toLowerCase();
  if (s.includes('codex')) return 'codex';
  if (s.includes('claude')) return 'claude';
  if (s.includes('deepseek') || s.includes('dsh')) return 'dsh';
  if (s.includes('minimax') || s.includes('mavis')) return 'minimax';
  if (s.includes('gemini') || s.includes('antigravity')) return 'gemini';
  if (s.includes('cursor')) return 'cursor';
  if (s.includes('workbuddy')) return 'workbuddy';
  return '';
}
