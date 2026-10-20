import React, { useEffect, useState } from 'react';
import { api, fmtTime, KIND_LABEL, type DuplicateGroup } from '../lib/api';
import { Badge, Empty, Spinner } from '../components/ui';

export default function DuplicatesView({ toast, onChanged }: { toast: (m: string, t?: string) => void; onChanged: () => void }) {
  const [groups, setGroups] = useState<DuplicateGroup[]>([]);
  const [decision, setDecision] = useState('pending');
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [expanded, setExpanded] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    try { setGroups(await api.duplicates(decision)); }
    catch (e: any) { toast(e.message, 'error'); }
    finally { setLoading(false); }
  };
  useEffect(() => { load(); }, [decision]);

  const scan = async () => {
    setBusy(true);
    try { const r = await api.scanDuplicates(); toast('识别完成：' + r.groups + ' 组（新增 ' + r.created + '）', 'success'); load(); onChanged(); }
    catch (e: any) { toast(e.message, 'error'); }
    finally { setBusy(false); }
  };

  const decide = async (id: string, action: string, chosenId?: string) => {
    setBusy(true);
    try {
      const r = await api.decideDuplicate(id, action, chosenId);
      toast(action === 'keep_latest' ? '已保留最新版本' : action === 'coexist' ? '已设为共存' : action === 'merge' ? '已合并' : '已忽略', 'success');
      load(); onChanged();
    } catch (e: any) { toast(e.message, 'error'); }
    finally { setBusy(false); }
  };

  return (
    <div>
      <div className="toolbar">
        <button className="btn" onClick={scan} disabled={busy}>{busy ? <><Spinner /> 识别中</> : '⌕ 重新识别重复'}</button>
        <div className="seg">
          {[['pending', '待裁决'], ['keep_latest', '保留最新'], ['merge', '已合并'], ['coexist', '共存'], ['ignore', '已忽略'], ['all', '全部']].map(([k, l]) => (
            <button key={k} className={decision === k ? 'active' : ''} onClick={() => setDecision(k)}>{l}</button>
          ))}
        </div>
        <div className="grow" />
        <span className="hint">判定依据：同身份多实例 / 名称归一相同 / 描述相似度 ≥ 0.86</span>
      </div>

      {loading ? <div className="empty"><Spinner /> 加载中…</div> : null}

      {groups.map((g) => (
        <div key={g.id} className="card mb12">
          <div className="card-header">
            <span className="kind-icon">⧉</span>
            <div className="card-title">{g.canonicalName}</div>
            <Badge tone="blue">{KIND_LABEL[g.kind] || g.kind}</Badge>
            <Badge tone={g.similarity > 0.95 ? 'red' : 'amber'}>相似度 {(g.similarity * 100).toFixed(0)}%</Badge>
            <Badge tone="gray">{g.members.length} 个实例</Badge>
            <span className="hint">{g.reason === 'same-name' ? '同名归一' : '描述高度相似'}</span>
            <div className="grow" />
            {g.decision === 'pending' ? (
              <>
                <button className="btn small primary" disabled={busy} onClick={() => decide(g.id, 'keep_latest', g.recommended)}>保留最新版本</button>
                <button className="btn small" disabled={busy} onClick={() => decide(g.id, 'merge', g.recommended)}>合并</button>
                <button className="btn small ghost" disabled={busy} onClick={() => decide(g.id, 'coexist')}>允许共存</button>
                <button className="btn small ghost" disabled={busy} onClick={() => decide(g.id, 'ignore')}>忽略</button>
              </>
            ) : <Badge tone="green">{g.decision === 'keep_latest' ? '已保留最新' : g.decision === 'merge' ? '已合并' : g.decision === 'coexist' ? '已共存' : '已忽略'}</Badge>}
            <button className="btn small ghost" onClick={() => setExpanded(expanded === g.id ? null : g.id)}>{expanded === g.id ? '收起' : '详情'}</button>
          </div>
          <table className="table">
            <thead><tr><th style={{ width: 40 }}></th><th>实例</th><th style={{ width: 90 }}>版本</th><th style={{ width: 140 }}>作者</th><th>安装路径</th><th style={{ width: 90 }}>状态</th><th style={{ width: 84 }}></th></tr></thead>
            <tbody>
              {g.members.map((m) => (
                <tr key={m.id} style={{ cursor: 'default', background: m.id === g.recommended ? 'rgba(53,208,165,.05)' : undefined }}>
                  <td>{m.id === g.recommended ? <span title="推荐保留" style={{ color: '#35d0a5' }}>★</span> : null}</td>
                  <td><div style={{ fontWeight: 550 }}>{m.displayName}</div><div className="hint">更新于 {fmtTime(m.updatedAt)}</div></td>
                  <td className="mono">{m.version || '—'}</td>
                  <td className="dim">{m.author || '未声明'}</td>
                  <td className="mono hint" style={{ maxWidth: 420, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{m.path}</td>
                  <td><Badge tone={m.status === 'active' ? 'green' : 'gray'}>{m.status === 'active' ? '启用' : m.status === 'archived' ? '归档' : m.status}</Badge></td>
                  <td><button className="btn small ghost" onClick={() => decide(g.id, 'keep_latest', m.id)} disabled={busy || g.decision !== 'pending'}>选它</button></td>
                </tr>
              ))}
            </tbody>
          </table>
          {expanded === g.id ? (
            <div className="card-body hint" style={{ borderTop: '1px solid #182430' }}>
              <div>裁决说明：保留最新版本时，其余实例会被标记为 shadowed 并归档（不删除磁盘文件），可在「宿主能力库」恢复。</div>
              <div className="mt8">组 ID：<span className="mono">{g.id}</span> · 推荐：<span className="mono">{g.recommended}</span></div>
            </div>
          ) : null}
        </div>
      ))}

      {!loading && !groups.length ? <div className="card"><Empty icon="⧉" title={decision === 'pending' ? '没有待裁决的重复项' : '没有匹配记录'} hint="点击「重新识别重复」扫描当前库" /></div> : null}
    </div>
  );
}
