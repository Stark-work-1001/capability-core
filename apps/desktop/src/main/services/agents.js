import fs from 'node:fs';
import path from 'node:path';
import { execFile } from 'node:child_process';
import { now, newId, jsonParse, readTextSafe } from '../core/util.js';
import { USER_HOME } from '../core/paths.js';
import { hostHome } from './discovery.js';

// 智能体接入识别（三条通道叠加）：
// A. 进程识别：任务列表 + 已知可执行文件特征；
// B. 心跳文件：各宿主写 JSON 到 capability-core-data/run/heartbeats/<host>.json（低成本，跨进程）；
// C. MCP 会话：外部智能体通过 /mcp 连入时登记（真实调用证据）。
const HOST_SIGNATURES = [
  { host: 'dsh', label: 'DeepSeek Harness', match: [/deepseek harness/i, /dsh\.exe/i, /dsh-desktop/i], protocol: 'mcp' },
  { host: 'codex', label: 'Codex', match: [/codex\.exe/i, /openai[\\/]codex/i, /codex/i], protocol: 'mcp' },
  { host: 'claude', label: 'Claude Code', match: [/claude/i, /anthropic/i], protocol: 'mcp' },
  { host: 'minimax', label: 'MiniMax Code', match: [/minimax code/i, /minimax.*codex/i], protocol: 'mcp' },
  { host: 'gemini', label: 'Gemini / Antigravity', match: [/antigravity/i, /gemini/i], protocol: 'mcp' },
  { host: 'workbuddy', label: 'WorkBuddy', match: [/workbuddy/i], protocol: 'mcp' },
  { host: 'cursor', label: 'Cursor', match: [/cursor\.exe/i, /cursor/i], protocol: 'mcp' },
  { host: 'qoder', label: 'Qoder', match: [/qoder/i], protocol: 'mcp' }
];

export class AgentService {
  constructor(ctx) {
    this.ctx = ctx;
    this.db = ctx.db;
    this.bus = ctx.bus;
  }

  async detectProcesses() {
    const list = await listProcesses();
    const found = new Map();
    for (const proc of list) {
      const hay = (proc.name + ' ' + proc.commandLine).toLowerCase();
      for (const sig of HOST_SIGNATURES) {
        if (!sig.match.some((re) => re.test(hay))) continue;
        // 排除中枢自身
        if (/capability-core|capabilitycore|electron\.exe/i.test(proc.name) && sig.host !== 'dsh') continue;
        const key = sig.host;
        if (!found.has(key)) found.set(key, { host: key, label: sig.label, protocol: sig.protocol, pids: [], exe: proc.exePath, commandLine: proc.commandLine });
        found.get(key).pids.push(proc.pid);
      }
    }
    return [...found.values()];
  }

  async scan(options) {
    const opts = options || {};
    const procs = await this.detectProcesses();
    const heartbeats = this.readHeartbeats();
    const sessions = this.activeSessions();
    const ts = now();
    const seen = new Set();

    for (const p of procs) {
      const id = 'proc:' + p.host;
      const existing = this.db.get('SELECT * FROM agents WHERE id = ?', [id]);
      const meta = { pids: p.pids, label: p.label };
      if (existing) {
        this.db.run('UPDATE agents SET pid=?, exe_path=?, command_line=?, status=?, last_seen=?, metadata=? WHERE id=?',
          [p.pids[0] || null, p.exe || '', p.commandLine || '', 'running', ts, JSON.stringify(meta), id]);
      } else {
        this.db.run('INSERT INTO agents(id, host, label, kind, pid, exe_path, command_line, status, protocol, first_seen, last_seen, metadata) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)',
          [id, p.host, p.label, 'process', p.pids[0] || null, p.exe || '', p.commandLine || '', 'running', p.protocol, ts, ts, JSON.stringify(meta)]);
        this.bus.info('agents', '发现运行中的智能体：' + p.label, { agentId: id });
      }
      seen.add(p.host);
    }

    for (const hb of heartbeats) {
      const id = 'hb:' + hb.host;
      const existing = this.db.get('SELECT * FROM agents WHERE id = ?', [id]);
      const label = labelFor(hb.host);
      if (existing) {
        this.db.run('UPDATE agents SET status=?, last_seen=?, last_heartbeat=?, metadata=? WHERE id=?', ['idle', ts, hb.seenAt, JSON.stringify(hb.payload), id]);
      } else {
        this.db.run('INSERT INTO agents(id, host, label, kind, pid, status, protocol, first_seen, last_seen, last_heartbeat, metadata) VALUES (?,?,?,?,?,?,?,?,?,?,?)',
          [id, hb.host, label, 'process', hb.pid || null, 'idle', 'heartbeat', ts, ts, hb.seenAt, JSON.stringify(hb.payload)]);
      }
      seen.add(hb.host);
    }

    for (const s of sessions) {
      const id = 'mcp:' + s.host;
      const existing = this.db.get('SELECT * FROM agents WHERE id = ?', [id]);
      const meta = { sessions: s.count, client: s.client };
      if (existing) {
        this.db.run("UPDATE agents SET status='running', protocol='mcp', endpoint=?, last_seen=?, metadata=? WHERE id=?", [s.endpoint || '', ts, JSON.stringify(meta), id]);
      } else {
        this.db.run('INSERT INTO agents(id, host, label, kind, status, protocol, endpoint, first_seen, last_seen, metadata) VALUES (?,?,?,?,?,?,?,?,?,?)',
          [id, s.host, labelFor(s.host), 'mcp-session', 'running', 'mcp', s.endpoint || '', ts, ts, JSON.stringify(meta)]);
      }
      seen.add(s.host);
    }

    // 超过 5 分钟未见 → stopped
    const stale = this.db.all("SELECT id, host FROM agents WHERE status != 'stopped'");
    for (const a of stale) {
      const hostSeen = [...seen].some((h) => a.id.endsWith(a.host));
      if (!hostSeen && ts - 5 * 60 * 1000 > ts) continue;
    }
    for (const a of stale) {
      const row = this.db.get('SELECT * FROM agents WHERE id = ?', [a.id]);
      if (row.last_seen < ts - 5 * 60 * 1000) {
        this.db.run("UPDATE agents SET status='stopped' WHERE id=?", [a.id]);
        this.bus.info('agents', '智能体已停止：' + row.label, { agentId: a.id });
      }
    }
    return this.list();
  }

  readHeartbeats() {
    const dir = path.join(this.ctx.options.home || '', 'run', 'heartbeats');
    const out = [];
    try {
      const files = fs.readdirSync(dir).filter((f) => f.endsWith('.json'));
      for (const f of files) {
        const full = path.join(dir, f);
        const payload = jsonParse(readTextSafe(full) || '{}', {});
        const host = payload.host || f.replace(/\.json$/, '');
        const seenAt = payload.heartbeatAt || payload.ts || fs.statSync(full).mtimeMs;
        out.push({ host, pid: payload.pid, payload, seenAt });
        this.db.run('INSERT INTO heartbeat_files(path, host, pid, payload, seen_at) VALUES (?,?,?,?,?) ON CONFLICT(path) DO UPDATE SET payload=excluded.payload, seen_at=excluded.seen_at',
          [full, host, payload.pid || null, JSON.stringify(payload), seenAt]);
      }
    } catch { /* 目录不存在 */ }
    return out;
  }

  activeSessions() {
    const rows = this.db.all("SELECT * FROM agent_sessions WHERE status = 'open' AND last_seen_at > ?", [now() - 10 * 60 * 1000]);
    const byHost = new Map();
    for (const r of rows) {
      if (!byHost.has(r.host)) byHost.set(r.host, { host: r.host, count: 0, client: '', endpoint: '' });
      const e = byHost.get(r.host);
      e.count++;
      try { const info = JSON.parse(r.client_info); e.client = info.name || info.clientInfo?.name || e.client; } catch { /* ignore */ }
    }
    return [...byHost.values()];
  }

  list() {
    const rows = this.db.all("SELECT * FROM agents ORDER BY (status = 'running') DESC, last_seen DESC");
    return rows.map((a) => ({
      id: a.id, host: a.host, label: a.label, kind: a.kind, pid: a.pid, exePath: a.exe_path,
      status: a.status, protocol: a.protocol, endpoint: a.endpoint, firstSeen: a.first_seen,
      lastSeen: a.last_seen, lastHeartbeat: a.last_heartbeat, metadata: jsonParse(a.metadata, {}),
      capabilityCount: this.countForHost(a.host)
    }));
  }

  countForHost(host) {
    const rows = this.db.all('SELECT hosts FROM capabilities');
    let n = 0;
    for (const r of rows) { try { if (JSON.parse(r.hosts).includes(host)) n++; } catch { /* ignore */ } }
    return n;
  }

  registerSession(info) {
    const ts = now();
    const id = info.id || newId('ses');
    this.db.run('INSERT INTO agent_sessions(id, agent_id, host, protocol, client_info, started_at, last_seen_at, status, ip) VALUES (?,?,?,?,?,?,?,?,?)',
      [id, info.agentId || null, info.host || 'unknown', info.protocol || 'mcp', JSON.stringify(info.client || {}), ts, ts, 'open', info.ip || '']);
    return id;
  }

  touchSession(id, deltaRequests) {
    this.db.run('UPDATE agent_sessions SET last_seen_at=?, requests = requests + ? WHERE id=?', [now(), deltaRequests || 0, id]);
  }

  sessions() {
    return this.db.all('SELECT * FROM agent_sessions ORDER BY started_at DESC LIMIT 100').map((s) => ({
      id: s.id, host: s.host, protocol: s.protocol, client: jsonParse(s.client_info, {}), startedAt: s.started_at,
      lastSeenAt: s.last_seen_at, endedAt: s.ended_at, requests: s.requests, toolCalls: jsonParse(s.tool_calls, {}), status: s.status
    }));
  }

  usageSummary() {
    const rows = this.db.all('SELECT host, COUNT(*) AS c, SUM(requests) AS req FROM agent_sessions GROUP BY host');
    return rows;
  }

  // ---------------- 智能体连通检测（基线 §10 agents.probe） ----------------
  // 逐项体检并返回固定 6 项 checks，任一失败都不抛错。
  // ok 的判据：宿主目录存在 且 MCP 配置可读或可创建（这两项决定「这个宿主能不能被接入」）。
  // online 是「此刻是否在线」的附加证据（进程 / 心跳 / MCP 会话任一命中）。
  async probe(host) {
    const h = String(host || '').trim();
    const started = Date.now();
    const port = Number(this.ctx.options.port) || 18297;
    const endpoint = 'http://127.0.0.1:' + port + '/mcp';
    if (!h) {
      return { ok: false, host: '', label: '', latencyMs: 0, online: false, endpoint, method: 'mcp',
        checks: [{ name: 'host', ok: false, detail: '缺少 host 参数' }], reason: '缺少 host 参数' };
    }
    const checks = [];
    const home = hostHome(h);

    // 1) 宿主主目录
    const homeOk = Boolean(home) && fs.existsSync(home);
    checks.push({ name: 'home', ok: homeOk, detail: home || '未找到宿主目录（该智能体可能未安装）' });

    // 2) MCP 配置：优先已有配置文件，否则看目录能否创建（能建就算可接入）
    let configFile = '';
    for (const name of ['capability-core-mcp.json', 'mcp.json', 'claude_desktop_config.json', 'config.toml', 'settings.json']) {
      if (!home) break;
      const f = path.join(home, name);
      if (fs.existsSync(f)) { configFile = f; break; }
    }
    let configDetail;
    let configOk;
    if (configFile) {
      configOk = true;
      configDetail = configFile;
    } else if (homeOk) {
      try {
        fs.accessSync(home, fs.constants.W_OK);
        configOk = true;
        configDetail = '可创建：' + path.join(home, 'capability-core-mcp.json');
      } catch {
        configOk = false;
        configDetail = '目录不可写：' + home;
      }
    } else {
      configOk = false;
      configDetail = '宿主目录不存在，无法写入配置';
    }
    checks.push({ name: 'config', ok: configOk, detail: configDetail });

    // 3) 进程
    let procs = [];
    try { procs = (await this.detectProcesses()).filter((x) => x.host === h); } catch { procs = []; }
    checks.push({
      name: 'process', ok: procs.length > 0,
      detail: procs.length ? ('运行中，pid ' + procs[0].pids.join(',')) : '未检测到运行中的进程'
    });

    // 4) 心跳文件
    let hb = null;
    try { hb = this.db.get('SELECT * FROM heartbeat_files WHERE host = ? ORDER BY seen_at DESC LIMIT 1', [h]); } catch { hb = null; }
    const hbFresh = Boolean(hb && now() - hb.seen_at < 10 * 60 * 1000);
    checks.push({
      name: 'heartbeat', ok: hbFresh,
      detail: hb ? ('最近心跳 ' + new Date(hb.seen_at).toISOString()) : '无心跳记录'
    });

    // 5) MCP 会话
    let ses = null;
    try {
      ses = this.db.get("SELECT * FROM agent_sessions WHERE host = ? AND status = 'open' AND last_seen_at > ? ORDER BY last_seen_at DESC LIMIT 1", [h, now() - 10 * 60 * 1000]);
    } catch { ses = null; }
    checks.push({ name: 'session', ok: Boolean(ses), detail: ses ? ('活跃 MCP 会话 ' + ses.id) : '无活跃 MCP 会话' });

    // 6) 中枢自身 MCP 端点（能力核心是否在线，决定该宿主能否接上）
    let endpointOk = false;
    let endpointDetail = '';
    try {
      const res = await fetch('http://127.0.0.1:' + port + '/health', { signal: AbortSignal.timeout(2500) });
      endpointOk = res.ok;
      endpointDetail = 'HTTP ' + res.status + ' ' + endpoint;
    } catch (err) {
      endpointDetail = '中枢服务未响应：' + String((err && err.message) || err);
    }
    checks.push({ name: 'endpoint', ok: endpointOk, detail: endpointDetail });

    const online = checks.find((c) => c.name === 'process').ok || checks.find((c) => c.name === 'heartbeat').ok || checks.find((c) => c.name === 'session').ok;
    const ok = homeOk && configOk;
    return {
      ok,
      host: h,
      label: labelFor(h),
      latencyMs: Date.now() - started,
      online,
      checks,
      endpoint,
      method: 'mcp',
      reason: ok ? '' : (homeOk ? 'MCP 配置不可写' : '宿主目录不存在')
    };
  }

  // ---------------- 主动连通（基线 §10 agents.connect） ----------------
  // 把能力核心的 MCP 端点写进宿主原生配置。写前先备份，失败不改原文件。
  connect(host, options) {
    const h = String(host || '').trim();
    const opts = options || {};
    const port = Number(opts.port || this.ctx.options.port) || 18297;
    const endpoint = 'http://127.0.0.1:' + port + '/mcp';
    if (!h) return { ok: false, endpoint, written: [], reason: '缺少 host 参数' };
    const eco = this.ctx.services && this.ctx.services.ecosystem;
    if (!eco) return { ok: false, endpoint, written: [], reason: '生态同步服务不可用' };
    let preview = null;
    try { preview = eco.previewMcp(h, { port }); } catch { preview = null; }
    try {
      const res = eco.applyMcp(h, { port });
      return {
        ok: Boolean(res && res.ok),
        endpoint,
        written: res && res.file ? [res.file] : [],
        backupId: res && res.backupId,
        reason: ''
      };
    } catch (err) {
      // 该宿主没有可直接写入的 MCP 配置文件：不抛错，返回预览供 UI 引导手动接入
      return {
        ok: false,
        endpoint,
        written: [],
        reason: String((err && err.message) || err),
        preview: preview ? preview.text : ''
      };
    }
  }
}

function labelFor(host) {
  const sig = HOST_SIGNATURES.find((s) => s.host === host);
  return sig ? sig.label : host;
}

function listProcesses() {
  return new Promise((resolve) => {
    const cmd = 'powershell.exe';
    const args = ['-NoProfile', '-Command', 'Get-CimInstance Win32_Process | Select-Object ProcessId,Name,ExecutablePath,CommandLine | ConvertTo-Json -Compress'];
    execFile(cmd, args, { timeout: 20000, windowsHide: true, maxBuffer: 24 * 1024 * 1024 }, (err, stdout) => {
      if (err) {
        // 退化：tasklist
        execFile('tasklist', ['/FO', 'CSV', '/NH'], { timeout: 8000, windowsHide: true }, (e2, out2) => {
          if (e2) return resolve([]);
          const items = String(out2).split('\n').map((line) => {
            const m = line.match(/^"([^"]+)","(\d+)"/);
            return m ? { pid: Number(m[2]), name: m[1], exePath: '', commandLine: '' } : null;
          }).filter(Boolean);
          resolve(items);
        });
        return;
      }
      let data;
      try { data = JSON.parse(stdout); } catch { return resolve([]); }
      const arr = Array.isArray(data) ? data : [data];
      resolve(arr.map((p) => ({ pid: p.ProcessId, name: p.Name || '', exePath: p.ExecutablePath || '', commandLine: p.CommandLine || '' })));
    });
  });
}
