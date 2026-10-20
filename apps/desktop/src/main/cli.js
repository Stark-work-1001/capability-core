import path from 'node:path';
import { createContext, closeContext } from './services/context.js';
import { createHttpServer } from './server/http.js';
import { seedSources } from './services/discovery.js';
import { McpServerCore } from './mcp/server-core.js';
import readline from 'node:readline';
import { now } from './core/util.js';
import { PRODUCT_NAME } from './core/identity.js';

// CLI：
//   node cli.js serve [--port 18297] [--db path]
//   node cli.js stdio [--db path]          —— 供只支持 stdio 的智能体以子进程方式接入
//   node cli.js scan
//   node cli.js search "关键词"
export async function main(argv) {
  const args = argv.slice(2);
  const cmd = args[0] || 'serve';
  const opt = parseArgs(args.slice(1));
  if (opt.db) process.env.CC_DB = opt.db;
  const ctx = createContext({ dbPath: opt.db });
  ctx.services.assets.ensureSeed();
  seedSources(ctx.db, ctx.bus, ctx.options.assetDir);

  if (cmd === 'scan') {
    // 先登记能力资产库，再扫描：资产库拥有 assets/ 下实体的身份，
    // 反过来先扫描会让扫描器按清单身份建出重复能力。
    const assets = ctx.services.assets.syncFromDisk();
    const res = await ctx.services.scanner.scanAll({});
    const scope = ctx.services.registry.stats().byScope;
    console.log(JSON.stringify({
      found: res.found, added: res.added, updated: res.updated, faults: res.faults,
      assets: { registered: assets.registered, ...ctx.services.assets.stats() },
      scope
    }, null, 2));
    closeContext(ctx);
    return;
  }
  if (cmd === 'heartbeat') {
    // 智能体侧低成本心跳：宿主启动时调用一次即可被中枢识别（通道 B）
    const os = await import('node:os');
    const fs = await import('node:fs');
    const dir = path.join(ctx.options.home, 'run', 'heartbeats');
    fs.mkdirSync(dir, { recursive: true });
    const host = opt.host || 'custom';
    const payload = { host, name: opt.name || host, pid: process.pid, heartbeatAt: Date.now(), version: opt.version || '', cwd: process.cwd() };
    fs.writeFileSync(path.join(dir, host + '.json'), JSON.stringify(payload, null, 2));
    console.log('心跳已写入：' + path.join(dir, host + '.json'));
    closeContext(ctx);
    return;
  }
  if (cmd === 'search') {
    const q = args[1] || '';
    const res = ctx.services.search.search(q, { limit: 10 });
    console.log(JSON.stringify(res, null, 2));
    closeContext(ctx);
    return;
  }
  if (cmd === 'stdio') {
    await runStdio(ctx, opt);
    return;
  }
  const port = Number(opt.port || process.env.CC_PORT || 18297);
  const host = opt.host || '127.0.0.1';
  const srv = createHttpServer(ctx, ctx.services, {});
  const addr = await srv.listen(port, host);
  console.log(PRODUCT_NAME + ' serving at http://' + host + ':' + addr.port + '  (MCP: /mcp)');
  const shutdown = async () => { await srv.close(); closeContext(ctx); process.exit(0); };
  process.on('SIGINT', shutdown); process.on('SIGTERM', shutdown);
}

function runStdio(ctx, opt) {
  const svc = ctx.services;
  const core = new McpServerCore(ctx, svc);
  const auth = { sessionId: null, agentId: null, ip: 'stdio' };
  const rl = readline.createInterface({ input: process.stdin });
  rl.on('line', async (line) => {
    const text = line.trim();
    if (!text) return;
    let msg;
    try { msg = JSON.parse(text); } catch { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: null, error: { code: -32700, message: 'Parse error' } }) + '\n'); return; }
    const out = Array.isArray(msg) ? await Promise.all(msg.map((m) => core.handle(m, auth))) : await core.handle(msg, auth);
    if (out) process.stdout.write(JSON.stringify(out) + '\n');
  });
  rl.on('close', () => { closeContext(ctx); process.exit(0); });
}

function parseArgs(args) {
  const out = {};
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a.startsWith('--')) {
      const key = a.slice(2);
      const next = args[i + 1];
      if (next && !next.startsWith('--')) { out[key] = next; i++; }
      else out[key] = true;
    }
  }
  return out;
}

main(process.argv).catch((err) => { console.error(err); process.exit(1); });
