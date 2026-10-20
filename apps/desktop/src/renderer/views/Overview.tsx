import React, { useEffect, useMemo, useState } from 'react';
import { api, fmtTime, HOST_LABEL, KIND_ICON, KIND_LABEL, type EventRow, type StatusInfo } from '../lib/api';
import { Badge, Empty, Stat } from '../components/ui';

export default function Overview({ status, events, onNavigate, onRefresh, toast, onOpenCapability }: {
  status: StatusInfo | null; events: EventRow[]; onNavigate: (v: string) => void; onRefresh: () => Promise<any>; toast: (m: string, t?: string) => void;
  onOpenCapability?: (id: string) => void;
}) {
  const [agents, setAgents] = useState<any[]>([]);
  const [topCaps, setTopCaps] = useState<any[]>([]);
  const [workflows, setWorkflows] = useState<any[]>([]);
  const [sources, setSources] = useState<any[]>([]);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api.agents().then(setAgents).catch(() => {});
    api.capabilities({ limit: 8, sort: 'usage' }).then(setTopCaps).catch(() => {});
    api.workflows().then(setWorkflows).catch(() => {});
    api.sources().then(setSources).catch(() => {});
  }, [status?.stats?.total]);

  const s = status?.stats || {};
  const byKind = useMemo(() => Object.fromEntries((s.byKind || []).map((k: any) => [k.kind, k.c])), [s.byKind]);
  const hosts = useMemo(() => (s.byHost || []).filter((h: any) => h.c > 0), [s.byHost]);
  const runningAgents = agents.filter((a) => a.status === 'running');
  const maxHost = Math.max(1, ...hosts.map((h: any) => h.c));

  const runInspection = async () => {
    setBusy(true);
    const wf = workflows.find((w) => w.name === '全量巡检');
    try {
      if (!wf) throw new Error('未找到巡检工作流');
      const r = await api.runWorkflow(wf.id);
      toast('巡检完成：' + r.summary.completed + '/' + r.summary.total + ' 步骤成功', r.status === 'failed' ? 'error' : 'success');
      await onRefresh();
      api.workflows().then(setWorkflows);
      api.sources().then(setSources);
    } catch (e: any) { toast('巡检失败：' + e.message, 'error'); }
    finally { setBusy(false); }
  };

  return (
    <div>
      <div className="hero mb16">
        <div className="hero-title">一个中枢，接入全部智能体能力</div>
        <div className="hero-sub">
          能力核心把散落在 DSH、Codex、Claude Code、MiniMax、Gemini 等宿主的技能、MCP 服务与插件统一采集进 SQLite，
          任何接入的智能体都可以通过 MCP 主动检索、读取并调用——不需要重复安装。
        </div>
        <div className="flex mt16 wrap">
          <button className="btn primary" onClick={runInspection} disabled={busy}>{busy ? '巡检中…' : '⚡ 执行全量巡检'}</button>
          <button className="btn" onClick={() => onNavigate('search')}>⌕ 检索实验室</button>
          <button className="btn" onClick={() => onNavigate('agents')}>◉ 接入状态</button>
          <button className="btn" onClick={() => onNavigate('assets')}>▣ 能力资产库</button>
          <div className="grow" />
          <div className="hint mono" style={{ fontSize: 11 }}>MCP 端点 http://127.0.0.1:{status?.port || 18297}/mcp</div>
        </div>
      </div>

      <div className="grid cols-4">
        <Stat label="能力总数" value={s.total || 0} foot={'安装实例 ' + (s.installations || 0) + ' · 来源 ' + (s.sources || 0)} tone="green" />
        <Stat label="在线智能体" value={runningAgents.length} foot={runningAgents.map((a) => a.label).join(' / ') || '暂无运行中智能体'} tone="blue" />
        <Stat label="待处理故障" value={s.faultsOpen || 0} foot="维修站统一处理" tone={s.faultsOpen ? 'red' : 'green'} />
        <Stat label="重复候选 / 可更新" value={(s.duplicatesPending || 0) + ' / ' + (s.updatesAvailable || 0)} foot="去重裁决 + 更新管理" tone="amber" />
      </div>

      <div className="grid cols-2 mt16">
        <div className="card">
          <div className="card-header"><div className="card-title">能力构成</div><div className="grow" /><span className="hint">{s.total || 0} 项</span></div>
          <div className="card-body">
            {['skill', 'mcp', 'plugin', 'prompt', 'agent'].map((k) => {
              const v = byKind[k] || 0;
              const pct = s.total ? Math.round((v / s.total) * 100) : 0;
              return (
                <div key={k} className="mb12">
                  <div className="flex" style={{ marginBottom: 5 }}>
                    <span className="kind-icon">{KIND_ICON[k]}</span>
                    <span style={{ fontSize: 12 }}>{KIND_LABEL[k] || k}</span>
                    <div className="grow" />
                    <span className="hint mono">{v} · {pct}%</span>
                  </div>
                  <div className="progress-line"><span style={{ width: pct + '%' }} /></div>
                </div>
              );
            })}
            <div className="section-title" style={{ marginTop: 18 }}>按来源智能体</div>
            {hosts.map((h: any) => (
              <div key={h.host} className="mb8">
                <div className="flex" style={{ marginBottom: 4 }}>
                  <span style={{ fontSize: 11.5 }}>{HOST_LABEL[h.host] || h.host}</span>
                  <div className="grow" />
                  <span className="hint mono">{h.c}</span>
                </div>
                <div className="progress-line"><span style={{ width: Math.round((h.c / maxHost) * 100) + '%', background: 'linear-gradient(90deg,#35d0a5,#2aa8ff)' }} /></div>
              </div>
            ))}
            {!hosts.length ? <div className="hint">尚未扫描任何来源</div> : null}
          </div>
        </div>

        <div className="card">
          <div className="card-header">
            <div className="card-title">智能体接入</div><div className="grow" />
            <button className="btn ghost small" onClick={() => onNavigate('agents')}>管理</button>
          </div>
          <div className="card-body" style={{ padding: 0, maxHeight: 330, overflowY: 'auto' }}>
            {agents.length === 0 ? <Empty icon="◉" title="尚未识别到智能体" hint="启动 DSH / Codex 等智能体后会自动出现" /> :
              agents.slice(0, 8).map((a) => (
                <div key={a.id} className="list-item" onClick={() => onNavigate('agents')}>
                  <div className="title">
                    <span className={'agent-orb ' + (a.status === 'running' ? 'running' : '')} style={{ width: 22, height: 22, fontSize: 10, borderRadius: 6 }}>◉</span>
                    {a.label}
                    <Badge tone={a.status === 'running' ? 'green' : a.status === 'idle' ? 'blue' : 'gray'}>{a.status === 'running' ? '运行中' : a.status === 'idle' ? '心跳' : '已停止'}</Badge>
                    <div className="grow" />
                    <span className="hint mono">{a.capabilityCount} 项能力</span>
                  </div>
                  <div className="meta">
                    <span>{HOST_LABEL[a.host] || a.host}</span>
                    <span>·</span>
                    <span>{a.protocol || 'mcp'}</span>
                    <span>·</span>
                    <span>最后活动 {fmtTime(a.lastSeen)}</span>
                  </div>
                </div>
              ))}
          </div>
        </div>
      </div>

      <div className="grid cols-2 mt16">
        <div className="card">
          <div className="card-header"><div className="card-title">最近事件</div><div className="grow" /><span className="hint">实时</span></div>
          <div className="card-body" style={{ maxHeight: 360, overflowY: 'auto' }}>
            {events.length === 0 ? <div className="hint">暂无事件</div> :
              <div className="timeline">
                {events.slice(0, 18).map((e) => (
                  <div key={e.id} className={'tl-item ' + e.level}>
                    <div className="tl-time">{new Date(e.ts).toLocaleTimeString('zh-CN')} · {e.topic}</div>
                    <div className="tl-msg">{e.message}</div>
                  </div>
                ))}
              </div>}
          </div>
        </div>

        <div className="card">
          <div className="card-header"><div className="card-title">工作流状态</div><div className="grow" /><button className="btn ghost small" onClick={() => onNavigate('workflows')}>全部</button></div>
          <div className="card-body" style={{ padding: 0 }}>
            {workflows.slice(0, 4).map((w) => (
              <div key={w.id} className="list-item" onClick={() => onNavigate('workflows')}>
                <div className="title">{w.name}
                  <Badge tone={w.lastRun ? (w.lastRun.status === 'completed' ? 'green' : w.lastRun.status === 'failed' ? 'red' : 'blue') : 'gray'}>
                    {w.lastRun ? (w.lastRun.status === 'completed' ? '已完成' : w.lastRun.status === 'failed' ? '失败' : '运行中') : '未运行'}
                  </Badge>
                  <div className="grow" />
                  <span className="hint">{w.steps.length} 步</span>
                </div>
                <div className="desc">{w.description}</div>
                <div className="meta">{w.lastRun ? '最近 ' + fmtTime(w.lastRun.startedAt) + ' · ' + (w.lastRun.summary?.completed || 0) + '/' + (w.lastRun.summary?.total || 0) + ' 成功' : '尚未执行'}</div>
              </div>
            ))}
          </div>
        </div>
      </div>

      <div className="card mt16">
        <div className="card-header"><div className="card-title">高频能力</div><div className="grow" /><button className="btn ghost small" onClick={() => onNavigate('capabilities')}>进入宿主能力库</button></div>
        <div className="card-body" style={{ padding: 0 }}>
          <table className="table">
            <thead><tr><th style={{ width: 40 }}></th><th>能力</th><th>类型</th><th>作者</th><th>宿主</th><th className="right">调用</th></tr></thead>
            <tbody>
              {topCaps.map((c) => (
                <tr key={c.id} onClick={() => onOpenCapability ? onOpenCapability(c.id) : onNavigate('capabilities')}>
                  <td><span className="kind-icon">{KIND_ICON[c.kind]}</span></td>
                  <td><div style={{ fontWeight: 550 }}>{c.displayName}</div><div className="hint" style={{ maxWidth: 460, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{c.summary}</div></td>
                  <td><Badge tone={c.kind === 'skill' ? 'blue' : c.kind === 'mcp' ? 'violet' : 'green'}>{KIND_LABEL[c.kind] || c.kind}</Badge></td>
                  <td className="dim">{c.author || '未声明'}</td>
                  <td>{(c.hosts || []).map((h: string) => <span key={h} className="chip" style={{ marginRight: 4 }}>{HOST_LABEL[h] || h}</span>)}</td>
                  <td className="right mono">{c.usageCount || 0}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {!topCaps.length ? <Empty title="宿主能力库暂无能力" hint="点击右上角「扫描全部」开始采集，或到「能力资产库」新建自有资产" /> : null}
        </div>
      </div>
    </div>
  );
}
