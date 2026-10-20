import React, { useEffect, useState } from 'react';
import { api, fmtTime, HOST_LABEL, type SourceRow } from '../lib/api';
import { Badge, Empty, Spinner } from '../components/ui';

export default function SourcesView({ toast, onChanged }: { toast: (m: string, t?: string) => void; onChanged: () => void }) {
  const [rows, setRows] = useState<SourceRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [scanning, setScanning] = useState<string | null>(null);
  const [addOpen, setAddOpen] = useState(false);
  const [form, setForm] = useState({ kind: 'skill', host: 'custom', label: '', rootPath: '', depth: 2, format: 'auto' });

  const load = async () => {
    setLoading(true);
    try { setRows(await api.sources()); } catch (e: any) { toast(e.message, 'error'); }
    finally { setLoading(false); }
  };
  useEffect(() => { load(); }, []);

  const scanOne = async (id: string) => {
    setScanning(id);
    try {
      const r = await api.scan({ sourceId: id });
      toast('扫描完成：' + (r.found || 0) + ' 项', 'success');
      load(); onChanged();
    } catch (e: any) { toast('扫描失败：' + e.message, 'error'); }
    finally { setScanning(null); }
  };

  const toggle = async (id: string, enabled: boolean) => {
    try { await api.toggleSource(id, enabled); load(); }
    catch (e: any) { toast(e.message, 'error'); }
  };

  const remove = async (id: string) => {
    if (!confirm('删除该来源？不会删除磁盘文件。')) return;
    try { await api.removeSource(id); toast('已删除来源', 'success'); load(); }
    catch (e: any) { toast(e.message, 'error'); }
  };

  const pick = async () => {
    const paths = await api.pickDirectory();
    if (paths && paths[0]) setForm({ ...form, rootPath: paths[0], label: form.label || paths[0].split(/[\\/]/).pop() || '' });
  };

  const add = async () => {
    if (!form.rootPath) { toast('请选择目录', 'error'); return; }
    try {
      await api.addSource({ ...form, depth: Number(form.depth) });
      toast('来源已添加', 'success');
      setAddOpen(false); setForm({ kind: 'skill', host: 'custom', label: '', rootPath: '', depth: 2, format: 'auto' });
      load();
    } catch (e: any) { toast(e.message, 'error'); }
  };

  const grouped = rows.reduce((acc: Record<string, SourceRow[]>, r) => {
    (acc[r.host] ||= []).push(r); return acc;
  }, {});

  return (
    <div>
      <div className="toolbar">
        <button className="btn primary" onClick={() => setAddOpen(true)}>＋ 添加来源</button>
        <button className="btn" onClick={load}>↻ 刷新</button>
        <span className="hint">内置来源可停用但不可删除；路径失效会在扫描时报告</span>
        <div className="grow" />
        <span className="hint">{rows.filter((r) => r.enabled).length} / {rows.length} 启用</span>
      </div>

      {loading ? <div className="empty"><Spinner /> 加载来源…</div> : null}

      {Object.entries(grouped).map(([host, list]) => (
        <div key={host} className="card mb16">
          <div className="card-header">
            <div className="card-title">{HOST_LABEL[host] || host}</div>
            <Badge tone="gray">{list.length} 个来源</Badge>
            <div className="grow" />
            <span className="hint">共 {list.reduce((s, r) => s + (r.last_scan_count || 0), 0)} 项能力</span>
          </div>
          <table className="table">
            <thead><tr><th>来源</th><th style={{ width: 70 }}>类型</th><th>根路径</th><th style={{ width: 110 }}>上次扫描</th><th style={{ width: 90 }}>状态</th><th style={{ width: 70 }} className="right">数量</th><th style={{ width: 150 }}></th></tr></thead>
            <tbody>
              {list.map((r) => (
                <tr key={r.id} style={{ cursor: 'default' }}>
                  <td><div style={{ fontWeight: 550 }}>{r.label}</div>{r.builtin ? <span className="hint">内置</span> : null}</td>
                  <td><Badge tone="blue">{r.kind}</Badge></td>
                  <td className="mono hint" style={{ maxWidth: 380, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{r.root_path}</td>
                  <td className="hint">{fmtTime(r.last_scan_at)}</td>
                  <td>
                    {!r.enabled ? <Badge tone="gray">已停用</Badge> :
                      r.last_scan_status === 'ok' ? <Badge tone="green">正常</Badge> :
                      r.last_scan_status === 'warn' ? <Badge tone="amber">有故障</Badge> :
                      r.last_scan_status === 'error' ? <Badge tone="red">错误</Badge> : <Badge tone="gray">未扫描</Badge>}
                  </td>
                  <td className="right mono">{r.last_scan_count || 0}</td>
                  <td className="right">
                    <button className="btn small ghost" disabled={scanning === r.id || !r.enabled} onClick={() => scanOne(r.id)}>{scanning === r.id ? '扫描中' : '扫描'}</button>
                    <button className="btn small ghost" onClick={() => toggle(r.id, !r.enabled)}>{r.enabled ? '停用' : '启用'}</button>
                    {!r.builtin ? <button className="btn small danger" onClick={() => remove(r.id)}>删除</button> : null}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {list.some((r) => r.last_scan_message && r.last_scan_status !== 'ok') ? (
            <div className="card-body hint" style={{ borderTop: '1px solid #182430' }}>
              {list.filter((r) => r.last_scan_message && r.last_scan_status !== 'ok').map((r) => <div key={r.id}>· {r.label}：{r.last_scan_message}</div>)}
            </div>
          ) : null}
        </div>
      ))}

      {!loading && !rows.length ? <div className="card"><Empty title="没有来源" hint="点击「添加来源」登记一个能力目录" /></div> : null}

      {addOpen ? (
        <div className="modal-mask" onClick={() => setAddOpen(false)}>
          <div className="modal" style={{ width: 620 }} onClick={(e) => e.stopPropagation()}>
            <div className="modal-head"><div className="card-title">添加能力来源</div></div>
            <div className="modal-body">
              <div className="flex mb12" style={{ gap: 10 }}>
                <select className="select" value={form.kind} onChange={(e) => setForm({ ...form, kind: e.target.value })}>
                  <option value="skill">技能目录</option><option value="mcp">MCP 配置/工程</option><option value="plugin">插件目录</option>
                  <option value="prompt">提示词目录</option><option value="agent">智能体目录</option>
                </select>
                <select className="select" value={form.host} onChange={(e) => setForm({ ...form, host: e.target.value })}>
                  {Object.entries(HOST_LABEL).map(([k, v]) => <option key={k} value={k}>{v}</option>)}
                </select>
                <input className="input" style={{ width: 90 }} type="number" value={form.depth} onChange={(e) => setForm({ ...form, depth: Number(e.target.value) })} title="扫描深度" />
              </div>
              <input className="input mb12" style={{ width: '100%' }} placeholder="名称（可选）" value={form.label} onChange={(e) => setForm({ ...form, label: e.target.value })} />
              <div className="flex">
                <input className="input grow" placeholder="根路径" value={form.rootPath} onChange={(e) => setForm({ ...form, rootPath: e.target.value })} />
                <button className="btn" onClick={pick}>选择…</button>
              </div>
            </div>
            <div className="modal-foot">
              <button className="btn" onClick={() => setAddOpen(false)}>取消</button>
              <button className="btn primary" onClick={add}>添加</button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
