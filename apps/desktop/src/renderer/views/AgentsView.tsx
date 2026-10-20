import React, { useEffect, useState } from 'react';
import { api, fmtTime, HOST_LABEL, type AgentRow, type ConnectResult, type ProbeResult } from '../lib/api';
import { Badge, Empty, Spinner } from '../components/ui';

export default function AgentsView({ toast, onChanged }: { toast: (m: string, t?: string) => void; onChanged: () => void }) {
  const [rows, setRows] = useState<AgentRow[]>([]);
  const [sessions, setSessions] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);
  const [targets, setTargets] = useState<any[]>([]);
  const [server, setServer] = useState<any>(null);

  // 连通检测 / 主动连通 结果按宿主缓存
  const [probes, setProbes] = useState<Record<string, ProbeResult>>({});
  const [connects, setConnects] = useState<Record<string, ConnectResult>>({});
  const [busy, setBusy] = useState<string>('');

  const load = async () => {
    setLoading(true);
    try {
      setRows(await api.scanAgents());
      setSessions(await api.sessions());
      setTargets(await api.ecosystemTargets());
      api.serverInfo().then(setServer).catch(() => {});
    } catch (e: any) { toast(e.message, 'error'); }
    finally { setLoading(false); }
  };
  useEffect(() => { load(); }, []);

  const preview = async (host: string) => {
    try { const r = await api.previewSync(host); toast('已生成 ' + host + ' 的 MCP 配置片段（见下方）'); setPreviewHost(host); setPreviewText(r.text); }
    catch (e: any) { toast(e.message, 'error'); }
  };
  const [previewHost, setPreviewHost] = useState('');
  const [previewText, setPreviewText] = useState('');

  const apply = async (host: string) => {
    try { const r = await api.applySync(host); toast('已写入 ' + r.file + '（备份 ' + r.backupId + '）', 'success'); onChanged(); }
    catch (e: any) { toast(e.message, 'error'); }
  };

  /** 连通检测：只读探测宿主是否可达，不写任何文件 */
  const probe = async (a: AgentRow) => {
    const key = 'probe:' + a.host;
    setBusy(key);
    try {
      const r = await api.probeAgent(a.host);
      setProbes((prev) => ({ ...prev, [a.host]: r }));
      const passed = (r.checks || []).filter((c) => c.ok).length;
      toast('连通检测：' + (r.ok ? '可达' : '不可达') + ' · ' + r.latencyMs + 'ms · 检查通过 ' + passed + '/' + (r.checks || []).length,
        r.ok ? 'success' : 'error');
    } catch (e: any) { toast('连通检测失败：' + e.message, 'error'); }
    finally { setBusy(''); }
  };

  /** 主动连通：把能力核心的 MCP 端点写入宿主原生配置（写前自动备份） */
  const connect = async (a: AgentRow) => {
    const key = 'connect:' + a.host;
    setBusy(key);
    try {
      const r = await api.connectAgent(a.host);
      setConnects((prev) => ({ ...prev, [a.host]: r }));
      toast(r.ok
        ? '主动连通完成：写入 ' + (r.written || []).length + ' 个配置文件' + (r.backupId ? '（备份 ' + r.backupId + '）' : '')
        : '主动连通未完成', r.ok ? 'success' : 'error');
      onChanged();
    } catch (e: any) { toast('主动连通失败：' + e.message, 'error'); }
    finally { setBusy(''); }
  };

  const port = server?.port || 18297;
  const mcpSample = JSON.stringify({
    mcpServers: {
      'capability-core': { type: 'streamableHttp', url: 'http://127.0.0.1:' + port + '/mcp' }
    }
  }, null, 2);

  return (
    <div>
      <div className="toolbar">
        <button className="btn" onClick={load} disabled={loading}>{loading ? <><Spinner /> 检测中</> : '↻ 重新检测'}</button>
        <span className="hint">通过进程名、心跳文件与 MCP 会话三条通道识别在线状态；卡片上的「连通检测」只读探测，「主动连通」会写入宿主配置</span>
        <div className="grow" />
      </div>

      <div className="agent-grid mb16">
        {rows.map((a) => {
          const pr = probes[a.host];
          const cn = connects[a.host];
          return (
            <div key={a.id} className="agent-card col">
              <div className="agent-card-head">
                <div className={'agent-orb ' + (a.status === 'running' ? 'running' : '')}>{a.status === 'running' ? '◉' : '○'}</div>
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div className="flex">
                    <span style={{ fontWeight: 600 }}>{a.label}</span>
                    <Badge tone={a.status === 'running' ? 'green' : a.status === 'idle' ? 'blue' : 'gray'}>{a.status === 'running' ? '运行中' : a.status === 'idle' ? '心跳在线' : '未运行'}</Badge>
                  </div>
                  <div className="hint" style={{ marginTop: 3 }}>
                    {HOST_LABEL[a.host] || a.host} · {a.kind === 'process' ? '进程识别' : 'MCP 会话'}
                    {a.pid ? ' · PID ' + a.pid : ''}
                  </div>
                  <div className="hint">能力 {a.capabilityCount} 项 · 最后活动 {fmtTime(a.lastSeen)}</div>
                </div>
                <div className="agent-card-actions">
                  <button className="btn small" disabled={busy === 'probe:' + a.host} onClick={() => probe(a)}>
                    {busy === 'probe:' + a.host ? '检测中' : '⌁ 连通检测'}
                  </button>
                  <button className="btn small primary" disabled={busy === 'connect:' + a.host} onClick={() => connect(a)}>
                    {busy === 'connect:' + a.host ? '连通中' : '⇄ 主动连通'}
                  </button>
                </div>
              </div>

              {pr ? (
                <div className="agent-result">
                  <div className="flex mb8">
                    <Badge tone={pr.ok ? 'green' : 'red'}>{pr.ok ? '连通正常' : '连通失败'}</Badge>
                    <span className="hint">时延</span>
                    <span className="mono" style={{ fontSize: 11.5 }}>{pr.latencyMs} ms</span>
                    <div className="grow" />
                    <span className="hint mono">{a.host}</span>
                  </div>
                  {(pr.checks || []).length ? (pr.checks || []).map((c, i) => (
                    <div key={c.name + i} className="check-row">
                      <span className={c.ok ? 'check-ok' : 'check-fail'}>{c.ok ? '✓' : '✕'}</span>
                      <span style={{ fontSize: 11.5 }}>{c.name}</span>
                      <div className="grow" />
                      <span className="hint mono" style={{ maxWidth: 190, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={c.detail}>{c.detail}</span>
                    </div>
                  )) : <div className="hint">该宿主未返回逐项检查结果</div>}
                </div>
              ) : null}

              {cn ? (
                <div className="agent-result">
                  <div className="flex mb8">
                    <Badge tone={cn.ok ? 'green' : 'red'}>{cn.ok ? '已连通' : '未连通'}</Badge>
                    <span className="hint">端点</span>
                    <span className="mono" style={{ fontSize: 11 }}>{cn.endpoint}</span>
                  </div>
                  <div className="hint mb8">写入的配置文件（{((cn.written || []) as string[]).length}）：</div>
                  {((cn.written || []) as string[]).length ? ((cn.written || []) as string[]).map((f) => (
                    <div key={f} className="mono hint" style={{ wordBreak: 'break-all', paddingLeft: 2 }}>· {f}</div>
                  )) : <div className="hint">没有写入任何文件（宿主可能只支持手动配置）</div>}
                  {cn.backupId ? <div className="hint mt8">写前备份：<span className="mono">{cn.backupId}</span></div> : null}
                </div>
              ) : null}
            </div>
          );
        })}
        {!rows.length && !loading ? (
          <div className="card" style={{ gridColumn: '1 / -1' }}><Empty icon="◉" title="尚未识别到智能体" hint="启动任意接入的智能体后点击「重新检测」" /></div>
        ) : null}
      </div>

      <div className="grid cols-2">
        <div className="card">
          <div className="card-header"><div className="card-title">MCP 接入配置</div><div className="grow" /><span className="hint">Streamable HTTP</span></div>
          <div className="card-body">
            <div className="hint mb8">把下面这段加入智能体的 MCP 配置，即可让它主动检索能力核心中的能力：</div>
            <div className="code">{mcpSample}</div>
            <div className="hint mt8">只支持 stdio 的宿主可用：<span className="mono">node apps/desktop/src/main/cli.js stdio</span></div>
            <div className="section-title">可写目标</div>
            {targets.filter((t) => t.writeable).map((t) => (
              <div key={t.file} className="flex mb8">
                <span className="chip">{t.host}</span>
                <span className="hint grow mono" style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>{t.file}</span>
                <button className="btn small ghost" onClick={() => preview(t.host)}>预览</button>
                <button className="btn small" onClick={() => apply(t.host)}>写入</button>
              </div>
            ))}
            {previewText ? <div className="mt12"><div className="hint mb8">预览（{previewHost}）：</div><div className="code" style={{ maxHeight: 180 }}>{previewText}</div></div> : null}
          </div>
        </div>

        <div className="card">
          <div className="card-header"><div className="card-title">最近 MCP 会话</div><div className="grow" /><span className="hint">{sessions.length}</span></div>
          <div className="card-body" style={{ padding: 0, maxHeight: 420, overflowY: 'auto' }}>
            {sessions.slice(0, 20).map((s) => (
              <div key={s.id} className="list-item" style={{ cursor: 'default' }}>
                <div className="title">
                  {HOST_LABEL[s.host] || s.host}
                  <Badge tone={s.status === 'open' ? 'green' : 'gray'}>{s.status === 'open' ? '活跃' : '已结束'}</Badge>
                  <div className="grow" />
                  <span className="hint mono">{s.requests} 次调用</span>
                </div>
                <div className="meta">
                  <span className="mono">{s.client?.name || '未知客户端'} {s.client?.version || ''}</span>
                  <span>·</span>
                  <span>开始 {fmtTime(s.startedAt)}</span>
                </div>
                {s.toolCalls && Object.keys(s.toolCalls).length ? (
                  <div className="meta">{Object.entries(s.toolCalls).map(([k, v]) => <span key={k} className="chip">{k} × {String(v)}</span>)}</div>
                ) : null}
              </div>
            ))}
            {!sessions.length ? <Empty title="还没有 MCP 会话" hint="智能体连入 /mcp 后会自动登记" /> : null}
          </div>
        </div>
      </div>
    </div>
  );
}
