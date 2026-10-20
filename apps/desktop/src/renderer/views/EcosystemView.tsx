import React, { useEffect, useState } from 'react';
import { api, fmtTime, HOST_LABEL } from '../lib/api';
import { Badge, Empty, Spinner } from '../components/ui';

export default function EcosystemView({ toast }: { toast: (m: string, t?: string) => void }) {
  const [targets, setTargets] = useState<any[]>([]);
  const [backups, setBackups] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);
  const [previewHost, setPreviewHost] = useState('');
  const [previewText, setPreviewText] = useState('');

  const load = async () => {
    setLoading(true);
    try { setTargets(await api.ecosystemTargets()); setBackups(await api.backups()); }
    catch (e: any) { toast(e.message, 'error'); }
    finally { setLoading(false); }
  };
  useEffect(() => { load(); }, []);

  const preview = async (host: string) => {
    try { const r = await api.previewSync(host); setPreviewHost(host); setPreviewText(r.text); }
    catch (e: any) { toast(e.message, 'error'); }
  };
  const apply = async (host: string) => {
    if (!confirm('将写入 ' + host + ' 的 MCP 配置（写入前自动备份），确认？')) return;
    try { const r = await api.applySync(host); toast('已写入 ' + r.file, 'success'); load(); }
    catch (e: any) { toast(e.message, 'error'); }
  };
  const restore = async (id: string) => {
    if (!confirm('回滚该备份？')) return;
    try { const r = await api.restoreBackup(id); toast('已恢复 ' + r.path, 'success'); }
    catch (e: any) { toast(e.message, 'error'); }
  };

  return (
    <div>
      <div className="hero mb16">
        <div className="hero-title">把能力资产写回每个智能体</div>
        <div className="hero-sub">
          能力核心不是只读索引。选中目标后可以一键把 capability-core 的 MCP 端点写入该宿主的原生配置，
          写入前自动生成可回滚备份；宿主侧重启后即可让该智能体主动检索能力资产与宿主能力。
        </div>
      </div>

      {loading ? <div className="empty"><Spinner /> 读取目标…</div> : null}

      <div className="grid cols-2">
        <div className="card">
          <div className="card-header"><div className="card-title">可同步目标</div><div className="grow" /><span className="hint">{targets.length} 个</span></div>
          <table className="table">
            <thead><tr><th>宿主</th><th>配置文件</th><th style={{ width: 80 }}>格式</th><th style={{ width: 140 }}></th></tr></thead>
            <tbody>
              {targets.map((t) => (
                <tr key={t.file} style={{ cursor: 'default' }}>
                  <td style={{ width: 190 }}><div style={{ fontWeight: 550 }}>{HOST_LABEL[t.host] || t.host}</div><div className="hint" style={{ whiteSpace: 'nowrap' }}>{t.label}</div></td>
                  <td className="mono hint" style={{ maxWidth: 300, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{t.file}</td>
                  <td><span className="chip">{t.format}</span></td>
                  <td className="right">
                    <button className="btn small ghost" onClick={() => preview(t.host)}>预览</button>
                    {t.writeable ? <button className="btn small primary" onClick={() => apply(t.host)}>写入</button> : <Badge tone="gray">只读</Badge>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {!loading && !targets.length ? <Empty title="没有可写目标" hint="启动对应智能体后会自动出现" /> : null}
          {previewText ? (
            <div className="card-body" style={{ borderTop: '1px solid #182430' }}>
              <div className="hint mb8">预览（{previewHost}）：</div>
              <div className="code" style={{ maxHeight: 200 }}>{previewText}</div>
            </div>
          ) : null}
        </div>

        <div className="card">
          <div className="card-header"><div className="card-title">备份与回滚</div><div className="grow" /><span className="hint">{backups.length}</span></div>
          <div className="card-body" style={{ padding: 0, maxHeight: 480, overflowY: 'auto' }}>
            {backups.map((b) => (
              <div key={b.id} className="list-item" style={{ cursor: 'default' }}>
                <div className="title">{b.reason || '备份'} <div className="grow" /><span className="hint">{fmtTime(b.createdAt)}</span></div>
                <div className="meta" style={{ wordBreak: 'break-all' }}><span className="mono">{b.path}</span></div>
                <div className="meta"><span>{b.size} 字节</span><button className="btn small ghost" onClick={() => restore(b.id)}>回滚</button></div>
              </div>
            ))}
            {!backups.length ? <Empty title="还没有备份" hint="写入或自动修复时会自动创建" /> : null}
          </div>
        </div>
      </div>
    </div>
  );
}
