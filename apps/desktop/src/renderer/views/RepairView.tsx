import React, { useEffect, useState } from 'react';
import { api, fmtTime, KIND_LABEL, type AgentRow, type FaultRow } from '../lib/api';
import { Badge, Empty, Modal, Spinner } from '../components/ui';

export default function RepairView({ toast, onChanged }: { toast: (m: string, t?: string) => void; onChanged: () => void }) {
  const [faults, setFaults] = useState<FaultRow[]>([]);
  const [stats, setStats] = useState<any>(null);
  const [filter, setFilter] = useState('open');
  const [severity, setSeverity] = useState('');
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [detail, setDetail] = useState<{ fault: FaultRow; repairs: any[] } | null>(null);
  const [dispatchFor, setDispatchFor] = useState<FaultRow | null>(null);
  const [dispatchNote, setDispatchNote] = useState('');
  const [agents, setAgents] = useState<AgentRow[]>([]);
  const [agentChoice, setAgentChoice] = useState('');

  const load = async () => {
    setLoading(true);
    try {
      setFaults(await api.faults({ status: filter, severity: severity || undefined }));
      setStats(await api.faultStats());
      setAgents(await api.agents());
    } catch (e: any) { toast(e.message, 'error'); }
    finally { setLoading(false); }
  };
  useEffect(() => { load(); }, [filter, severity]);

  const diagnose = async () => {
    setBusy('diagnose');
    try { const r = await api.diagnose(); toast('诊断完成：新增 ' + r.detected + '，自动关闭 ' + r.closed + '，当前 ' + r.open + ' 个未解决', 'success'); load(); onChanged(); }
    catch (e: any) { toast(e.message, 'error'); }
    finally { setBusy(null); }
  };

  const autofix = async (f: FaultRow) => {
    setBusy(f.id);
    try { const r = await api.autoFix(f.id); toast('修复成功：' + r.message + '（备份 ' + r.backupId + '）', 'success'); load(); onChanged(); }
    catch (e: any) { toast('修复失败：' + e.message, 'error'); }
    finally { setBusy(null); }
  };

  const openDetail = async (f: FaultRow) => {
    const repairs = await api.repairs(f.id);
    setDetail({ fault: f, repairs });
  };

  const doDispatch = async () => {
    if (!dispatchFor) return;
    try {
      const r = await api.dispatchFault(dispatchFor.id, agentChoice || 'auto', dispatchNote);
      toast('已派单，工单提示已生成', 'success');
      setDispatchFor(null); setDispatchNote('');
      await navigator.clipboard?.writeText(r.prompt).catch(() => {});
      load(); onChanged();
    } catch (e: any) { toast(e.message, 'error'); }
  };

  return (
    <div>
      <div className="grid cols-4 mb16">
        <div className="card stat red"><div className="stat-glow" /><div className="stat-label">未解决故障</div><div className="stat-value">{stats?.open || 0}</div><div className="stat-foot">跨全部来源</div></div>
        <div className="card stat amber"><div className="stat-glow" /><div className="stat-label">可自动修复</div><div className="stat-value">{faults.filter((f) => f.autoFixable).length}</div><div className="stat-foot">内置安全修复规则</div></div>
        <div className="card stat blue"><div className="stat-glow" /><div className="stat-label">已派单</div><div className="stat-value">{faults.filter((f) => f.status === 'dispatched').length}</div><div className="stat-foot">等待智能体回传补丁</div></div>
        <div className="card stat green"><div className="stat-glow" /><div className="stat-label">维修记录</div><div className="stat-value">{detail ? detail.repairs.length : '—'}</div><div className="stat-foot">含备份与哈希</div></div>
      </div>

      <div className="toolbar">
        <button className="btn primary" onClick={diagnose} disabled={busy === 'diagnose'}>{busy === 'diagnose' ? <><Spinner /> 诊断中</> : '⚒ 执行诊断'}</button>
        <div className="seg">
          {[['open', '未解决'], ['dispatched', '已派单'], ['resolved', '已解决'], ['all', '全部']].map(([k, l]) => (
            <button key={k} className={filter === k ? 'active' : ''} onClick={() => setFilter(k)}>{l}</button>
          ))}
        </div>
        <select className="select" value={severity} onChange={(e) => setSeverity(e.target.value)}>
          <option value="">全部级别</option><option value="critical">严重</option><option value="error">错误</option><option value="warning">警告</option>
        </select>
        <div className="grow" />
        <button className="btn ghost" onClick={load}>↻</button>
      </div>

      <div className="card">
        {loading ? <div className="empty"><Spinner /> 加载故障清单…</div> : (
          <table className="table">
            <thead><tr><th style={{ width: 80 }}>级别</th><th>故障</th><th style={{ width: 100 }}>能力类型</th><th style={{ width: 120 }}>状态</th><th style={{ width: 100 }}>发现时间</th><th style={{ width: 210 }}></th></tr></thead>
            <tbody>
              {faults.map((f) => (
                <tr key={f.id} onClick={() => openDetail(f)}>
                  <td><Badge tone={f.severity === 'critical' ? 'red' : f.severity === 'error' ? 'red' : f.severity === 'warning' ? 'amber' : 'gray'}>{f.severity === 'critical' ? '严重' : f.severity === 'error' ? '错误' : f.severity === 'warning' ? '警告' : '提示'}</Badge></td>
                  <td>
                    <div style={{ fontWeight: 550 }}>{f.title}</div>
                    <div className="hint" style={{ maxWidth: 560, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{f.capabilityName || '—'} · {f.code}</div>
                  </td>
                  <td>{f.kind ? <Badge tone="blue">{KIND_LABEL[f.kind] || f.kind}</Badge> : '—'}</td>
                  <td><Badge tone={f.status === 'resolved' ? 'green' : f.status === 'dispatched' ? 'blue' : f.status === 'open' ? 'amber' : 'gray'}>{f.status === 'resolved' ? '已解决' : f.status === 'dispatched' ? '已派单' : f.status === 'open' ? '待处理' : f.status}</Badge></td>
                  <td className="hint">{fmtTime(f.detectedAt)}</td>
                  <td className="right">
                    {f.autoFixable && f.status !== 'resolved' ? <button className="btn small" disabled={busy === f.id} onClick={(e) => { e.stopPropagation(); autofix(f); }}>{busy === f.id ? '修复中' : '自动修复'}</button> : null}
                    {f.status !== 'resolved' ? <button className="btn small ghost" onClick={(e) => { e.stopPropagation(); setDispatchFor(f); }}>派单</button> : null}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {!loading && !faults.length ? <Empty icon="⚒" title="没有故障" hint="点击「执行诊断」检查全部能力的完整性" /> : null}
      </div>

      {dispatchFor ? (
        <Modal title="派单给智能体" onClose={() => setDispatchFor(null)} footer={<>
          <button className="btn" onClick={() => setDispatchFor(null)}>取消</button>
          <button className="btn primary" onClick={doDispatch}>生成工单</button>
        </>}>
          <div className="hint mb12">中枢只负责诊断与定位，修复由接入的智能体执行。生成工单后，把提示交给智能体；修复完成后通过 MCP 工具 capability.submit_patch 回传补丁。</div>
          <div className="kv">
            <dt>故障</dt><dd>{dispatchFor.title}</dd>
            <dt>能力</dt><dd>{dispatchFor.capabilityName} <span className="mono hint">({dispatchFor.capabilityId})</span></dd>
            <dt>证据</dt><dd className="mono hint">{JSON.stringify(dispatchFor.evidence)}</dd>
          </div>
          <div className="mt12">
            <select className="select" style={{ width: '100%' }} value={agentChoice} onChange={(e) => setAgentChoice(e.target.value)}>
              <option value="">自动选择（auto）</option>
              {agents.filter((a) => a.status !== 'stopped').map((a) => <option key={a.id} value={a.id}>{a.label}（{a.status}）</option>)}
            </select>
            <textarea className="textarea mt12" style={{ width: '100%', minHeight: 90 }} placeholder="备注（可选）：例如期望的修复方向" value={dispatchNote} onChange={(e) => setDispatchNote(e.target.value)} />
          </div>
        </Modal>
      ) : null}

      {detail ? (
        <Modal title="故障详情与维修历史" onClose={() => setDetail(null)}>
          <div className="flex mb12">
            <Badge tone="red">{detail.fault.severity}</Badge>
            <span style={{ fontWeight: 600 }}>{detail.fault.title}</span>
          </div>
          <div className="kv mb12">
            <dt>故障代码</dt><dd className="mono">{detail.fault.code}</dd>
            <dt>能力</dt><dd>{detail.fault.capabilityName}</dd>
            <dt>说明</dt><dd>{detail.fault.detail}</dd>
            <dt>证据</dt><dd className="mono hint">{JSON.stringify(detail.fault.evidence, null, 2)}</dd>
            <dt>发现时间</dt><dd>{fmtTime(detail.fault.detectedAt)}</dd>
          </div>
          <div className="section-title">维修记录</div>
          {detail.repairs.length ? (
            <div className="timeline">
              {detail.repairs.map((r) => (
                <div key={r.id} className="tl-item info">
                  <div className="tl-time">{fmtTime(r.createdAt)} · {r.actor} · {r.action}</div>
                  <div className="tl-msg">{r.message?.slice(0, 300) || r.result}</div>
                  {r.patch?.file ? <div className="hint mono">{r.patch.file}</div> : null}
                </div>
              ))}
            </div>
          ) : <div className="hint">尚无维修记录</div>}
        </Modal>
      ) : null}
    </div>
  );
}
