import {
  Component,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  capabilityUsage,
  diagnostics,
  getAsset,
  kernelInfo,
  listDedupCandidates,
  openRegistry,
  rescan,
  runtime,
} from "./bridge";
import type {
  AssetSummary,
  CapabilityManifest,
  IngestSummary,
  KernelInfo,
  RegistryStats,
} from "./types";
import { kindColor } from "./types";
import type { UsageRow } from "./panel-types";
import {
  COMBINED_PLUGIN_SUITE_ID,
  SECTIONS,
  assetKindsOf,
  sectionOfSidebar,
  sidebarDef,
  type SectionId,
  type SidebarId,
} from "./nav";
import { DetailPane } from "./components/DetailPane";
import { FleetPanel } from "./panels/FleetPanel";
import { PullPanel } from "./panels/PullPanel";
import { SearchPanel } from "./panels/SearchPanel";
import { AssetKindPanel } from "./panels/AssetKindPanel";
import { FaultPanel } from "./panels/FaultPanel";
import { SolutionsPanel } from "./panels/SolutionsPanel";
import { FixPanel } from "./panels/FixPanel";
import { RoutingPanel } from "./panels/RoutingPanel";
// ModelsPanel（旧的平铺 provider 列表）不再挂在侧栏上：
// 「模型配置」现在是三槽位（ModelSlotsPanel）。旧组件保留文件是因为它里面
// 还有「需要 LLM 求援的故障项」这类内容，后续可以并进维修站。
import { ModelSlotsPanel } from "./panels/ModelSlotsPanel";
import { CreatePanel } from "./panels/CreatePanel";
import { DedupPanel } from "./panels/DedupPanel";
import { HostsPanel } from "./panels/HostsPanel";
import { HostDetail } from "./components/HostDetail";
import type { AgentHost } from "./panel-types";

/**
 * 主界面。
 *
 * 布局参考用户给的 danmaku-frame / LiveControl 侧栏：**一条侧栏**，
 * 分区是侧栏里的分组标题（.nav-label），分区下的侧栏是标题下的可点击行
 * （.nav-item）。右边是内容区 + 属性栏。
 *
 * 分区顺序（本轮按用户要求调整）：
 *   总览 → 资产 → 模型配置 → 新增能力 → 维修站
 * 「维修站分区及其侧栏分组」在「新增能力」下面。
 *
 * 每条 nav-item 右侧的徽标数字都来自真实查询：
 *  - 资产各类 = registry_stats 的 counts（合并栏是 plugin + suite 相加）
 *  - 故障清单 = diagnostics 的 critical + error + warning
 *  - 去重 = list_dedup_candidates 的组数
 * 取不到就不显示徽标（不写 0 冒充）。
 *
 * ## 本轮删掉了什么
 *
 * 用户原话：「能力中枢在界面显示了两遍，保留左上角的那个，圈起来的这个去掉」。
 * 左上角 = `header.topbar` 里的 `.brand`（Capability Core + 能力中枢 · Capability Nexus），
 * 保留；被圈掉的 = 侧栏顶部的 `.sidebar-top`（◈ + 能力中枢 + N 个分区），整块删除。
 * 删掉之后左侧导航从第一个 .nav-section 直接开始，其他布局不动。
 */

type Status =
  | { phase: "connecting" }
  | { phase: "ready"; info: KernelInfo }
  | { phase: "error"; message: string };

/** 侧栏图标：纯字形，不引图标库。 */
const SIDEBAR_ICON: Record<string, string> = {
  fleet: "◎",
  hosts: "◫",
  pull: "⇄",
  search: "⌕",
  faults: "⚑",
  solutions: "☰",
  fixes: "⚒",
  dedup: "⧉",
  routing: "⇉",
  providers: "⚙",
  "new-capability": "＋",
};

const ASSET_ICON = "◈";

export default function App() {
  const [status, setStatus] = useState<Status>({ phase: "connecting" });
  const [sidebar, setSidebar] = useState<SidebarId>("fleet");

  const [stats, setStats] = useState<RegistryStats | null>(null);
  const [summary, setSummary] = useState<IngestSummary | null>(null);
  const [scanning, setScanning] = useState(false);
  const [scanError, setScanError] = useState<string | null>(null);

  // 顶栏全局检索：回车跳进「能力检索」侧栏并把关键词带过去。
  // 搜索本身不在这里做 —— 检索逻辑只有一份，在 SearchPanel 里。
  const [globalQuery, setGlobalQuery] = useState("");
  const [searchSeed, setSearchSeed] = useState("");

  // 总览与维修站共享同一个诊断结果：维修站跑出来的计数喂给总览的紧凑视图与侧栏徽标。
  const [faultCounts, setFaultCounts] = useState<{
    critical: number;
    error: number;
    warning: number;
  } | null>(null);

  // 去重待确认组数：原先读 rescan 摘要的 dedup_pending，本轮由
  // list_dedup_candidates 的实际行数给出（与「去重」侧栏同一数据源）。
  const [dedupPending, setDedupPending] = useState<number | null>(null);

  // 属性栏：选中的资产 id + 完整 manifest（get_asset 拉取）
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selected, setSelected] = useState<CapabilityManifest | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  // 当前选中行在列表里的摘要 —— author / host_support / lifecycle 这些
  // 只在列表行里有（manifest 里没有），属性栏要靠它显示。
  const [selectedSummary, setSelectedSummary] = useState<AssetSummary | null>(null);
  // 来源智能体：capability_usage 按 asset_id 反查，只在首次需要时拉一次
  const [usage, setUsage] = useState<UsageRow[] | null>(null);
  const usageLoading = useRef(false);

  // 协议支持矩阵：选中的智能体宿主。与「资产详情」是**两个互斥的属性栏形态**
  // —— 一个是 manifest，一个是 AgentHost，数据形状完全不同，不能混着渲染。
  // 选中宿主时清空资产选中，反之亦然（见 pickSidebar / openDetail）。
  const [selectedHost, setSelectedHost] = useState<AgentHost | null>(null);

  // 列表刷新信号：属性栏改完生命周期后 +1，让资产列表重拉。
  const [listVersion, setListVersion] = useState(0);

  // 握手 + 首屏扫描
  useEffect(() => {
    let alive = true;
    kernelInfo()
      .then(async (info) => {
        if (!alive) return undefined;
        setStatus({ phase: "ready", info });
        // **先打开注册表**：后端的 open_registry 是显式命令，
        // 不调用它，rescan / listAssets 一律报「数据库未打开」。
        await openRegistry();
        if (!alive) return undefined;
        setScanning(true);
        return rescan();
      })
      .then((r) => {
        if (!alive || !r) return;
        setSummary(r.summary);
        setStats(r.stats);
        setScanError(null);
      })
      .catch((e) => {
        if (!alive) return;
        setScanError(String(e));
      })
      .finally(() => {
        if (alive) setScanning(false);
      });
    return () => {
      alive = false;
    };
  }, []);

  // 诊断只读命令：侧栏徽标（故障数）与总览的紧凑故障卡共用一个数据源。
  useEffect(() => {
    if (status.phase !== "ready") return;
    let alive = true;
    diagnostics()
      .then((r) => {
        if (!alive) return;
        setFaultCounts({ critical: r.critical, error: r.error, warning: r.warning });
      })
      .catch(() => {
        // 诊断失败不写 0：徽标保持不显示
        if (alive) setFaultCounts(null);
      });
    return () => {
      alive = false;
    };
  }, [status.phase]);

  // 去重待确认组数：给「去重」侧栏徽标用，取不到就不显示（不写 0）。
  // 数据源与 DedupPanel 完全一致（list_dedup_candidates），不是另一套统计口径。
  const syncDedupCount = useCallback(async () => {
    try {
      const list = await listDedupCandidates();
      setDedupPending(list.length);
    } catch {
      setDedupPending(null);
    }
  }, []);

  useEffect(() => {
    if (status.phase !== "ready") return;
    void syncDedupCount();
  }, [status.phase, syncDedupCount]);

  const doRescan = useCallback(async () => {
    setScanning(true);
    setScanError(null);
    try {
      const r = await rescan();
      setSummary(r.summary);
      setStats(r.stats);
      // 扫描会重新生成去重候选 —— 徽标跟着更新
      void syncDedupCount();
    } catch (e) {
      setScanError(String(e));
    } finally {
      setScanning(false);
    }
  }, [syncDedupCount]);

  // stats.counts 是内核给的统计数组；缺失时降级成空数组。
  const countOf = (k: string) =>
    (stats?.counts ?? []).find((c) => c.kind === k)?.count ?? 0;

  /** 切换侧栏：同时清空属性栏的选中项，避免显示上一条侧栏的旧资产。 */
  const pickSidebar = useCallback((id: SidebarId) => {
    setSidebar(id);
    setSelectedId(null);
    setSelected(null);
    setSelectedSummary(null);
    // 切侧栏时也清掉宿主选中，否则「协议支持矩阵」的选中行会串到别的侧栏上
    setSelectedHost(null);
  }, []);

  /** 点协议矩阵某一行 → 右侧属性栏切到「宿主详情」。 */
  const selectHost = useCallback((host: AgentHost) => {
    setSelectedHost(host);
    // 宿主详情与资产详情是互斥形态：留着旧的资产选中会让属性栏标题自相矛盾
    setSelectedId(null);
    setSelected(null);
    setSelectedSummary(null);
  }, []);

  const goRepair = useCallback(() => pickSidebar("faults"), [pickSidebar]);
  const goDedup = useCallback(() => pickSidebar("dedup"), [pickSidebar]);

  /** 打开资产详情（资产侧栏与能力检索共用）。 */
  const openDetail = useCallback(
    async (id: string, summary?: AssetSummary) => {
      // 打开资产详情 = 离开宿主详情（两者互斥）
      setSelectedHost(null);
      setSelectedId(id);
      setSelectedSummary(summary ?? null);
      setDetailLoading(true);
      try {
        setSelected(await getAsset(id));
      } catch {
        setSelected(null);
      } finally {
        setDetailLoading(false);
      }
      if (!usageLoading.current) {
        usageLoading.current = true;
        try {
          setUsage(await capabilityUsage(400));
        } catch {
          setUsage([]);
        }
      }
    },
    [],
  );

  const currentSection: SectionId = sectionOfSidebar(sidebar).id;
  const currentDef = sidebarDef(sidebar);
  const assetKinds = assetKindsOf(sidebar);
  const isAssetView = assetKinds.length > 0;

  /** 侧栏徽标：真实数字，没有就返回 null（不显示）。 */
  const badgeOf = (id: SidebarId): number | null => {
    const kinds = assetKindsOf(id);
    if (kinds.length > 0) {
      if (!stats) return null;
      // 合并栏（Plugin / Suite）是两个 kind 的真实计数之和 ——
      // 两个数都来自 registry_stats.counts，不是估算。
      return kinds.reduce((n, k) => n + countOf(k), 0);
    }
    if (id === "faults") {
      if (!faultCounts) return null;
      return faultCounts.critical + faultCounts.error + faultCounts.warning;
    }
    if (id === "dedup") {
      return dedupPending;
    }
    return null;
  };

  /** 来源智能体：按 capability_usage.asset_id 反查，附带最后使用时间。 */
  const sources = useMemo(() => {
    if (!selectedId || !usage) return undefined;
    const rows = usage.filter((u) => u.asset_id === selectedId);
    if (rows.length === 0) return [];
    const m = new Map<
      string,
      { agentId: string; agentName: string; calls: number; lastUsedAt: number }
    >();
    for (const u of rows) {
      const e = m.get(u.agent_id) ?? {
        agentId: u.agent_id,
        agentName: u.agent_name,
        calls: 0,
        lastUsedAt: 0,
      };
      e.calls += u.call_count ?? 0;
      if ((u.last_used_at ?? 0) > e.lastUsedAt) e.lastUsedAt = u.last_used_at ?? 0;
      m.set(u.agent_id, e);
    }
    return [...m.values()].sort((a, b) => b.calls - a.calls);
  }, [selectedId, usage]);

  const info = status.phase === "ready" ? status.info : null;

  return (
    <div className="app">
      <header className="topbar">
        {/* 左上角品牌：**这是界面上唯一一处「能力中枢」**。
            侧栏顶部原来那块重复品牌（.sidebar-top）已整块删除。

            品牌区：**纵向三行**（用户点名）。
            第 1 行 Capability Core / 第 2 行 能力中枢 · Capability Nexus /
            第 3 行 版本徽标（standalone=正式版 STANDALONE / devel=开发版 DEVEL）。
            两版结构完全一致，只有第三行的文字与配色不同。

            原来徽标挤在 h1 右边（.brand-line 横向 flex），名字一长就挤成一团，
            而且 devel / standalone 两版的品牌块高度还不一样，顶栏跟着抖。
            现在改成三条独立基线：logo 竖排在左侧，右侧固定三行，顶栏高度稳定。 */}
        <div className="brand">
          <span className="logo" aria-hidden="true">
            ◈
          </span>
          <div className="brand-text">
            <h1>Capability Core</h1>
            <p className="tagline">能力核心 · 能力中枢</p>
            {/* 第三行**始终占位**（.brand-edition 有最小高度）：
                取不到 edition 时徽标不渲染，但顶栏高度不会因此矮一行 ——
                否则 devel / standalone / 未握手三态下顶栏会抖。 */}
            <div className="brand-edition">
              <EditionBadge info={info} />
            </div>
          </div>
        </div>

        <div className="search-wrap">
          <input
            className="search"
            placeholder="全局检索：回车跳到「能力检索」…"
            value={globalQuery}
            onChange={(e) => setGlobalQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key !== "Enter") return;
              setSearchSeed(globalQuery.trim());
              pickSidebar("search");
            }}
            aria-label="全局检索"
          />
          {globalQuery && (
            <button
              className="clear"
              onClick={() => setGlobalQuery("")}
              aria-label="清空"
            >
              ×
            </button>
          )}
        </div>

        <div className="topbar-crumb">
          <span className="crumb-section">{sectionOfSidebar(sidebar).label}</span>
          <span className="crumb-sep">/</span>
          <span className="crumb-sidebar">{currentDef.label}</span>
          {currentDef.state === "stub" && (
            <span className="crumb-stub" title={currentDef.dataPath}>
              未接后端
            </span>
          )}
        </div>

        <div className="topbar-right">
          <button
            className="rescan"
            onClick={doRescan}
            disabled={scanning || runtime === "browser-preview"}
            title="重新扫描资产目录并同步入库"
          >
            {scanning ? "扫描中…" : "重新扫描"}
          </button>
          <KernelBadge status={status} />
        </div>
      </header>

      <div className="body">
        {/* ── 一条侧栏：分区标题 + 分区下的侧栏行 ──
            注意：这里**不再有** .sidebar-top 品牌块。品牌只在顶栏出现一次。 */}
        <aside className="sidebar nav-rail" aria-label="分区与侧栏导航">
          {SECTIONS.map((section) => (
            <div className="nav-section" key={section.id}>
              <div className="nav-label">
                <span className="nav-label-icon">{section.icon}</span>
                {section.label}
              </div>
              <nav className="nav" aria-label={section.label}>
                {section.sidebars.map((sb) => {
                  const badge = badgeOf(sb.id);
                  const kinds = assetKindsOf(sb.id);
                  const multi = kinds.length > 1;
                  return (
                    <button
                      key={sb.id}
                      className={"nav-item" + (sidebar === sb.id ? " is-active" : "")}
                      onClick={() => pickSidebar(sb.id)}
                      title={sb.hint + " · " + sb.dataPath}
                      aria-current={sidebar === sb.id ? "page" : undefined}
                    >
                      <span
                        className="nav-icon"
                        style={kinds.length > 0 ? { color: kindColor(kinds[0]) } : undefined}
                      >
                        {kinds.length > 0 ? (multi ? "◧" : ASSET_ICON) : SIDEBAR_ICON[sb.id] ?? "•"}
                      </span>
                      <span className="nav-text">{sb.label}</span>
                      {sb.state === "stub" && <span className="nav-dot-stub" />}
                      {badge !== null && <span className="nav-badge">{badge}</span>}
                    </button>
                  );
                })}
              </nav>

              {/* 「库内记录 506」从左下角搬进**资产分区**，作为该分区的汇总行。
                  用户原话：「左下角的库内记录 506 数字显示在资产分区」。
                  放在分区标题与侧栏列表之间，属于资产分区自己的范围。 */}
              {section.id === "assets" && stats !== null && (
                <div className="section-summary" title={stats.db_path}>
                  <span>库内记录</span>
                  <b>{stats.total}</b>
                </div>
              )}
            </div>
          ))}

          <div className="sidebar-footer">
            {scanError !== null && (
              <div className="scan-error compact">{scanError}</div>
            )}
            {/* 「待确认去重」已移到维修站分区的「去重」侧栏承接（见 DedupPanel），
                footer 里不再重复显示。 */}
            <div className="rail-foot">
              {runtime === "browser-preview" ? "预览模式" : "本地内核"}
              {" · "}
              {sectionOfSidebar(sidebar).label}
              {info?.http_port
                ? ` · 端口 ${info.http_port}` +
                  (info.http_listening === false ? "（未监听）" : "")
                : ""}
            </div>
          </div>
        </aside>

        {/* ── 内容区：只渲染当前侧栏 ── */}
        <main className="content">
          <RenderBoundary label={currentDef.label} key={sidebar}>
            {sidebar === "fleet" && (
              <FleetPanel faultCounts={faultCounts} onGoRepair={goRepair} />
            )}
            {sidebar === "pull" && <PullPanel stats={stats} summary={summary} />}
            {sidebar === "search" && (
              <SearchPanel
                initialQuery={searchSeed}
                onSelect={(item) => void openDetail(item.id, item)}
              />
            )}
            {sidebar === "hosts" && (
              <HostsPanel
                onSelect={selectHost}
                selectedId={selectedHost?.id ?? null}
              />
            )}
            {isAssetView && (
              <AssetKindPanel
                kinds={assetKinds}
                onSelect={(item) => {
                  // 列表行把整条摘要交上来：author / host_support / lifecycle
                  // 只有列表行里有（manifest 里没有），属性栏要靠它显示。
                  void openDetail(item.id, item);
                }}
                selectedId={selectedId}
                refreshKey={listVersion}
              />
            )}
            {sidebar === "faults" && <FaultPanel onCounts={setFaultCounts} />}
            {sidebar === "solutions" && <SolutionsPanel onGoDedup={goDedup} />}
            {sidebar === "fixes" && <FixPanel />}
            {sidebar === "dedup" && (
              <DedupPanel onDecided={() => void syncDedupCount()} />
            )}
            {sidebar === "routing" && <RoutingPanel />}
            {/* 「模型配置」侧栏换成三槽位（Embedding / Judge / Rerank）。
                ModelsPanel 保留但不再挂在这里：它的「LLM 接入位」是旧的平铺
                provider 列表，与三槽位是两套模型。旧列表在「模型分工」的
                数据通路说明里还能看到，不会丢。 */}
            {sidebar === "providers" && <ModelSlotsPanel />}
            {sidebar === "new-capability" && <CreatePanel />}
          </RenderBoundary>
        </main>

        {/* ── 右侧属性栏：资产详情 / 当前侧栏的数据通路 ── */}
        <aside className="detail" aria-label="属性栏">
          <h2>
            {selectedHost
              ? "宿主详情"
              : selectedId
                ? "资产属性"
                : "侧栏数据通路"}
          </h2>
          <RenderBoundary
            label="属性栏"
            key={(selectedHost ? "host:" + selectedHost.id : null) ?? selectedId ?? sidebar}
          >
            {selectedHost ? (
              /* 「协议支持矩阵」选中行：证据逐条展示在这里，表格里只留「N 条」摘要 */
              <HostDetail host={selectedHost} />
            ) : selectedId || isAssetView ? (
              <DetailPane
                manifest={selected}
                loading={detailLoading}
                emptyText={`在左侧选中一条 ${currentDef.label} 资产，这里显示它的身份维度、能力与溯源。`}
                summary={selectedSummary}
                sources={sources}
                sourcesNote={
                  usage === null
                    ? "来源智能体：选中后按 capability_usage.asset_id 反查（尚未加载）。"
                    : undefined
                }
                onLifecycleChanged={(state) => {
                  // 切完生命周期要做两件事：
                  //  1. 资产列表重拉 —— 行上的「已暂停 / 已归档」标记要立刻变化；
                  //  2. 选中行的摘要跟着改 —— 属性栏的「当前」与列表行不能对不上。
                  // 注意这里写的是真实的 new state，不是空刷新：
                  // 否则列表说「已暂停」、属性栏还说「使用中」。
                  setListVersion((v) => v + 1);
                  setSelectedSummary((prev) =>
                    prev ? { ...prev, lifecycle: state } : prev,
                  );
                }}
              />
            ) : (
              <SidebarMeta
                label={currentDef.label}
                hint={currentDef.hint}
                dataPath={currentDef.dataPath}
                state={currentDef.state}
                commands={currentDef.commands}
              />
            )}
          </RenderBoundary>
        </aside>
      </div>

      <footer className="statusbar">
        <span className="milestone">工作流 · 版本与审计 · 多智能体接入</span>
        <span className="sep">│</span>
        <span>
          {currentSection === "assets"
            ? sidebar === COMBINED_PLUGIN_SUITE_ID
              ? "资产分区 · Plugin 与 Suite 合并栏"
              : "资产分区"
            : currentDef.state === "live"
              ? "已接真实内核命令"
              : "本轮未接后端"}
        </span>
        {stats && (
          <>
            <span className="sep">│</span>
            <span title="逻辑资产 = 一份内容；位置 = 它的每个磁盘副本">
              {stats.total} 条记录
              {summary && summary.alias_count > 0
                ? ` · ${summary.alias_count} 个重复位置`
                : ""}
            </span>
          </>
        )}
        {stats && (
          <>
            <span className="sep">│</span>
            <span className="path" title={stats.db_path}>
              {stats.db_path}
            </span>
          </>
        )}
      </footer>
    </div>
  );
}

/**
 * 版本标识徽标（顶栏 brand 区，Capability Core 名字右边）。
 *
 * - `devel` → 橙色「开发版 DEVEL」
 * - `standalone` → 青色「正式版 STANDALONE」
 * - 取不到 edition → **不渲染**（不猜版本）
 */
export function EditionBadge({ info }: { info: KernelInfo | null }) {
  const edition = info?.edition ?? null;
  if (edition === null || edition === "") return null;
  if (info?.review_candidate) return <span className="edition-badge devel" title="隔离候选包，审核后才能更新日常使用版">候选版 · 待审核</span>;

  if (edition === "devel") {
    return (
      <span
        className="edition-badge devel"
        title="开发测试版：用户测试改动、审查通过后才进 standalone"
      >
        开发版 DEVEL
      </span>
    );
  }
  if (edition === "standalone") {
    return (
      <span
        className="edition-badge standalone"
        title="免安装运行版：供实际使用"
      >
        正式版 STANDALONE
      </span>
    );
  }
  // 未知值也不猜：原样显示，标成 unknown 样式，避免把陌生值冒充成某个已知版本
  return (
    <span className="edition-badge unknown" title={"后端返回了未识别的 edition：" + edition}>
      {edition}
    </span>
  );
}

/** 属性栏：当前侧栏接的是哪条真实数据通路。 */
function SidebarMeta({
  label,
  hint,
  dataPath,
  state,
  commands,
}: {
  label: string;
  hint: string;
  dataPath: string;
  state: "live" | "stub";
  commands: string[];
}) {
  return (
    <div className="detail-body">
      <div className="detail-title">
        <span className="tag" style={{ background: "#334155" }}>
          {state === "live" ? "已接" : "未接"}
        </span>
        <h3>{label}</h3>
        <p className="muted small">{hint}</p>
      </div>

      <h4>数据通路</h4>
      <p className="detail-desc">{dataPath}</p>

      <h4>涉及命令（真实内核）</h4>
      {commands.length === 0 ? (
        <p className="muted small">无 —— 本轮没有任何后端命令接这一栏。</p>
      ) : (
        <div className="chips">
          {commands.map((c) => (
            <span className="chip" key={c}>
              {c}
            </span>
          ))}
        </div>
      )}

      <h4>读取纪律</h4>
      <p className="muted small">
        这一栏里出现的每个数字都来自上面这些命令的实际返回；
        命令拿不到数据时显示「暂无数据 + 原因」，不填占位值。
      </p>
    </div>
  );
}

/**
 * 渲染错误边界。
 *
 * 归一化已经挡住了已知的数据形状问题，但前端不应该再有"一条坏数据
 * 抹掉整个窗口"这种失败模式：任何一处渲染抛错，降级成一块可见的
 * 错误卡片，其余面板照常可用。
 */
export class RenderBoundary extends Component<
  { label: string; children: ReactNode },
  { error: Error | null }
> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: { componentStack?: string | null }) {
    console.error(
      `[${this.props.label}] 渲染失败，界面已降级显示：`,
      error,
      info?.componentStack ?? "",
    );
  }

  render() {
    if (this.state.error) {
      return (
        <div className="render-error" role="alert">
          <b>⚠ {this.props.label}渲染失败</b>
          <code>{String(this.state.error?.message ?? this.state.error)}</code>
          <small className="muted">其余面板不受影响。</small>
        </div>
      );
    }
    return <>{this.props.children}</>;
  }
}

function KernelBadge({ status }: { status: Status }) {
  if (status.phase === "connecting") {
    return <div className="kernel-badge pending">内核连接中…</div>;
  }
  if (status.phase === "error") {
    return <div className="kernel-badge error">内核异常</div>;
  }
  const { info } = status;
  const preview = runtime === "browser-preview";
  return (
    <div
      className={`kernel-badge ${preview ? "preview" : "ok"}`}
      title={
        preview
          ? "浏览器预览模式，数据为 mock"
          : "Rust 内核握手成功" +
            (info.edition ? ` · edition=${info.edition}` : "") +
            (info.http_port ? ` · http_port=${info.http_port}` : "") +
            (info.http_listening === false ? " · HTTP 服务未监听" : "")
      }
    >
      <span className={preview ? "dot-preview" : "pulse"} />
      <div className="kernel-lines">
        <strong>
          {preview && "预览 · "}
          {info.app_version} · schema {info.schema_version}
        </strong>
        <small>
          {info.rustc_version} · {info.os}/{info.arch}
          {info.http_port
            ? ` · :${info.http_port}` + (info.http_listening === false ? " ✕" : "")
            : ""}
        </small>
      </div>
    </div>
  );
}
