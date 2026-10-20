import React, { useCallback, useEffect, useMemo, useState } from 'react';
import {
  api, fmtBytes, fmtTime, KIND_ICON, KIND_LABEL, type AssetContent, type AssetStats,
  type CapabilitySummary
} from '../lib/api';
import { Badge, Empty, Markdown, Modal, Spinner } from '../components/ui';

const KIND_ORDER = ['skill', 'mcp', 'plugin', 'prompt', 'agent'];

/** byKind 兼容 { kind, c }[] 与 { [kind]: count } 两种返回形态 */
function normalizeByKind(byKind: AssetStats['byKind'] | undefined): { kind: string; c: number }[] {
  if (!byKind) return [];
  if (Array.isArray(byKind)) return byKind.map((k) => ({ kind: String(k.kind), c: Number(k.c) || 0 }));
  return Object.keys(byKind).map((k) => ({ kind: k, c: Number((byKind as Record<string, number>)[k]) || 0 }));
}

export default function AssetsView({ toast, onChanged, focusId, onFocusHandled, onOpenHostCapability }: {
  toast: (m: string, t?: string) => void;
  onChanged: () => void;
  focusId?: string | null;
  onFocusHandled?: () => void;
  onOpenHostCapability?: (id: string) => void;
}) {
  const [rows, setRows] = useState<CapabilitySummary[]>([]);
  const [stats, setStats] = useState<AssetStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [q, setQ] = useState('');
  const [kind, setKind] = useState('');
  const [selected, setSelected] = useState<string | null>(null);
  const [content, setContent] = useState<AssetContent | null>(null);
  const [busy, setBusy] = useState(false);

  // 宿主能力 → 能力资产
  const [promoteOpen, setPromoteOpen] = useState(false);
  const [hostRows, setHostRows] = useState<CapabilitySummary[]>([]);
  const [hostQ, setHostQ] = useState('');
  const [hostLoading, setHostLoading] = useState(false);

  // 新建资产
  const [createOpen, setCreateOpen] = useState(false);
  const [newKind, setNewKind] = useState('skill');
  const [newName, setNewName] = useState('');
  const [newSummary, setNewSummary] = useState('');
  const [newContent, setNewContent] = useState('');

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [list, st] = await Promise.all([
        api.assets({ kind: kind || undefined, query: q || undefined, limit: 400 }),
        api.assetStats().catch(() => null)
      ]);
      setRows(list.items || []);
      if (st) setStats(st);
    } catch (e: any) { toast('加载能力资产失败：' + e.message, 'error'); }
    finally { setLoading(false); }
  }, [kind, q, toast]);

  useEffect(() => { const t = setTimeout(load, 180); return () => clearTimeout(t); }, [load]);

  const open = useCallback(async (id: string) => {
    setSelected(id);
    setContent(null);
    try { setContent(await api.asset(id)); }
    catch (e: any) { toast('读取资产内容失败：' + e.message, 'error'); }
  }, [toast]);

  useEffect(() => {
    if (!focusId) return;
    const timer = setTimeout(() => { open(focusId).finally(() => onFocusHandled && onFocusHandled()); }, 150);
    return () => clearTimeout(timer);
  }, [focusId]);

  const byKind = useMemo(() => normalizeByKind(stats?.byKind), [stats]);
  const kindCount = useMemo(() => {
    const map = new Map<string, number>();
    for (const r of rows) map.set(r.kind, (map.get(r.kind) || 0) + 1);
    return map;
  }, [rows]);

  /** 按 kind 分组；保持 KIND_ORDER 的稳定顺序，未知 kind 排在最后 */
  const groups = useMemo(() => {
    const map = new Map<string, CapabilitySummary[]>();
    for (const r of rows) {
      const k = r.kind || 'other';
      if (!map.has(k)) map.set(k, []);
      map.get(k)!.push(r);
    }
    const keys = [...map.keys()].sort((a, b) => {
      const ia = KIND_ORDER.indexOf(a); const ib = KIND_ORDER.indexOf(b);
      return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib);
    });
    return keys.map((k) => ({ kind: k, items: map.get(k)! }));
  }, [rows]);

  const loadHost = async () => {
    setHostLoading(true);
    try { setHostRows(await api.capabilities({ limit: 400 })); }
    catch (e: any) { toast('读取宿主能力库失败：' + e.message, 'error'); }
    finally { setHostLoading(false); }
  };

  const promote = async (cap: CapabilitySummary) => {
    setBusy(true);
    try {
      const r = await api.promoteAsset(cap.id, cap.kind);
      toast('已转为能力资产：' + cap.displayName + ' → ' + r.path, 'success');
      await load(); onChanged();
      if (r.assetId) open(r.assetId);
    } catch (e: any) { toast('转化失败：' + e.message, 'error'); }
    finally { setBusy(false); }
  };

  const doCreate = async () => {
    if (!newName.trim()) { toast('请填写资产名称', 'error'); return; }
    setBusy(true);
    try {
      const r = await api.createAsset({ kind: newKind, name: newName.trim(), content: newContent, summary: newSummary.trim() || undefined });
      toast('已创建能力资产：' + r.path, 'success');
      setCreateOpen(false); setNewName(''); setNewSummary(''); setNewContent('');
      await load(); onChanged();
      open(r.id);
    } catch (e: any) { toast('创建失败：' + e.message, 'error'); }
    finally { setBusy(false); }
  };

  const doRemove = async () => {
    if (!selected) return;
    if (!confirm('将把该能力资产移入回收站（可恢复），确认？')) return;
    setBusy(true);
    try {
      await api.removeAsset(selected);
      toast('已移入回收站', 'success');
      setSelected(null); setContent(null);
      await load(); onChanged();
    } catch (e: any) { toast('删除失败：' + e.message, 'error'); }
    finally { setBusy(false); }
  };

  const total = stats?.total ?? rows.length;
  const hostFiltered = useMemo(() => {
    const ql = hostQ.trim().toLowerCase();
    const list = ql ? hostRows.filter((r) => (r.displayName + ' ' + r.name + ' ' + (r.summary || '')).toLowerCase().includes(ql)) : hostRows;
    return list.slice(0, 120);
  }, [hostRows, hostQ]);

  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', minHeight: 0 }}>
      <div className="grid cols-4 mb16">
        <div className="card stat green"><div className="stat-glow" /><div className="stat-label">能力资产总数</div><div className="stat-value">{total}</div><div className="stat-foot">能力核心自有、可维修、可派发</div></div>
        <div className="card stat blue"><div className="stat-glow" /><div className="stat-label">占用体积</div><div className="stat-value" style={{ fontSize: 20 }}>{fmtBytes(stats?.sizeBytes)}</div><div className="stat-foot">assets/ 下的实体文件</div></div>
        <div className="card stat violet"><div className="stat-glow" /><div className="stat-label">分组类型</div><div className="stat-value">{byKind.filter((k) => k.c > 0).length || groups.length}</div><div className="stat-foot">{byKind.map((k) => (KIND_LABEL[k.kind] || k.kind) + ' ' + k.c).join(' · ') || '暂无分组'}</div></div>
        <div className="card stat amber"><div className="stat-glow" /><div className="stat-label">资产目录</div><div className="stat-value" style={{ fontSize: 13, wordBreak: 'break-all' }}>{stats?.assetDir || '—'}</div><div className="stat-foot"><button className="btn small ghost" style={{ padding: 0 }} onClick={() => stats?.assetDir && api.openPath(stats.assetDir)}>打开目录</button></div></div>
      </div>

      <div className="toolbar">
        <input className="input" style={{ width: 240 }} placeholder="按名称 / 摘要过滤资产…" value={q} onChange={(e) => setQ(e.target.value)} />
        <div className="seg">
          {[['', '全部'] as [string, string], ...KIND_ORDER.map((k) => [k, KIND_LABEL[k] || k] as [string, string])].map(([k, label]) => (
            <button key={k} className={kind === k ? 'active' : ''} onClick={() => setKind(k)}>{label}{k && kindCount.get(k) ? ' ' + kindCount.get(k) : ''}</button>
          ))}
        </div>
        <div className="grow" />
        <button className="btn" onClick={() => { setPromoteOpen(true); setHostQ(''); if (!hostRows.length) loadHost(); }}>⇩ 从宿主能力转为能力资产</button>
        <button className="btn" onClick={() => setCreateOpen(true)}>＋ 新建能力资产</button>
        <button className="btn ghost" onClick={load}>↻</button>
      </div>

      <div className="split" style={{ gridTemplateColumns: 'minmax(0,1fr) 480px' }}>
        <div className="card" style={{ display: 'flex', flexDirection: 'column', minHeight: 0 }}>
          <div className="scroll-y">
            {groups.map((g) => (
              <div key={g.kind}>
                <div className="asset-group-head">
                  <span className="kind-icon">{KIND_ICON[g.kind] || '◆'}</span>
                  <span>{KIND_LABEL[g.kind] || g.kind}</span>
                  <span className="hint">· {g.items.length} 项</span>
                </div>
                <table className="table">
                  <tbody>
                    {g.items.map((r) => (
                      <tr key={r.id} className={selected === r.id ? 'selected' : ''} onClick={() => open(r.id)}>
                        <td style={{ width: 34 }}><span className="kind-icon">{KIND_ICON[r.kind] || '◆'}</span></td>
                        <td>
                          <div style={{ fontWeight: 550, display: 'flex', alignItems: 'center', gap: 6 }}>
                            {r.displayName}
                            {r.version ? <span className="chip">{r.version}</span> : null}
                            {r.scope === 'own' ? <Badge tone="green">能力资产</Badge> : null}
                          </div>
                          <div className="hint" style={{ maxWidth: 520, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{r.summary || '（无摘要）'}</div>
                        </td>
                        <td style={{ width: 110 }}><Badge tone={r.status === 'active' ? 'green' : 'gray'}>{r.status === 'active' ? '已启用' : r.status === 'disabled' ? '已停用' : r.status}</Badge></td>
                        <td className="right hint" style={{ width: 90 }}>{fmtTime(r.updatedAt)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ))}
            {loading ? <div className="empty"><Spinner /> 加载能力资产…</div> : null}
            {!loading && !rows.length ? <Empty icon="▣" title="能力资产库还是空的" hint="用「从宿主能力转为能力资产」把宿主能力库中的能力复制进来，或「新建能力资产」" /> : null}
          </div>
        </div>

        <div className="card" style={{ display: 'flex', flexDirection: 'column', minHeight: 0 }}>
          {!content ? (
            <Empty icon="▣" title="选择左侧能力资产查看详情" hint="可读取资产正文、落盘路径，并移入回收站" />
          ) : (
            <>
              <div className="card-header">
                <span className="kind-icon">{KIND_ICON[content.kind] || '◆'}</span>
                <div className="card-title" style={{ maxWidth: 240, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{content.title}</div>
                <div className="grow" />
                <button className="btn small ghost" title="打开所在目录" onClick={() => api.openPath((content.path || '').replace(/[\\/][^\\/]+$/, ''))}>⤢</button>
                <button className="btn small danger" disabled={busy} onClick={doRemove}>移入回收站</button>
                <button className="btn ghost small" onClick={() => { setContent(null); setSelected(null); }}>✕</button>
              </div>
              <div className="scroll-y" style={{ padding: '12px 14px 14px', flex: 1 }}>
                <div className="flex wrap mb12" style={{ gap: 5 }}>
                  <Badge tone="green">{KIND_LABEL[content.kind] || content.kind}</Badge>
                  <Badge tone="blue">能力资产</Badge>
                  <Badge tone="gray">scope = own</Badge>
                </div>
                <dl className="kv mb12">
                  <dt>资产 ID</dt><dd className="mono">{content.id}</dd>
                  <dt>落盘路径</dt><dd className="mono">{content.path}</dd>
                </dl>
                <div className="section-title">正文内容</div>
                <Markdown text={(content.content || '').slice(0, 60000) || '（该能力资产暂无正文）'} />
              </div>
            </>
          )}
        </div>
      </div>

      {promoteOpen ? (
        <Modal title="从宿主能力库转为能力资产" onClose={() => setPromoteOpen(false)}
          footer={<>
            <span className="hint">转化会把能力实体复制进 assets/，登记来源并保留追溯；宿主原文件不受影响。</span>
            <div className="grow" />
            <button className="btn" onClick={() => setPromoteOpen(false)}>关闭</button>
          </>}>
          <input className="input" style={{ width: '100%' }} placeholder="搜索宿主能力（名称 / 摘要）…" value={hostQ} onChange={(e) => setHostQ(e.target.value)} />
          <div className="mt12" style={{ maxHeight: 420, overflowY: 'auto' }}>
            {hostLoading ? <div className="empty"><Spinner /> 读取宿主能力库…</div> : null}
            {!hostLoading && !hostFiltered.length ? <Empty title="没有可转化的宿主能力" hint="先到「来源与扫描」扫描宿主，或检查过滤词" /> : null}
            {hostFiltered.map((r) => (
              <div key={r.id} className="list-item" style={{ cursor: 'default' }}>
                <div className="title">
                  <span className="kind-icon">{KIND_ICON[r.kind] || '◆'}</span>
                  {r.displayName}
                  <Badge tone="gray">{KIND_LABEL[r.kind] || r.kind}</Badge>
                  {r.scope === 'own' ? <Badge tone="green">已在资产库</Badge> : null}
                  <div className="grow" />
                  <button className="btn small primary" disabled={busy || r.scope === 'own'} onClick={() => promote(r)}>转为能力资产</button>
                  {onOpenHostCapability ? <button className="btn small ghost" onClick={() => onOpenHostCapability(r.id)}>在宿主能力库查看</button> : null}
                </div>
                <div className="meta"><span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{r.summary || '（无摘要）'}</span></div>
              </div>
            ))}
          </div>
        </Modal>
      ) : null}

      {createOpen ? (
        <Modal title="新建能力资产" onClose={() => setCreateOpen(false)}
          footer={<>
            <button className="btn" onClick={() => setCreateOpen(false)}>取消</button>
            <button className="btn primary" disabled={busy} onClick={doCreate}>创建</button>
          </>}>
          <div className="hint mb12">新建的资产直接落在 assets/&lt;kind&gt;/&lt;id&gt;/ 下，可在「生态同步」中写回各宿主原生配置。</div>
          <div className="flex mb12">
            <span className="hint" style={{ width: 64 }}>类型</span>
            <select className="select" value={newKind} onChange={(e) => setNewKind(e.target.value)}>
              {KIND_ORDER.map((k) => <option key={k} value={k}>{KIND_LABEL[k] || k}</option>)}
            </select>
          </div>
          <div className="flex mb12">
            <span className="hint" style={{ width: 64 }}>名称</span>
            <input className="input grow" placeholder="例如：pdf-text-extract" value={newName} onChange={(e) => setNewName(e.target.value)} />
          </div>
          <div className="flex mb12">
            <span className="hint" style={{ width: 64 }}>摘要</span>
            <input className="input grow" placeholder="一句话说明这个能力资产做什么" value={newSummary} onChange={(e) => setNewSummary(e.target.value)} />
          </div>
          <div className="hint mb8">正文（支持 Markdown）</div>
          <textarea className="textarea" style={{ width: '100%', minHeight: 180 }} placeholder="# 能力说明…" value={newContent} onChange={(e) => setNewContent(e.target.value)} />
        </Modal>
      ) : null}
    </div>
  );
}
