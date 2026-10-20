import http from 'node:http';
import { URL } from 'node:url';
import crypto from 'node:crypto';
import { McpServerCore } from '../mcp/server-core.js';
import { now, jsonParse, newId } from '../core/util.js';
import { hostLabel } from '../services/search.js';
import { PRODUCT_NAME, PRODUCT_LABEL } from '../core/identity.js';

// 中枢 HTTP 服务：
// - /api/v1/*   REST（界面与外部脚本）
// - /mcp        MCP Streamable HTTP（智能体接入）
// - /health     健康探针
// 安全边界：默认只监听 127.0.0.1；请求体大小、并发与超时受限。
export function createHttpServer(ctx, services, options) {
  const opts = options || {};
  const core = new McpServerCore(ctx, services);
  const sessions = new Map();     // mcp sessionId -> auth
  let active = 0;
  const startedAt = now();

  const server = http.createServer(async (req, res) => {
    const url = new URL(req.url || '/', 'http://127.0.0.1');
    const ip = req.socket.remoteAddress || '';
    try {
      if (req.method === 'OPTIONS') return send(res, 204, null);
      if (url.pathname === '/health') {
        return sendJson(res, 200, { ok: true, name: PRODUCT_NAME, title: PRODUCT_LABEL, version: '1.0.0', uptimeMs: now() - startedAt });
      }
      if (url.pathname === '/mcp' || url.pathname.startsWith('/mcp')) {
        return await handleMcp(req, res, core, sessions, ip);
      }
      if (url.pathname.startsWith('/api/')) {
        if (active > 32) return sendJson(res, 503, { error: '服务繁忙' });
        active++;
        try { return await handleRest(req, res, url, services, ctx); }
        finally { active--; }
      }
      sendJson(res, 404, { error: 'Not Found', path: url.pathname });
    } catch (err) {
      ctx.logger.error('HTTP 请求失败', { path: url.pathname, error: String(err && err.message) });
      if (!res.headersSent) sendJson(res, 500, { error: String(err && err.message || err) });
    }
  });

  server.on('clientError', (err, socket) => { try { socket.destroy(); } catch { /* ignore */ } });

  return {
    server,
    core,
    listen(port, host) {
      return new Promise((resolve, reject) => {
        server.once('error', reject);
        server.listen(port, host || '127.0.0.1', () => resolve(server.address()));
      });
    },
    close() { return new Promise((r) => server.close(() => r())); },
    sessions
  };
}

async function handleMcp(req, res, core, sessions, ip) {
  if (req.method !== 'POST') return sendJson(res, 405, { error: '仅支持 POST' });
  const body = await readBody(req, 4 * 1024 * 1024);
  let msg;
  try { msg = JSON.parse(body); } catch { return sendJson(res, 400, rpcErr(null, -32700, 'Parse error')); }
  const sessionId = req.headers['mcp-session-id'] || '';
  const auth = sessionId && sessions.has(sessionId) ? sessions.get(sessionId) : { ip, agentId: null, sessionId: null };
  const out = Array.isArray(msg)
    ? (await Promise.all(msg.map((m) => core.handle(m, auth)))).filter(Boolean)
    : await core.handle(msg, auth);
  if (auth.sessionId && !sessions.has(auth.sessionId)) sessions.set(auth.sessionId, auth);
  if (!out) return send(res, 202, '');
  res.setHeader('mcp-session-id', auth.sessionId || '');
  return sendJson(res, 200, out);
}

async function handleRest(req, res, url, services, ctx) {
  const p = url.pathname.replace(/^\/api\/v1/, '');
  const method = req.method;
  const q = Object.fromEntries(url.searchParams.entries());
  const body = ['POST', 'PUT', 'PATCH'].includes(method) ? await readJson(req) : {};

  // ---- 只读 ----
  if (method === 'GET' && p === '/status') {
    return sendJson(res, 200, {
      ok: true, version: '1.0.0', startedAt: now(),
      stats: services.registry.stats(),
      db: ctx.db.stats(),
      lastScan: ctx.db.get('SELECT MAX(last_scan_at) AS t FROM sources').t
    });
  }
  if (method === 'GET' && p === '/capabilities') {
    return sendJson(res, 200, services.registry.listCapabilities({
      kind: q.kind, status: q.status, health: q.health, host: q.host, q: q.q,
      limit: q.limit ? Number(q.limit) : 200, offset: q.offset ? Number(q.offset) : 0, sort: q.sort
    }));
  }
  if (method === 'GET' && p.startsWith('/capabilities/')) {
    const id = p.split('/')[2];
    const cap = services.registry.getCapability(id);
    if (!cap) return sendJson(res, 404, { error: 'not found' });
    return sendJson(res, 200, cap);
  }
  if (method === 'GET' && p === '/search') {
    return sendJson(res, 200, services.search.search(q.q || '', { kind: q.kind, host: q.host, limit: q.limit ? Number(q.limit) : 20 }));
  }
  if (method === 'GET' && p === '/sources') return sendJson(res, 200, services.scanner.listSources());
  if (method === 'GET' && p === '/agents') return sendJson(res, 200, services.agents.list());
  if (method === 'GET' && p === '/agents/sessions') return sendJson(res, 200, services.agents.sessions());
  if (method === 'GET' && p === '/faults') return sendJson(res, 200, services.faults.list({ status: q.status, severity: q.severity }));
  if (method === 'GET' && p === '/faults/stats') return sendJson(res, 200, services.faults.stats());
  if (method === 'GET' && p === '/repairs') return sendJson(res, 200, services.faults.repairs(q.faultId));
  if (method === 'GET' && p === '/duplicates') return sendJson(res, 200, services.dedupe.list({ decision: q.decision || 'pending' }));
  if (method === 'GET' && p === '/updates') return sendJson(res, 200, services.updates.list({ status: q.status }));
  if (method === 'GET' && p === '/events') return sendJson(res, 200, ctx.bus.recent(Number(q.limit || 200)));
  if (method === 'GET' && p === '/workflows') return sendJson(res, 200, services.workflows.list());
  if (method === 'GET' && p === '/workflows/runs') return sendJson(res, 200, services.workflows.runs(Number(q.limit || 50)));
  if (method === 'GET' && p === '/assets') return sendJson(res, 200, services.assets.list({ kind: q.kind, query: q.query, limit: q.limit ? Number(q.limit) : 200 }));
  if (method === 'GET' && p === '/assets/stats') return sendJson(res, 200, services.assets.stats());
  if (method === 'GET' && p.startsWith('/assets/')) return sendJson(res, 200, services.assets.get(p.split('/')[2]));
  if (method === 'GET' && p === '/models/providers') return sendJson(res, 200, services.models.providers());
  if (method === 'GET' && p === '/models/routes') return sendJson(res, 200, services.models.routes());
  if (method === 'GET' && p === '/models/usage') return sendJson(res, 200, services.models.usage({ limit: q.limit ? Number(q.limit) : 50 }));
  if (method === 'GET' && p === '/models/health') return sendJson(res, 200, services.models.health());
  if (method === 'POST' && p === '/models/test') return sendJson(res, 200, await services.models.testProvider(body.id));
  if (method === 'GET' && p === '/ecosystem/targets') return sendJson(res, 200, services.ecosystem.targets());
  if (method === 'GET' && p === '/ecosystem/backups') return sendJson(res, 200, services.ecosystem.backups());

  // ---- 写操作 ----
  if (method === 'POST' && p === '/scan') return sendJson(res, 200, await services.scanner.scanAll({ sourceId: body.sourceId }));
  if (method === 'POST' && p === '/search') return sendJson(res, 200, services.search.search(body.q || '', body));
  if (method === 'POST' && p === '/dedupe/scan') return sendJson(res, 200, services.dedupe.scan());
  if (method === 'POST' && p.startsWith('/duplicates/') && p.endsWith('/decide')) {
    const id = p.split('/')[2];
    return sendJson(res, 200, services.dedupe.decide(id, body.action, body.chosenId));
  }
  if (method === 'POST' && p === '/faults/diagnose') return sendJson(res, 200, services.faults.diagnose());
  if (method === 'POST' && p.startsWith('/faults/') && p.endsWith('/autofix')) {
    return sendJson(res, 200, await services.faults.autoFix(p.split('/')[2]));
  }
  if (method === 'POST' && p.startsWith('/faults/') && p.endsWith('/dispatch')) {
    return sendJson(res, 200, services.faults.dispatch(p.split('/')[2], body.agentId, body.note));
  }
  if (method === 'POST' && p === '/updates/check') return sendJson(res, 200, await services.updates.checkAll(body));
  if (method === 'POST' && p === '/agents/scan') return sendJson(res, 200, await services.agents.scan());
  if (method === 'POST' && p === '/agents/probe') return sendJson(res, 200, await services.agents.probe(body.host));
  if (method === 'POST' && p === '/agents/connect') return sendJson(res, 200, services.agents.connect(body.host, body));
  if (method === 'POST' && p === '/assets/promote') return sendJson(res, 200, services.assets.promote(body));
  if (method === 'POST' && p === '/assets/create') return sendJson(res, 200, services.assets.create(body));
  if (method === 'POST' && p === '/assets/remove') return sendJson(res, 200, services.assets.remove(body));
  if (method === 'POST' && p === '/models/saveProvider') return sendJson(res, 200, services.models.saveProvider(body));
  if (method === 'POST' && p === '/models/removeProvider') return sendJson(res, 200, services.models.removeProvider(body.id));
  if (method === 'POST' && p === '/models/setRoute') return sendJson(res, 200, services.models.setRoute(body));
  if (method === 'POST' && p === '/workflows/run') return sendJson(res, 200, await services.workflows.run(body.id, body.trigger || 'manual'));
  if (method === 'POST' && p === '/ecosystem/apply') return sendJson(res, 200, services.ecosystem.applyMcp(body.host, body));
  if (method === 'POST' && p === '/ecosystem/preview') return sendJson(res, 200, services.ecosystem.previewMcp(body.host, body));
  if (method === 'POST' && p === '/capabilities/update') {
    const { id, action, value } = body;
    return sendJson(res, 200, services.lifecycle.action(id, action, value));
  }
  if (method === 'POST' && p === '/capabilities/import') return sendJson(res, 200, services.imports.importPath(body.path, body));

  return sendJson(res, 404, { error: '未知接口', path: p });
}

function readBody(req, limit) {
  return new Promise((resolve, reject) => {
    let size = 0; const chunks = [];
    req.on('data', (c) => {
      size += c.length;
      if (size > limit) { reject(new Error('请求体过大')); req.destroy(); return; }
      chunks.push(c);
    });
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

async function readJson(req) {
  const text = await readBody(req, 8 * 1024 * 1024);
  if (!text) return {};
  try { return JSON.parse(text); } catch { return {}; }
}

function sendJson(res, code, data) {
  const body = JSON.stringify(data);
  res.writeHead(code, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(body),
    'access-control-allow-origin': '*',
    'access-control-allow-headers': 'content-type, mcp-session-id',
    'access-control-expose-headers': 'mcp-session-id'
  });
  res.end(body);
}

function send(res, code, text) {
  res.writeHead(code, { 'content-type': 'text/plain; charset=utf-8', 'access-control-allow-origin': '*' });
  res.end(text);
}

function rpcErr(id, code, message) {
  return { jsonrpc: '2.0', id, error: { code, message } };
}
