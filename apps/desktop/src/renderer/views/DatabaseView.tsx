import React, { useEffect, useState } from 'react';
import { api, fmtBytes } from '../lib/api';
import { Spinner } from '../components/ui';

export default function DatabaseView({ toast }: { toast: (m: string, t?: string) => void }) {
  const [info, setInfo] = useState<any>(null);
  const [loading, setLoading] = useState(true);

  const load = async () => {
    setLoading(true);
    try { setInfo(await api.dbStats()); }
    catch (e: any) { toast(e.message, 'error'); }
    finally { setLoading(false); }
  };
  useEffect(() => { load(); }, []);
  useEffect(() => { const t = setInterval(load, 10000); return () => clearInterval(t); }, []);

  const db = info?.db || {};
  const max = Math.max(1, ...((info?.tables || []).map((x: any) => x.rows)));
  return (
    <div>
      <div className="grid cols-4 mb16">
        <div className="card stat green"><div className="stat-glow" /><div className="stat-label">数据库大小</div><div className="stat-value" style={{ fontSize: 20 }}>{fmtBytes(db.dbBytes)}</div><div className="stat-foot">WAL {fmtBytes(db.walBytes)}</div></div>
        <div className="card stat blue"><div className="stat-glow" /><div className="stat-label">能力 / 安装</div><div className="stat-value" style={{ fontSize: 20 }}>{db.capabilities || 0} / {db.installations || 0}</div><div className="stat-foot">逻辑能力 / 物理实例</div></div>
        <div className="card stat amber"><div className="stat-glow" /><div className="stat-label">事件 / 智能体</div><div className="stat-value" style={{ fontSize: 20 }}>{db.events || 0} / {db.agents || 0}</div><div className="stat-foot">审计追踪</div></div>
        <div className="card stat violet"><div className="stat-glow" /><div className="stat-label">SQLite</div><div className="stat-value" style={{ fontSize: 20 }}>{db.sqliteVersion || '—'}</div><div className="stat-foot">FTS5 · JSON1</div></div>
      </div>

      <div className="toolbar">
        <span className="hint mono">{info?.file}</span>
        <div className="grow" />
        <button className="btn ghost" onClick={load}>↻ 刷新</button>
        <button className="btn" onClick={() => api.openPath((info?.file || '').replace(/[\\/][^\\/]+$/, ''))}>打开所在目录</button>
      </div>

      <div className="grid cols-2">
        <div className="card">
          <div className="card-header"><div className="card-title">数据表</div><div className="grow" /><span className="hint">行数</span></div>
          <div className="card-body" style={{ padding: 0, maxHeight: 600, overflowY: 'auto' }}>
            {loading && !info ? <div className="empty"><Spinner /></div> : (
              <table className="table">
                <thead><tr><th>表名</th><th style={{ width: 80 }} className="right">行数</th><th style={{ width: 140 }}>占比</th></tr></thead>
                <tbody>
                  {(info?.tables || []).map((t: any) => (
                    <tr key={t.name} style={{ cursor: 'default' }}>
                      <td className="mono" style={{ fontSize: 11.5 }}>{t.name}</td>
                      <td className="right mono">{t.rows}</td>
                      <td><div className="progress-line"><span style={{ width: Math.max(2, (t.rows / max) * 100) + '%' }} /></div></td>
                    </tr>
                  ))}
                </tbody>
              </table>)}
          </div>
        </div>

        <div className="card">
          <div className="card-header"><div className="card-title">存储结构</div></div>
          <div className="card-body">
            <div className="hint" style={{ lineHeight: 1.95 }}>
              <b>双轨存储</b>：元数据、状态与修复记录在 SQLite；能力资产实体保存在 assets/ 下的分散目录
              （skill/、mcp/、plugin/、prompt/），宿主能力则以索引方式登记。<br /><br />
              <b>能力三分</b>：capabilities.scope 区分 own（能力资产库，自有）与 host（宿主能力库，扫描登记），
              两者共用同一张表与同一套检索、维修、派发链路。<br /><br />
              <b>逻辑能力 vs 安装实例</b>：capabilities 表一行代表一个逻辑能力（按身份键归一），
              installations 表记录它在每个宿主与路径下的物理副本，便于去重与定位。<br /><br />
              <b>检索</b>：search_fts（unicode61，CJK 逐字切分）+ search_trigram（子串）+ 结构化 LIKE，三路融合打分。<br /><br />
              <b>审计</b>：events 记录全部扫描/修复/检索事件，repairs 与 backup_snapshots 保存补丁与可回滚备份。
            </div>
            <div className="section-title">直接查询</div>
            <div className="code" style={{ fontSize: 11 }}>{'sqlite3 "' + (info?.file || '') + '"\n\nSELECT kind, COUNT(*) FROM capabilities GROUP BY kind;\nSELECT * FROM faults WHERE status != \'resolved\';'}</div>
          </div>
        </div>
      </div>
    </div>
  );
}
