import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api, fmtTime, type EventRow, type StatusInfo } from './lib/api';
import { Toast, Spinner } from './components/ui';
import Overview from './views/Overview';
import AssetsView from './views/AssetsView';
import CapabilityLibrary from './views/CapabilityLibrary';
import SearchLab from './views/SearchLab';
import AgentsView from './views/AgentsView';
import DuplicatesView from './views/DuplicatesView';
import RepairView from './views/RepairView';
import UpdatesView from './views/UpdatesView';
import WorkflowsView from './views/WorkflowsView';
import ModelsView from './views/ModelsView';
import EcosystemView from './views/EcosystemView';
import DatabaseView from './views/DatabaseView';
import SettingsView from './views/SettingsView';
import SourcesView from './views/SourcesView';

type ViewKey = 'overview' | 'assets' | 'capabilities' | 'search' | 'agents' | 'sources' | 'duplicates' | 'repair' | 'updates' | 'workflows' | 'models' | 'ecosystem' | 'database' | 'settings';

// 侧栏结构严格对齐基线 §7（能力中枢 / 治理 / 运行 三组）
const NAV: { group: string; items: { key: ViewKey; label: string; icon: string }[] }[] = [
  {
    group: '能力中枢',
    items: [
      { key: 'overview', label: '总览', icon: '◈' },
      { key: 'assets', label: '能力资产库', icon: '▣' },
      { key: 'capabilities', label: '宿主能力库', icon: '◆' },
      { key: 'search', label: '检索实验室', icon: '⌕' },
      { key: 'agents', label: '智能体接入', icon: '◉' }
    ]
  },
  {
    group: '治理',
    items: [
      { key: 'sources', label: '来源与扫描', icon: '⛁' },
      { key: 'duplicates', label: '去重裁决', icon: '⧉' },
      { key: 'repair', label: '维修站', icon: '⚒' },
      { key: 'updates', label: '更新管理', icon: '↻' }
    ]
  },
  {
    group: '运行',
    items: [
      { key: 'workflows', label: '工作流', icon: '⇉' },
      { key: 'models', label: '模型接入', icon: '◎' },
      { key: 'ecosystem', label: '生态同步', icon: '⇄' },
      { key: 'database', label: '数据库', icon: '▤' },
      { key: 'settings', label: '设置', icon: '⚙' }
    ]
  }
];

const TITLES: Record<ViewKey, { title: string; sub: string }> = {
  overview: { title: '总览', sub: '能力中枢运行状态与最近动态' },
  assets: { title: '能力资产库', sub: '能力核心自有、可维修、可派发的能力资产实体' },
  capabilities: { title: '宿主能力库', sub: '从各宿主扫描登记的技能 / MCP / 插件索引' },
  search: { title: '检索实验室', sub: '测试智能体如何在中枢内寻找能力' },
  agents: { title: '智能体接入', sub: '进程识别、连通检测与主动连通' },
  sources: { title: '来源与扫描', sub: '能力从哪些目录被采集' },
  duplicates: { title: '去重裁决', sub: '识别重复能力并保留最新版本' },
  repair: { title: '维修站', sub: '故障诊断、自动修复与派单' },
  updates: { title: '更新管理', sub: '上游新版本检查与快照更新' },
  workflows: { title: '工作流', sub: '可观察、可复跑的维护流程' },
  models: { title: '模型接入', sub: '提供商、角色路由、调用统计与本地兜底' },
  ecosystem: { title: '生态同步', sub: '把能力资产写回各智能体原生配置' },
  database: { title: '数据库', sub: 'SQLite 运行指标与表结构' },
  settings: { title: '设置', sub: '能力核心行为与连接信息' }
};

export default function App() {
  const [view, setView] = useState<ViewKey>('overview');
  const [focusCapability, setFocusCapability] = useState<string | null>(null);
  const [focusAsset, setFocusAsset] = useState<string | null>(null);
  const contentRef = React.useRef<HTMLDivElement | null>(null);

  // 切换视图时回到顶部，避免沿用上一页的滚动位置
  useEffect(() => {
    if (contentRef.current) contentRef.current.scrollTop = 0;
  }, [view]);
  const [status, setStatus] = useState<StatusInfo | null>(null);
  const [events, setEvents] = useState<EventRow[]>([]);
  const [toasts, setToasts] = useState<{ id: number; text: string; tone: string }[]>([]);
  const [booting, setBooting] = useState(true);
  const [scanning, setScanning] = useState(false);

  const toast = useCallback((text: string, tone = '') => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t, { id, text, tone }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 4200);
  }, []);

  const refreshStatus = useCallback(async () => {
    try {
      const s = await api.status();
      setStatus(s);
      return s;
    } catch (e) { return null; }
  }, []);

  useEffect(() => {
    refreshStatus().finally(() => setBooting(false));
    api.events(60).then(setEvents).catch(() => {});
    const off = api.onEvent((evt: EventRow) => {
      setEvents((prev) => [evt, ...prev].slice(0, 300));
      if (evt.topic === 'scan' && /完成/.test(evt.message)) { setScanning(false); refreshStatus(); }
    });
    const timer = setInterval(refreshStatus, 15000);
    // 首次扫描期间每 2 秒轮询一次，扫描结束立即刷新（避免错过事件）
    let seenBusy = false;
    const bootPoll = setInterval(async () => {
      try {
        const { busy } = await api.scanBusy();
        if (busy) { seenBusy = true; setScanning(true); return; }
        if (seenBusy) { seenBusy = false; setScanning(false); refreshStatus(); clearInterval(bootPoll); }
      } catch { clearInterval(bootPoll); }
    }, 2000);
    setTimeout(() => clearInterval(bootPoll), 180000);
    (window as any).__ccGoto = (v: string) => { if (TITLES[v as ViewKey]) setView(v as ViewKey); };
    (window as any).__ccOpenCapability = (id: string) => { setFocusCapability(id); setView('capabilities'); };
    (window as any).__ccOpenAsset = (id: string) => { setFocusAsset(id); setView('assets'); };
    return () => { off && off(); clearInterval(timer); };
  }, [refreshStatus]);

  const doScan = useCallback(async () => {
    setScanning(true);
    toast('开始扫描全部来源…');
    try {
      const r = await api.scan({});
      toast('扫描完成：发现 ' + r.found + ' 项，新增 ' + r.added + '，更新 ' + r.updated, 'success');
      await refreshStatus();
    } catch (e: any) { toast('扫描失败：' + e.message, 'error'); }
    finally { setScanning(false); }
  }, [toast, refreshStatus]);

  const counts = useMemo(() => {
    const s = status?.stats;
    return { faults: s?.faultsOpen || 0, dups: s?.duplicatesPending || 0, updates: s?.updatesAvailable || 0, agents: s?.agentsRunning || 0 };
  }, [status]);

  const navBadge = (key: ViewKey) => {
    if (key === 'repair' && counts.faults) return <span className="nav-badge alert">{counts.faults}</span>;
    if (key === 'duplicates' && counts.dups) return <span className="nav-badge warn">{counts.dups}</span>;
    if (key === 'updates' && counts.updates) return <span className="nav-badge warn">{counts.updates}</span>;
    if (key === 'agents' && counts.agents) return <span className="nav-badge">{counts.agents}</span>;
    if (key === 'capabilities' && status?.stats?.total) return <span className="nav-badge">{status.stats.total}</span>;
    return null;
  };

  const content = (() => {
    switch (view) {
      case 'overview': return <Overview status={status} events={events} onNavigate={(v) => setView(v as ViewKey)} onRefresh={refreshStatus} toast={toast}
        onOpenCapability={(id) => { setFocusCapability(id); setView('capabilities'); }} />;
      case 'assets': return <AssetsView toast={toast} onChanged={refreshStatus} focusId={focusAsset} onFocusHandled={() => setFocusAsset(null)}
        onOpenHostCapability={(id) => { setFocusCapability(id); setView('capabilities'); }} />;
      case 'capabilities': return <CapabilityLibrary toast={toast} onChanged={refreshStatus} focusId={focusCapability} onFocusHandled={() => setFocusCapability(null)} />;
      case 'search': return <SearchLab toast={toast} onOpen={(id) => { setFocusCapability(id); setView('capabilities'); }} />;
      case 'agents': return <AgentsView toast={toast} onChanged={refreshStatus} />;
      case 'sources': return <SourcesView toast={toast} onChanged={refreshStatus} />;
      case 'duplicates': return <DuplicatesView toast={toast} onChanged={refreshStatus} />;
      case 'repair': return <RepairView toast={toast} onChanged={refreshStatus} />;
      case 'updates': return <UpdatesView toast={toast} onChanged={refreshStatus} />;
      case 'workflows': return <WorkflowsView toast={toast} onChanged={refreshStatus} />;
      case 'models': return <ModelsView toast={toast} onChanged={refreshStatus} />;
      case 'ecosystem': return <EcosystemView toast={toast} />;
      case 'database': return <DatabaseView toast={toast} />;
      case 'settings': return <SettingsView status={status} toast={toast} onChanged={refreshStatus} />;
      default: return null;
    }
  })();

  const t = TITLES[view];

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">
            <div className="brand-orb" />
            <div>
              <div className="brand-title">能力核心</div>
              <div className="brand-sub">Capability Core</div>
            </div>
          </div>
          <div className="brand-status">
            <span className="dot" style={{ background: status?.serverRunning ? '#4ade80' : '#ff6b6b', boxShadow: '0 0 8px ' + (status?.serverRunning ? '#4ade80' : '#ff6b6b') }} />
            {status?.serverRunning ? '中枢服务运行中 · :' + status.port : '中枢服务未启动'}
          </div>
        </div>
        <nav className="nav">
          {NAV.map((g) => (
            <div className="nav-group" key={g.group}>
              <div className="nav-group-title">{g.group}</div>
              {g.items.map((it) => (
                <div key={it.key} className={'nav-item ' + (view === it.key ? 'active' : '')} onClick={() => setView(it.key)}>
                  <span className="nav-icon">{it.icon}</span>
                  <span>{it.label}</span>
                  {navBadge(it.key)}
                </div>
              ))}
            </div>
          ))}
        </nav>
        <div className="sidebar-foot">
          <div className="row"><span>版本</span><span className="mono">v{status?.version || '1.0.0'}</span></div>
          <div className="row"><span>能力数据库</span><span className="mono">{status?.stats?.total || 0} 项</span></div>
          <div className="row"><span>更新</span><span className="mono">{status ? fmtTime(Date.now()) : '—'}</span></div>
        </div>
      </aside>

      <main className="main">
        <header className="topbar">
          <div>
            <h1>{t.title}</h1>
          </div>
          <div className="sub">{t.sub}</div>
          <div className="topbar-spacer" />
          <button className="btn" disabled={scanning} onClick={doScan}>
            {scanning ? <><Spinner /> 扫描中</> : '↻ 扫描全部'}
          </button>
          <button className="btn ghost" onClick={() => refreshStatus()}>刷新</button>
        </header>
        <div className="content" ref={contentRef}>
          {booting ? <div className="empty"><Spinner /> <div className="mt8">正在连接能力中枢…</div></div> : content}
        </div>
      </main>

      <Toast items={toasts} onClose={(id) => setToasts((t) => t.filter((x) => x.id !== id))} />
    </div>
  );
}
