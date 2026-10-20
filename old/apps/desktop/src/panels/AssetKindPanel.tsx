import { useCallback, useEffect, useMemo, useState } from "react";
import { listAssets, runtime } from "../bridge";
import {
  ASSET_KINDS,
  AUTHORITY_META,
  authorSourceMeta,
  kindColor,
  kindLabel,
  lifecycleMeta,
  originMeta,
  registrationMeta,
} from "../types";
import type { AssetSummary } from "../types";
import {
  EMPTY_ASSET_FILTER,
  activeFilterCount,
  buildFilterOptions,
  describeFilter,
  filterAssets,
  isFilterActive,
  type AssetFilterState,
} from "../asset-filter";

/**
 * 行点击回调。
 *
 * 把整条 {@link AssetSummary} 交出去，而不只是 id：`author` /
 * `host_support` / `lifecycle` 这几个字段只有列表行里有，manifest 里没有，
 * 属性栏要靠它显示「作者 / 支持的智能体 / 当前生命周期」。
 * 只传 id 的话属性栏就得再查一次列表 —— 那是第二个数据源。
 */
export type AssetSelectHandler = (item: AssetSummary) => void;

/**
 * 资产分区下各类侧栏的共用列表。
 *
 * 数据通路：`list_assets({ kind })` —— 与资产浏览用的是同一条内核查询，
 * 没有第二套数据源，也没有前端本地过滤出来的「大概」。
 *
 * ## 本轮：支持一次列多个 kind（Plugin + Suite 合并栏）
 *
 * 用户要求「合并 plugin 和 suite 组别」。合并的是**侧栏条目**，
 * 不是数据源：这里对每个 kind 各发一次 `list_assets`，
 * 每次都用后端返回的 total，界面照实显示成「Plugin 20 · Suite 0」。
 *
 * **不做前端假过滤**：不会拉全量再 `filter(i => i.kind === k)` ——
 * 那样 total 会变成「当前这一页里有几条」，和库内真实数量对不上。
 * `kind` 参数是内核 SQL 的过滤条件，total 是 SQL 的 COUNT。
 *
 * ## 列表每行显示的状态
 *
 *  - 权限（只读 / 观察 / 可写 / 破坏性）＝ identity.authority
 *  - 来源（厂商随包 / 中枢自建 / 本机自长）+ 注册方式（自包含 / 挂载 / 远程）
 *  - **作者**（author）—— 判不出来是空串，这一格就不渲染
 *  - 生命周期（激活 / 暂停 / 归档）—— 后端本轮才落这个字段，没有就不显示
 *  - 能力条数、磁盘位置数（>1 = 内容重复，还没合并）
 *  - 版本、来源路径
 */

export interface AssetKindPanelProps {
  /** 要列出的 kind。长度 > 1 = 合并栏（Plugin + Suite）。 */
  kinds: string[];
  onSelect: AssetSelectHandler;
  selectedId: string | null;
  /**
   * 外部刷新信号：属性栏改了生命周期后传一个新的数字，
   * 列表据此重拉，保证「暂停」立刻反映到行上。
   */
  refreshKey?: number;
}

const PAGE = 200;

/** 一个 kind 的加载结果。total 是内核 COUNT，不是本页条数。 */
interface KindBucket {
  kind: string;
  total: number;
  items: AssetSummary[];
  error: string | null;
}

export function AssetKindPanel({
  kinds,
  onSelect,
  selectedId,
  refreshKey = 0,
}: AssetKindPanelProps) {
  const [buckets, setBuckets] = useState<KindBucket[]>([]);
  const [loading, setLoading] = useState(false);
  const [loaded, setLoaded] = useState(false);
  /** 资产筛选状态：关键词 + 来源 / 注册方式 / 生命周期 / 作者四个维度。 */
  const [filter, setFilter] = useState<AssetFilterState>(EMPTY_ASSET_FILTER);

  /** kinds 是数组字面量，每次渲染都是新引用 —— 用字符串做依赖键。 */
  const kindsKey = kinds.join(",");

  const load = useCallback(async () => {
    const list = kindsKey === "" ? [] : kindsKey.split(",");
    setLoading(true);
    try {
      // 每个 kind 一次独立请求（并行）。失败的那一个单独记错，
      // 不连累其他 kind —— 合并栏里 suite 查询失败不该让 plugin 也空着。
      const results = await Promise.all(
        list.map(async (kind): Promise<KindBucket> => {
          try {
            const r = await listAssets({ kind, limit: PAGE, offset: 0 });
            return { kind, total: r.total, items: r.items, error: null };
          } catch (e) {
            return { kind, total: 0, items: [], error: String(e) };
          }
        }),
      );
      setBuckets(results);
    } finally {
      setLoading(false);
      setLoaded(true);
    }
  }, [kindsKey]);

  useEffect(() => {
    setBuckets([]);
    setLoaded(false);
    void load();
  }, [load, refreshKey]);

  const preview = runtime === "browser-preview";
  const total = buckets.reduce((n, b) => n + b.total, 0);
  const allItems = buckets.flatMap((b) => b.items);
  // 「关键词」这一维仍走同一条本地过滤路径：以前只支持它，本轮把它和
  // 来源 / 注册方式 / 生命周期 / 作者四个维度合并成一个筛选器。
  const shown = filterAssets(allItems, filter);
  const filterOn = isFilterActive(filter);
  const filterDims = activeFilterCount(filter);
  // 下拉选项按「已加载列表」的实际值实时生成，不写死一份可能与数据漂移的清单
  const options = useMemo(() => buildFilterOptions(allItems), [allItems]);
  const loadedCount = allItems.length;

  /** 合并栏显示「Plugin 20 · Suite 0」；单类栏只显示名称。 */
  const multi = kinds.length > 1;
  const title = multi
    ? kinds.map((k) => kindLabel(k)).join(" / ")
    : (ASSET_KINDS.find((k) => k.key === kinds[0])?.label ?? kindLabel(kinds[0] ?? ""));
  const tone = kindColor(kinds[0] ?? "");

  const errors = buckets.filter((b) => b.error !== null);

  return (
    <div className="panel-scroll asset-kind-panel">
      <div className="card-head sticky-head">
        <h3>
          <span className="kind-pip" style={{ background: tone }} />
          {title} 资产
        </h3>
        {/* 拆分计数：每个 kind 各自的真实 total，不做任何合并或估算 */}
        <span className="card-num kind-split">
          {loading
            ? "加载中…"
            : buckets.map((b) => kindLabel(b.kind) + " " + b.total).join(" · ") +
              (buckets.length > 1 ? " = " + total + " 项" : " 项")}
        </span>
      </div>

      {multi && (
        <div className="note kind-merged-note">
          「Plugin / Suite」是合并侧栏：下面两段分别来自{" "}
          <code>{'list_assets({ kind: "plugin" })'}</code> 与{" "}
          <code>{'list_assets({ kind: "suite" })'}</code> 两条独立查询，
          各自的 total 由内核 COUNT 给出（Suite 目前库内为 0 是真实结果，不是加载失败）。
        </div>
      )}

      {/* ── 筛选器 ──
          四个维度全部是**前端本地筛选**：内核 list_assets 目前只接受
          kind / text / tag / min_authority，这四个字段只是随行返回、还没有
          成为 SQL 条件。所以这里对「已加载的列表」过滤，并在下面明确写出
          「本地筛选（当前已加载 N 条）」——不让人误以为是全库筛选。 */}
      <div className="asset-filters">
        <div className="asset-filters-row">
          <input
            className="kind-search"
            value={filter.text}
            onChange={(e) => setFilter({ ...filter, text: e.target.value })}
            placeholder={"在 " + title + " 资产内搜索名称 / 描述 / id（本地筛选，不查库）"}
            aria-label="资产关键词过滤"
          />
          <FilterSelect
            label="来源"
            allLabel="全部来源"
            value={filter.origin}
            options={options.origins}
            onChange={(v) => setFilter({ ...filter, origin: v })}
          />
          <FilterSelect
            label="注册方式"
            allLabel="全部注册方式"
            value={filter.registration}
            options={options.registrations}
            onChange={(v) => setFilter({ ...filter, registration: v })}
          />
          <FilterSelect
            label="生命周期"
            allLabel="全部状态"
            value={filter.lifecycle}
            options={options.lifecycles}
            onChange={(v) => setFilter({ ...filter, lifecycle: v })}
          />
          <FilterSelect
            label="作者"
            allLabel="全部作者"
            value={filter.author}
            options={options.authors}
            emptyLabel="无作者数据"
            onChange={(v) => setFilter({ ...filter, author: v })}
          />
          <button
            type="button"
            className="filter-clear"
            disabled={!filterOn}
            onClick={() => setFilter(EMPTY_ASSET_FILTER)}
            title="清空全部筛选条件（关键词 + 四个维度）"
          >
            清空筛选
          </button>
        </div>

        {/* 这句是**承诺**，不是装饰：它说明这里筛的是已加载的 N 条，
            不是全库。后端把这四个维度下推到 SQL 之前，这段话不能删。 */}
        <div className="asset-filters-note">
          <span className="local-filter-badge">本地筛选</span>
          <span className="local-filter-scope">
            （当前已加载 <b>{loadedCount}</b> 条
            {total > loadedCount && <>，库内共 {total} 条未全部加载</>}）
          </span>
          <span className="local-filter-result">
            显示 <b>{shown.length}</b> 条
            {filterDims > 0 && <> · 已筛 {filterDims} 个维度</>}
            {filterOn && <> · {describeFilter(filter)}</>}
          </span>
          <span className="local-filter-why">
            为什么在前端筛：内核 list_assets 只接受 kind / text / tag，
            来源 / 注册方式 / 生命周期 / 作者这几个字段只是随行返回、还没下推到 SQL。
            所以这里筛的是已加载的 {loadedCount} 条，不是全库；
            后端补上维度后应改成传参查询，并把「本地筛选」字样一并去掉。
          </span>
        </div>
      </div>

      {errors.map((b) => (
        <div className="alert error" key={b.kind}>
          {kindLabel(b.kind)} 查询失败：{b.error}
        </div>
      ))}
      {preview && (
        <div className="empty">
          预览模式：没有内核，读不到真实资产。列表保持为空，不填占位行。
        </div>
      )}

      {!preview && loaded && total === 0 && (
        <div className="empty">
          暂无数据：库里没有 {title} 类资产。
          <br />
          原因：该类资产要从磁盘扫描入库；点顶栏「重新扫描」后才会出现。
        </div>
      )}

      {shown.length === 0 && filterOn && total > 0 && (
        <div className="empty">
          本地筛选无匹配：{describeFilter(filter)}
          <br />
          「本地筛选」只在这 {loadedCount} 条已加载记录里找 ——
          不匹配不等于库内没有，清空筛选即可看到全部。
        </div>
      )}

      <ul className="asset-list kind-asset-list">
        {shown.map((it) => {
          const auth = AUTHORITY_META[it.authority];
          const lc = lifecycleMeta(it.lifecycle);
          return (
            <li key={it.id}>
              <button
                className={"asset-row" + (selectedId === it.id ? " active" : "")}
                onClick={() => onSelect(it)}
                title={it.source_path ?? it.id}
              >
                <span className="asset-main">
                  <span className="asset-name">
                    {multi && (
                      <span
                        className="kind-tag"
                        style={{
                          color: kindColor(it.kind),
                          borderColor: kindColor(it.kind) + "55",
                        }}
                        title={"这类资产来自 " + kindLabel(it.kind) + " 查询"}
                      >
                        {kindLabel(it.kind)}
                      </span>
                    )}
                    {it.name}
                  </span>
                  <span className="asset-desc">{it.description || "（无描述）"}</span>
                  {it.source_path && (
                    <span className="asset-src">{it.source_path}</span>
                  )}
                </span>
                <span className="asset-meta">
                  {/* 用户点名的两个维度：来源（谁给的）+ 类型（怎么注册的）。
                      值全部来自内核 origin.rs 的判定结果，判不出来就不显示标签，
                      不写 "unknown" 之类的占位。 */}
                  {originTag(it)}
                  {regTag(it)}
                  {/* 作者：后端判不出来时是空串 / null —— 这一格直接不渲染。
                      行上空间有限，只显示作者名；鼠标悬停给出**出处**
                      （自声明 / frontmatter / 包推断），属性栏里有完整标注。 */}
                  {it.author ? (
                    <span
                      className={
                        "tag-author" +
                        (it.author_source ? " src-" + it.author_source : "")
                      }
                      title={
                        "作者：" + it.author +
                        (authorSourceMeta(it.author_source)
                          ? " · 出处：" + authorSourceMeta(it.author_source)!.label
                          : " · 出处未提供")
                      }
                    >
                      {it.author}
                    </span>
                  ) : null}
                  {/* 生命周期：active 是默认态，不占视觉噪声；
                      只有 paused / archived 才打标，让「被暂停的资产」一眼可见。 */}
                  {lc && it.lifecycle !== "active" && (
                    <span
                      className={"tag-life " + it.lifecycle}
                      style={{
                        color: lc.color,
                        borderColor: lc.color + "55",
                        background: lc.color + "18",
                      }}
                      title={lc.hint}
                    >
                      {lc.short}
                    </span>
                  )}
                  <span
                    className="auth"
                    style={{ color: auth?.color ?? "#94a3b8" }}
                    title={"权限：" + it.authority + " · 工具姿态：" + it.tool_posture}
                  >
                    {it.authority_label || auth?.label || it.authority || "—"}
                  </span>
                  <span className="caps" title="能力数量">
                    ⚙ {it.capability_count ?? 0}
                  </span>
                  {it.version && it.version !== "0" && (
                    <span className="ver">v{it.version}</span>
                  )}
                  {it.location_count !== undefined && it.location_count > 1 && (
                    <span
                      className="dup"
                      title={"同一逻辑资产共有 " + it.location_count + " 个磁盘位置"}
                    >
                      ⧉ {it.location_count}
                    </span>
                  )}
                </span>
              </button>
            </li>
          );
        })}
      </ul>

      {total > allItems.length && (
        <div className="muted small">
          仅显示前 {allItems.length} 项，共 {total} 项
        </div>
      )}

      <div className="muted small origin-legend">
        来源：厂商随包 vendor / 中枢自建 self_built / 本机自长 nurtured
        {" · "}
        注册：自包含 embedded / 挂载 mounted / 远程 remote
        {" · "}
        判不出来时不打标签（不是 unknown）
      </div>
    </div>
  );
}

/**
 * 一个筛选下拉。
 *
 * 用原生 `<select>` 而不是自绘下拉：不引依赖、键盘可达、屏幕阅读器直接可用。
 * 「全部 X」是空值选项（= 不筛这一维），每个具体选项后面带**已加载列表里**
 * 的条数，用户能立刻判断这一档值不值得点。
 */
function FilterSelect({
  label,
  allLabel,
  emptyLabel,
  value,
  options,
  onChange,
}: {
  label: string;
  allLabel: string;
  /** 一个选项都没有时的提示（例如库里没有任何作者） */
  emptyLabel?: string;
  value: string;
  options: { key: string; label: string; count: number }[];
  onChange: (v: string) => void;
}) {
  const empty = options.length === 0;
  return (
    <label className="filter-select">
      <span className="filter-select-label">{label}</span>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        aria-label={"按" + label + "筛选资产"}
        disabled={empty && emptyLabel !== undefined}
        className={value !== "" ? "is-set" : undefined}
      >
        <option value="">{empty && emptyLabel ? emptyLabel : allLabel}</option>
        {options.map((o) => (
          <option value={o.key} key={o.key}>
            {o.label}（{o.count}）
          </option>
        ))}
      </select>
    </label>
  );
}

/** 来源标签。判不出来（空串 / undefined）返回 null，不占位。 */
function originTag(it: AssetSummary) {
  const k = it.origin_kind;
  const meta = originMeta(k);
  if (!k || !meta) return null;
  return (
    <span
      className={"tag-origin " + meta.cls}
      title={
        "来源：" + meta.filterLabel + "（" + k + "）" + " · " + meta.hint +
        (it.origin_agent ? " · 归属宿主：" + it.origin_agent : " · 宿主判不出来")
      }
    >
      {meta.label}
    </span>
  );
}

/** 注册方式标签。mounted / remote 同时标出「不自包含」，因为那正是它们的意思。 */
function regTag(it: AssetSummary) {
  const k = it.registration_kind;
  const meta = registrationMeta(k);
  if (!k || !meta) return null;
  return (
    <span
      className={"tag-reg tag-reg-" + k}
      title={
        "注册方式：" + meta.filterLabel + "（" + k + "）" + " · " + meta.hint +
        (it.self_contained === false ? " · 删掉包外依赖后不可运行" : " · 包内自足")
      }
    >
      {meta.label}
    </span>
  );
}
