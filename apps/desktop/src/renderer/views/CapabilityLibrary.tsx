import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api, fmtBytes, fmtTime, HOST_LABEL, KIND_ICON, KIND_LABEL, type CapabilityDetail, type CapabilitySummary } from '../lib/api';
import { Badge, Empty, Markdown, Modal, Spinner, Tabs, statusTone, healthTone } from '../components/ui';

export default function CapabilityLibrary({ toast, onChanged, focusId, onFocusHandled }: { toast: (m: string, t?: string) => void; onChanged: () => void; focusId?: string | null; onFocusHandled?: () => void }) {
  const [rows, setRows] = useState<CapabilitySummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [q, setQ] = useState('');
  const [kind, setKind] = useState('');
  const [status, setStatus] = useState('');
  const [selected, setSelected] = useState<string | null>(null);
  const [detail, setDetail] = useState<CapabilityDetail | null>(null);
  const [content, setContent] = useState<{ content: string; path: string; title: string } | null>(null);
  const [tab, setTab] = useState('overview');
  const [busy, setBusy] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [importPath, setImportPath] = useState('');
  const [preview, setPreview] = useState<any>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try { setRows(await api.capabilities({ q, kind: kind || undefined, status: status || undefined, limit: 400 })); }
    catch (e: any) { toast('加载失败：' + e.message, 'error'); }
    finally { setLoading(false); }
  }, [q, kind, status, toast]);

  useEffect(() => { const t = setTimeout(load, 180); return () => clearTimeout(t); }, [load]);

  useEffect(() => {
    if (!focusId) return;
    const timer = setTimeout(() => { open(focusId).finally(() => onFocusHandled && onFocusHandled()); }, 150);
    return () => clearTimeout(timer);
  }, [focusId]);

  const open = async (id: string) => {
    setSelected(id);
    setTab('overview');
    setContent(null);
    try {
      const d = await api.capability(id);
      setDetail(d);
      const c = await api.readCapability(id);
      setContent(c);
    } catch (e: any) { toast(e.message, 'error'); }
  };

  const act = async (action: string, value?: any) => {
    if (!selected) return;
    setBusy(true);
    try {
      const r = await api.capabilityAction(selected, action, value);
      toast(actionLabel(action) + '完成' + (r && r.snapshotId ? ' · 快照 ' + r.snapshotId : ''), 'success');
      const d = await api.capability(selected);
      setDetail(d);
      load(); onChanged();
    } catch (e: any) { toast(e.message, 'error'); }
    finally { setBusy(false); }
  };

  const doImport = async (path?: string) => {
    const p = path || importPath;
    if (!p) { toast('请先选择目录或文件', 'error'); return; }
    setBusy(true);
    try {
      if (!preview) { setPreview(await api.previewImport(p)); setBusy(false); return; }
      const r = await api.importCapability(p, { kind: preview.kind });
      toast('已入库：' + r.name + ' → ' + r.dest, 'success');
      setImportOpen(false); setImportPath(''); setPreview(null);
      load(); onChanged();
    } catch (e: any) { toast('导入失败：' + e.message, 'error'); }
    finally { setBusy(false); }
  };

  const pickDir = async () => {
    const paths = await api.pickDirectory();
    if (paths && paths[0]) { setImportPath(paths[0]); setPreview(null); try { setPreview(await api.previewImport(paths[0])); } catch {} }
  };

  const grouped = useMemo(() => {
    const map = new Map<string, number>();
    for (const r of rows) map.set(r.kind, (map.get(r.kind) || 0) + 1);
    return map;
  }, [rows]);

  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', minHeight: 0 }}>
      <div className="toolbar">
        <input className="input" style={{ width: 260 }} placeholder="按名称 / 摘要 / 作者过滤…" value={q} onChange={(e) => setQ(e.target.value)} />
        <div className="seg">
          {[['', '全部'], ['skill', '技能'], ['mcp', 'MCP'], ['plugin', '插件'], ['prompt', '提示词'], ['agent', '智能体']].map(([k, label]) => (
            <button key={k} className={kind === k ? 'active' : ''} onClick={() => setKind(k)}>{label}{k && grouped.get(k) ? ' ' + grouped.get(k) : ''}</button>
          ))}
        </div>
        <select className="select" value={status} onChange={(e) => setStatus(e.target.value)}>
          <option value="">全部状态</option>
          <option value="active">已启用</option>
          <option value="disabled">已停用</option>
          <option value="quarantined">已隔离</option>
          <option value="archived">已归档</option>
        </select>
        <div className="grow" />
        <button className="btn" onClick={() => { setImportOpen(true); setPreview(null); setImportPath(''); }}>＋ 导入能力</button>
        <button className="btn ghost" onClick={load}>↻</button>
      </div>

      <div className="split" style={{ gridTemplateColumns: 'minmax(0,1fr) 480px' }}>
        <div className="card" style={{ display: 'flex', flexDirection: 'column', minHeight: 0 }}>
          <div className="scroll-y">
            <table className="table">
              <thead>
                <tr>
                  <th style={{ width: 34 }}></th>
                  <th>能力</th>
                  <th style={{ width: 80 }}>类型</th>
                  <th style={{ width: 130 }}>作者</th>
                  <th style={{ width: 100 }}>状态</th>
                  <th style={{ width: 90 }}>宿主</th>
                  <th style={{ width: 80 }} className="right">更新</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => (
                  <tr key={r.id} className={selected === r.id ? 'selected' : ''} onClick={() => open(r.id)}>
                    <td><span className="kind-icon">{KIND_ICON[r.kind] || '◆'}</span></td>
                    <td>
                      <div style={{ fontWeight: 550, display: 'flex', alignItems: 'center', gap: 6 }}>
                        {r.displayName}
                        {r.version ? <span className="chip">{r.version}</span> : null}
                        {r.health === 'broken' ? <Badge tone="red">故障</Badge> : r.health === 'warning' ? <Badge tone="amber">警告</Badge> : null}
                        {r.updateStatus === 'outdated' ? <Badge tone="blue">可更新 {r.latestVersion}</Badge> : null}
                      </div>
                      <div className="hint" style={{ maxWidth: 520, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{r.summary || '（无摘要）'}</div>
                    </td>
                    <td><Badge tone={r.kind === 'skill' ? 'blue' : r.kind === 'mcp' ? 'violet' : r.kind === 'plugin' ? 'green' : 'gray'}>{KIND_LABEL[r.kind] || r.kind}</Badge></td>
                    <td className="dim" style={{ maxWidth: 130, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{r.author || '未声明'}</td>
                    <td><Badge tone={statusTone(r.status)}>{r.status === 'active' ? '已启用' : r.status === 'disabled' ? '已停用' : r.status === 'archived' ? '已归档' : r.status}</Badge></td>
                    <td>{(r.hosts || []).slice(0, 2).map((h) => <span key={h} className="chip" style={{ marginRight: 3 }}>{(HOST_LABEL[h] || h).split(' ')[0]}</span>)}</td>
                    <td className="right hint">{fmtTime(r.updatedAt)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {loading ? <div className="empty"><Spinner /> 加载中…</div> : null}
            {!loading && !rows.length ? <Empty title="宿主能力库没有匹配的能力" hint="尝试清除筛选，或到「来源与扫描」新增来源；自有实物请到「能力资产库」查看" /> : null}
          </div>
        </div>

        <div className="card" style={{ display: 'flex', flexDirection: 'column', minHeight: 0 }}>
          {!detail ? (
            <Empty icon="◆" title="选择左侧能力查看详情" hint="可查看作者、版本、安装路径、正文内容、健康状态与更新情况" />
          ) : (
            <>
              <div className="card-header" style={{ flexWrap: 'wrap', gap: 6 }}>
                <span className="kind-icon">{KIND_ICON[detail.kind]}</span>
                <div className="card-title" style={{ maxWidth: 250, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{detail.displayName}</div>
                <div className="grow" />
                <button className="btn small ghost" title="导出" onClick={() => act('export').then(() => {})}>⤓</button>
                <button className="btn ghost small" onClick={() => setDetail(null)}>✕</button>
              </div>
              <div style={{ padding: '8px 12px 0' }}>
                <Tabs tabs={[{ key: 'overview', label: '概览' }, { key: 'content', label: '正文' }, { key: 'install', label: '安装' }, { key: 'faults', label: '故障' }]} active={tab} onChange={setTab} />
              </div>
              <div className="scroll-y" style={{ padding: '0 14px 14px', flex: 1 }}>
                {tab === 'overview' ? (
                  <div>
                    <div className="flex wrap mb12" style={{ gap: 5 }}>
                      <Badge tone={detail.kind === 'skill' ? 'blue' : detail.kind === 'mcp' ? 'violet' : 'green'}>{KIND_LABEL[detail.kind]}</Badge>
                      <Badge tone={statusTone(detail.status)}>{detail.status === 'active' ? '已启用' : detail.status === 'disabled' ? '已停用' : detail.status}</Badge>
                      <Badge tone={healthTone(detail.health)}>{detail.health === 'healthy' ? '健康' : detail.health === 'broken' ? '故障' : detail.health === 'warning' ? '警告' : '未检测'}</Badge>
                      <Badge tone={detail.verifyState === 'verified' ? 'green' : detail.verifyState === 'invalid' ? 'red' : 'gray'}>{detail.verifyState === 'verified' ? '已校验' : detail.verifyState === 'invalid' ? '校验失败' : '未校验'}</Badge>
                      {(detail.protocols || []).map((p) => <span key={p} className="chip">{p}</span>)}
                    </div>
                    <div className="hint mb12" style={{ lineHeight: 1.7 }}>{detail.summary || '（无摘要）'}</div>
                    <dl className="kv">
                      <dt>归属</dt><dd>{detail.scope === 'own' ? '能力资产库（自有，assets/）' : '宿主能力库（扫描登记）'}<span className="faint"> · scope = {detail.scope || 'host'}</span></dd>
                      <dt>作者</dt><dd>{detail.author || '未声明'} <span className="faint">({detail.authorSource || 'unknown'})</span></dd>
                      <dt>版本</dt><dd className="mono">{detail.version || '—'}</dd>
                      <dt>宿主</dt><dd>{(detail.hosts || []).map((h) => HOST_LABEL[h] || h).join('、') || '—'}</dd>
                      <dt>标签</dt><dd>{(detail.tags || []).map((t) => <span key={t} className="chip" style={{ marginRight: 4 }}>{t}</span>)}</dd>
                      <dt>主页</dt><dd>{detail.homepage ? <a style={{ color: '#6dc5ff', cursor: 'pointer' }} onClick={() => api.openExternal(detail.homepage)}>{detail.homepage}</a> : '—'}</dd>
                      <dt>许可证</dt><dd>{detail.license || '—'}</dd>
                      <dt>调用次数</dt><dd className="mono">{detail.usageCount || 0}</dd>
                      <dt>首次发现</dt><dd>{fmtTime(detail.firstSeen)}</dd>
                      <dt>最后更新</dt><dd>{fmtTime(detail.updatedAt)}</dd>
                    </dl>
                    <div className="section-title">操作</div>
                    <div className="flex wrap" style={{ gap: 6 }}>
                      {detail.status === 'active'
                        ? <button className="btn small" disabled={busy} onClick={() => act('disable')}>停用</button>
                        : <button className="btn small primary" disabled={busy} onClick={() => act('activate')}>启用</button>}
                      <button className="btn small" disabled={busy} onClick={() => act('verify')}>校验完整性</button>
                      <button className="btn small" disabled={busy} onClick={() => act('snapshot')}>创建快照</button>
                      <button className="btn small" disabled={busy} onClick={() => act('pin', !detail.pinned)}>{detail.pinned ? '取消固定' : '固定'}</button>
                      {detail.status === 'archived'
                        ? <button className="btn small" disabled={busy} onClick={() => act('restore')}>从回收站恢复</button>
                        : <button className="btn small danger" disabled={busy} onClick={() => { if (confirm('将把能力文件移入回收站（可恢复），确认？')) act('delete'); }}>删除</button>}
                    </div>
                    <div className="mt12">
                      <button className="btn small" onClick={() => api.openPath(detail.installations?.[0]?.path || '')}>打开安装位置</button>
                    </div>
                    {detail.metadata?.entry ? <div className="hint mt12 mono">入口：{detail.metadata.entry}</div> : null}
                  </div>
                ) : null}

                {tab === 'content' ? (
                  content ? (
                    <div>
                      <div className="hint mono mb8">{content.path}</div>
                      <Markdown text={(content.content || '').slice(0, 60000) || '（无正文内容）'} />
                    </div>
                  ) : <div className="empty"><Spinner /></div>
                ) : null}

                {tab === 'install' ? (
                  <div>
                    {(detail.installations || []).map((i) => (
                      <div key={i.id} className="card mb12" style={{ background: '#0d151c' }}>
                        <div className="card-body" style={{ padding: 12 }}>
                          <div className="flex mb8"><span className="chip">{i.host || '未知宿主'}</span><span className="chip">{i.version || '无版本'}</span><div className="grow" /><span className="hint">{fmtBytes(i.size)}</span></div>
                          <div className="mono hint" style={{ wordBreak: 'break-all' }}>{i.path}</div>
                          <div className="flex mt8">
                            <button className="btn small ghost" onClick={() => api.openPath(i.path.split('#')[0])}>打开</button>
                            <span className="hint mono">{i.fingerprint?.slice(0, 16)}</span>
                          </div>
                        </div>
                      </div>
                    ))}
                    {!detail.installations?.length ? <div className="hint">没有安装实例</div> : null}
                  </div>
                ) : null}

                {tab === 'faults' ? (
                  <div>
                    {(detail.faults || []).map((f: any) => (
                      <div key={f.id} className="card mb8" style={{ background: '#0d151c' }}>
                        <div className="card-body" style={{ padding: 12 }}>
                          <div className="flex"><Badge tone={f.severity === 'critical' || f.severity === 'error' ? 'red' : 'amber'}>{f.severity}</Badge><span style={{ fontSize: 12 }}>{f.title}</span></div>
                          <div className="hint mt8">{f.detail}</div>
                        </div>
                      </div>
                    ))}
                    {!detail.faults?.length ? <div className="hint">该能力没有未解决故障</div> : null}
                  </div>
                ) : null}
              </div>
            </>
          )}
        </div>
      </div>

      {importOpen ? (
        <Modal title="导入能力到能力资产库" onClose={() => setImportOpen(false)}
          footer={<>
            <button className="btn" onClick={pickDir}>选择目录…</button>
            <div className="grow" />
            <button className="btn" onClick={() => setImportOpen(false)}>取消</button>
            <button className="btn primary" disabled={busy || !importPath} onClick={() => doImport()}>{preview ? '确认入库' : '预检'}</button>
          </>}>
          <div className="hint mb8">把外部技能目录 / MCP 工程 / 插件复制进能力资产库（assets/），原始位置不变。删除时也只会删除资产库副本，不影响宿主原文件。</div>
          <input className="input" style={{ width: '100%' }} placeholder="粘贴路径，或点击「选择目录…」" value={importPath} onChange={(e) => { setImportPath(e.target.value); setPreview(null); }} />
          {preview ? (
            <div className="card mt12"><div className="card-body">
              <div className="flex mb8"><Badge tone={preview.ok ? 'green' : 'amber'}>{preview.ok ? '预检通过' : '存在警告'}</Badge><span className="chip">{preview.kind}</span></div>
              <dl className="kv">
                <dt>名称</dt><dd>{preview.name}</dd>
                <dt>路径</dt><dd className="mono">{preview.path}</dd>
                <dt>问题</dt><dd>{preview.issues?.length ? preview.issues.join('；') : '无'}</dd>
              </dl>
              {preview.manifest ? <div className="code mt8" style={{ maxHeight: 200 }}>{JSON.stringify({ name: preview.manifest.name, version: preview.manifest.version, author: preview.manifest.author, summary: preview.manifest.summary }, null, 2)}</div> : null}
            </div></div>
          ) : null}
        </Modal>
      ) : null}
    </div>
  );
}

function actionLabel(a: string) {
  const map: Record<string, string> = { activate: '启用', disable: '停用', quarantine: '隔离', delete: '删除', restore: '恢复', snapshot: '快照', export: '导出', verify: '校验', pin: '固定' };
  return map[a] || a;
}
