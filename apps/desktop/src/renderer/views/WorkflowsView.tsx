import React, { useEffect, useState } from 'react';
import { api, fmtTime, type WorkflowRow } from '../lib/api';
import { Badge, Empty, Spinner } from '../components/ui';

export default function WorkflowsView({ toast, onChanged }: { toast: (m: string, t?: string) => void; onChanged: () => void }) {
  const [rows, setRows] = useState<WorkflowRow[]>([]);
  const [runs, setRuns] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);
  const [running, setRunning] = useState<string | null>(null);
  const [activeRun, setActiveRun] = useState<any>(null);

  const load = async () => {
    setLoading(true);
    try { setRows(await api.workflows()); setRuns(await api.workflowRuns()); }
    catch (e: any) { toast(e.message, 'error'); }
    finally { setLoading(false); }
  };
  useEffect(() => { load(); }, []);

  const run = async (w: WorkflowRow) => {
    setRunning(w.id);
    setActiveRun({ name: w.name, steps: w.steps.map((s) => ({ ...s, status: 'pending' })), status: 'running' });
    try {
      const r = await api.runWorkflow(w.id);
      setActiveRun({ name: w.name, steps: r.steps, status: r.status, summary: r.summary });
      toast('工作流' + (r.status === 'completed' ? '完成' : '失败') + '：' + r.summary.completed + '/' + r.summary.total + ' 步', r.status === 'completed' ? 'success' : 'error');
      load(); onChanged();
    } catch (e: any) {
      toast('执行失败：' + e.message, 'error');
      setActiveRun((a: any) => a ? { ...a, status: 'failed' } : null);
    }
    finally { setRunning(null); }
  };

  return (
    <div className="grid" style={{ gridTemplateColumns: 'minmax(0,1fr) 380px' }}>
      <div>
        {loading ? <div className="empty"><Spinner /></div> : null}
        {rows.map((w) => (
          <div key={w.id} className="card mb12">
            <div className="card-header">
              <span className="kind-icon">⇉</span>
              <div className="card-title">{w.name}</div>
              <Badge tone={w.kind === 'seed' ? 'blue' : 'gray'}>{w.kind === 'seed' ? '内置' : '自定义'}</Badge>
              {w.lastRun ? <Badge tone={w.lastRun.status === 'completed' ? 'green' : w.lastRun.status === 'failed' ? 'red' : 'blue'}>{w.lastRun.status === 'completed' ? '上次成功' : w.lastRun.status === 'failed' ? '上次失败' : '运行中'}</Badge> : <Badge tone="gray">未运行</Badge>}
              <div className="grow" />
              <span className="hint">{w.steps.length} 步</span>
              <button className="btn small primary" disabled={running === w.id} onClick={() => run(w)}>{running === w.id ? <><Spinner /> 执行中</> : '▶ 执行'}</button>
            </div>
            <div className="card-body">
              <div className="hint mb8">{w.description}</div>
              <div className="flex wrap" style={{ gap: 6 }}>
                {w.steps.map((s, i) => (
                  <React.Fragment key={i}>
                    <span className="chip">{s.label}</span>
                    {i < w.steps.length - 1 ? <span className="faint">→</span> : null}
                  </React.Fragment>
                ))}
              </div>
              {w.lastRun ? (
                <div className="hint mt8">最近一次：{fmtTime(w.lastRun.startedAt)} · 用时 {w.lastRun.summary?.ms || 0}ms · {w.lastRun.summary?.completed || 0}/{w.lastRun.summary?.total || 0} 成功</div>
              ) : null}
            </div>
          </div>
        ))}
        {!loading && !rows.length ? <div className="card"><Empty title="没有工作流" /></div> : null}

        <div className="section-title">运行历史</div>
        <div className="card">
          <table className="table">
            <thead><tr><th>工作流</th><th style={{ width: 90 }}>状态</th><th style={{ width: 80 }}>触发</th><th style={{ width: 120 }}>开始</th><th style={{ width: 90 }} className="right">用时</th><th style={{ width: 80 }} className="right">步骤</th></tr></thead>
            <tbody>
              {runs.map((r) => (
                <tr key={r.id} style={{ cursor: 'default' }}>
                  <td>{r.name || r.workflowId}</td>
                  <td><Badge tone={r.status === 'completed' ? 'green' : r.status === 'failed' ? 'red' : 'blue'}>{r.status === 'completed' ? '成功' : r.status === 'failed' ? '失败' : '运行中'}</Badge></td>
                  <td className="hint">{r.trigger === 'manual' ? '手动' : r.trigger}</td>
                  <td className="hint">{fmtTime(r.startedAt)}</td>
                  <td className="right mono hint">{r.summary?.ms || 0}ms</td>
                  <td className="right mono hint">{r.summary?.completed || 0}/{r.summary?.total || 0}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {!runs.length ? <Empty title="还没有运行记录" /> : null}
        </div>
      </div>

      <div>
        <div className="card">
          <div className="card-header"><div className="card-title">执行过程</div></div>
          <div className="card-body">
            {!activeRun ? <div className="hint">点击左侧「执行」查看每一步的实时结果。</div> : (
              <>
                <div className="flex mb12"><span style={{ fontWeight: 600 }}>{activeRun.name}</span><div className="grow" />
                  <Badge tone={activeRun.status === 'completed' ? 'green' : activeRun.status === 'failed' ? 'red' : 'blue'}>{activeRun.status === 'completed' ? '完成' : activeRun.status === 'failed' ? '失败' : '运行中'}</Badge>
                </div>
                {activeRun.steps.map((s: any, i: number) => (
                  <div key={i} className="wf-step">
                    <div className={'wf-dot ' + (s.status === 'completed' ? 'done' : s.status === 'failed' ? 'failed' : '')}>
                      {s.status === 'completed' ? '✓' : s.status === 'failed' ? '✕' : i + 1}
                    </div>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ fontSize: 12 }}>{s.label}</div>
                      <div className="hint">{s.status === 'pending' ? '等待' : s.status === 'running' ? '执行中…' : (s.ms || 0) + 'ms'}{s.error ? ' · ' + s.error : ''}</div>
                    </div>
                  </div>
                ))}
                {activeRun.summary ? <div className="hint mt8">合计 {activeRun.summary.ms}ms，成功 {activeRun.summary.completed}/{activeRun.summary.total}</div> : null}
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
