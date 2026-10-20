import React, { useEffect, useState } from 'react';
import { api, fmtTime, KIND_LABEL, type UpdateRow } from '../lib/api';
import { Badge, Empty, Spinner, Tabs } from '../components/ui';

export default function UpdatesView({ toast, onChanged }: { toast: (m: string, t?: string) => void; onChanged: () => void }) {
  const [tab, setTab] = useState('installed');
  const [rows, setRows] = useState<UpdateRow[]>([]);
  const [status, setStatus] = useState('all');
  const [loading, setLoading] = useState(true);
  const [checking, setChecking] = useState(false);
  const [progress, setProgress] = useState('');
  const [catalog, setCatalog] = useState<any[]>([]);
  const [catalogCount, setCatalogCount] = useState(0);
  const [catalogQuery, setCatalogQuery] = useState('');
  const [catalogLoading, setCatalogLoading] = useState(false);

  const load = async () => {
    setLoading(true);
    try { setRows(await api.updates(status === 'all' ? undefined : status)); }
    catch (e: any) { toast(e.message, 'error'); }
    finally { setLoading(false); }
  };
  useEffect(() => { load(); }, [status]);
  useEffect(() => { if (tab === 'market') loadCatalog(); }, [tab]);
  useEffect(() => { api.importCatalog().then((r) => setCatalogCount(r.imported || 0)).catch(() => {}); }, []);

  const loadCatalog = async () => {
    setCatalogLoading(true);
    try { setCatalog(await api.catalog(catalogQuery, { limit: 300 })); }
    catch (e: any) { toast(e.message, 'error'); }
    finally { setCatalogLoading(false); }
  };

  const check = async (useNpm: boolean) => {
    setChecking(true);
    setProgress(useNpm ? '正在检查 npm / DSH Market 上游版本…' : '正在检查 DSH Market 缓存…');
    try {
      const r = await api.checkUpdates({ useNpm });
      toast('检查完成：' + r.checked + ' 项，' + r.outdated + ' 个可更新，' + r.failed + ' 个失败', r.failed ? 'error' : 'success');
      load(); onChanged();
    } catch (e: any) { toast(e.message, 'error'); }
    finally { setChecking(false); setProgress(''); }
  };

  const snapshot = async (r: UpdateRow) => {
    try { const res = await api.applyUpdate(r.capabilityId); toast('已创建快照 ' + res.snapshotId + '，可安全更新', 'success'); }
    catch (e: any) { toast(e.message, 'error'); }
  };

  const stats = {
    outdated: rows.filter((r) => r.status === 'outdated').length,
    current: rows.filter((r) => r.status === 'current').length,
    failed: rows.filter((r) => r.status === 'check-failed').length,
    unknown: rows.filter((r) => r.status === 'unknown').length
  };

  return (
    <div>
      <div className="tabs">
        <div className={'tab ' + (tab === 'installed' ? 'active' : '')} onClick={() => setTab('installed')}>已纳管能力</div>
        <div className={'tab ' + (tab === 'market' ? 'active' : '')} onClick={() => setTab('market')}>上游市场{catalogCount ? '（' + catalogCount + '）' : ''}</div>
      </div>

      {tab === 'market' ? (
        <div>
          <div className="toolbar">
            <input className="input" style={{ width: 280 }} placeholder="搜索上游包名…" value={catalogQuery}
              onChange={(e) => setCatalogQuery(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') loadCatalog(); }} />
            <button className="btn" onClick={loadCatalog} disabled={catalogLoading}>{catalogLoading ? <><Spinner /> 查询</> : '搜索'}</button>
            <button className="btn ghost" onClick={async () => { try { const r = await api.importCatalog(); toast('已导入 ' + r.imported + ' 条上游目录', 'success'); loadCatalog(); } catch (e: any) { toast(e.message, 'error'); } }}>重新导入目录</button>
            <div className="grow" />
            <span className="hint">来源：DSH Market 兼容性缓存（{catalog.filter((c) => c.installed).length} 个已安装）</span>
          </div>
          <div className="card">
            <table className="table">
              <thead><tr><th>上游包</th><th style={{ width: 110 }}>最新版本</th><th style={{ width: 150 }}>DSH 版本要求</th><th style={{ width: 90 }}>依赖数</th><th style={{ width: 90 }}>状态</th><th style={{ width: 110 }}>检查时间</th></tr></thead>
              <tbody>
                {catalog.map((c) => (
                  <tr key={c.id} style={{ cursor: 'default' }}>
                    <td><div style={{ fontWeight: 550 }}>{c.packageName}</div><div className="hint">{c.registry}</div></td>
                    <td className="mono">{c.version || '—'}</td>
                    <td className="mono hint">{c.enginesDsh || '不限'}</td>
                    <td className="mono">{c.peerCount}</td>
                    <td>{c.installed ? <Badge tone="green">已安装</Badge> : <Badge tone="gray">未安装</Badge>}</td>
                    <td className="hint">{fmtTime(c.checkedAt)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {!catalog.length && !catalogLoading ? <Empty icon="↻" title="没有目录数据" hint="点击「重新导入目录」，从本机 DSH Market 缓存读取" /> : null}
          </div>
        </div>
      ) : (
    <div>
      <div className="grid cols-4 mb16">
        <div className="card stat amber"><div className="stat-glow" /><div className="stat-label">可更新</div><div className="stat-value">{stats.outdated}</div><div className="stat-foot">上游存在更高版本</div></div>
        <div className="card stat green"><div className="stat-glow" /><div className="stat-label">已是最新</div><div className="stat-value">{stats.current}</div><div className="stat-foot">版本与上游一致</div></div>
        <div className="card stat red"><div className="stat-glow" /><div className="stat-label">检查失败</div><div className="stat-value">{stats.failed}</div><div className="stat-foot">网络/包名无法解析</div></div>
        <div className="card stat violet"><div className="stat-glow" /><div className="stat-label">无上游信息</div><div className="stat-value">{stats.unknown}</div><div className="stat-foot">本地技能，无 registry 记录</div></div>
      </div>

      <div className="toolbar">
        <button className="btn primary" onClick={() => check(true)} disabled={checking}>{checking ? <><Spinner /> 检查中</> : '↻ 检查全部更新'}</button>
        <button className="btn" onClick={() => check(false)} disabled={checking}>仅用本地缓存</button>
        <div className="seg">
          {[['all', '全部'], ['outdated', '可更新'], ['current', '最新'], ['unknown', '无上游信息'], ['check-failed', '失败']].map(([k, l]) => (
            <button key={k} className={status === k ? 'active' : ''} onClick={() => setStatus(k)}>{l}</button>
          ))}
        </div>
        <div className="grow" />
        {progress ? <span className="hint">{progress}</span> : null}
      </div>

      <div className="card">
        {loading ? <div className="empty"><Spinner /> 加载中…</div> : (
          <table className="table">
            <thead><tr><th>能力</th><th style={{ width: 80 }}>类型</th><th style={{ width: 100 }}>当前版本</th><th style={{ width: 100 }}>上游版本</th><th style={{ width: 100 }}>来源</th><th style={{ width: 90 }}>状态</th><th style={{ width: 110 }}>检查时间</th><th style={{ width: 100 }}></th></tr></thead>
            <tbody>
              {rows.map((r) => (
                <tr key={r.id} style={{ cursor: 'default' }}>
                  <td><div style={{ fontWeight: 550 }}>{r.name}</div><div className="hint mono">{r.canonicalName}</div></td>
                  <td><Badge tone="blue">{KIND_LABEL[r.kind] || r.kind}</Badge></td>
                  <td className="mono">{r.currentVersion || '—'}</td>
                  <td className="mono" style={{ color: r.status === 'outdated' ? '#ffc266' : undefined }}>{r.latestVersion || '—'}</td>
                  <td><span className="chip">{r.sourceKind}</span></td>
                  <td><Badge tone={r.status === 'outdated' ? 'amber' : r.status === 'current' ? 'green' : r.status === 'ahead' ? 'blue' : r.status === 'check-failed' ? 'red' : 'gray'}>
                    {r.status === 'outdated' ? '可更新' : r.status === 'current' ? '最新' : r.status === 'ahead' ? '领先' : r.status === 'check-failed' ? '检查失败' : '无上游信息'}
                  </Badge></td>
                  <td className="hint">{fmtTime(r.checkedAt)}</td>
                  <td><button className="btn small ghost" title={r.detail} onClick={() => snapshot(r)}>快照</button></td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {!loading && !rows.length ? <Empty icon="↻" title="还没有检查记录" hint="点击「检查全部更新」，会用本地 DSH Market 缓存 + npm registry 判断版本" /> : null}
      </div>
    </div>
      )}
    </div>
  );
}
