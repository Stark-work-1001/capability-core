import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createContext, closeContext } from '../apps/desktop/src/main/services/context.js';
import { createHttpServer } from '../apps/desktop/src/main/server/http.js';

test('HTTP + MCP：initialize / tools/list / tools/call / REST', async () => {
  const home = path.join(os.tmpdir(), 'cc-http-' + Date.now());
  fs.mkdirSync(path.join(home, 'skills', 'web-skill'), { recursive: true });
  fs.writeFileSync(path.join(home, 'skills', 'web-skill', 'SKILL.md'), '---\nname: web-skill\ndescription: Fetch web pages\nversion: 1.0.0\n---\n\nUse fetch.\n');
  const ctx = createContext({ dbPath: path.join(home, 'test.db'), logDir: null, assetDir: path.join(home, 'assets') });
  ctx.db.run('INSERT INTO sources(id, kind, host, label, root_path, format, enabled, builtin, scan_interval, created_at, updated_at, config) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)',
    ['s1', 'skill', 'dsh', 'test', path.join(home, 'skills'), 'auto', 1, 0, 900, Date.now(), Date.now(), '{"depth":2}']);
  await ctx.services.scanner.scanAll({});
  const srv = createHttpServer(ctx, ctx.services, {});
  const addr = await srv.listen(0, '127.0.0.1');
  const base = 'http://127.0.0.1:' + addr.port;
  const post = async (body, headers) => {
    const r = await fetch(base + '/mcp', { method: 'POST', headers: { 'content-type': 'application/json', ...(headers || {}) }, body: JSON.stringify(body) });
    return { status: r.status, headers: r.headers, body: await r.json() };
  };
  try {
    const init = await post({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { clientInfo: { name: 'codex-test', version: '9' } } });
    assert.equal(init.status, 200);
    assert.equal(init.body.result.serverInfo.name, 'capability-core');
    const sid = init.headers.get('mcp-session-id');
    assert.ok(sid);

    const tools = await post({ jsonrpc: '2.0', id: 2, method: 'tools/list' }, { 'mcp-session-id': sid });
    const names = tools.body.result.tools.map((t) => t.name);
    for (const n of ['capability.search', 'capability.read_skill', 'capability.submit_patch', 'capability.list_faults']) assert.ok(names.includes(n), n);

    const call = await post({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'capability.search', arguments: { query: 'web fetch', limit: 3 } } }, { 'mcp-session-id': sid });
    assert.equal(call.body.result.structuredContent.items.length >= 1, true);
    assert.ok(call.body.result.content[0].text.includes('web-skill'));

    const read = await post({ jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'capability.read_skill', arguments: { id: call.body.result.structuredContent.items[0].id } } }, { 'mcp-session-id': sid });
    assert.ok(read.body.result.content[0].text.includes('Use fetch'));

    const bad = await post({ jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'nope', arguments: {} } }, { 'mcp-session-id': sid });
    assert.ok(bad.body.error);

    const status = await fetch(base + '/api/v1/status');
    const statusJson = await status.json();
    assert.equal(statusJson.ok, true);
    assert.ok(statusJson.stats.total >= 1);

    const search = await fetch(base + '/api/v1/search?q=web');
    const searchJson = await search.json();
    assert.ok(searchJson.items.length >= 1);

    const health = await fetch(base + '/health');
    assert.equal(health.status, 200);
  } finally {
    await srv.close();
    closeContext(ctx);
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test('HTTP 安全边界：超大请求体被拒绝', async () => {
  const home = path.join(os.tmpdir(), 'cc-http2-' + Date.now());
  fs.mkdirSync(home, { recursive: true });
  const ctx = createContext({ dbPath: path.join(home, 'test.db'), logDir: null, assetDir: path.join(home, 'assets') });
  const srv = createHttpServer(ctx, ctx.services, {});
  const addr = await srv.listen(0, '127.0.0.1');
  try {
    const big = 'x'.repeat(5 * 1024 * 1024);
    const r = await fetch('http://127.0.0.1:' + addr.port + '/mcp', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'ping', params: { big } }) }).catch(() => null);
    assert.ok(!r || r.status >= 400 || r.status === 202);
  } finally {
    await srv.close();
    closeContext(ctx);
    fs.rmSync(home, { recursive: true, force: true });
  }
});