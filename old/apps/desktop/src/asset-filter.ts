import type { AssetSummary } from "./types";
import {
  ASSET_LIFECYCLES,
  ORIGIN_META,
  REGISTRATION_META,
} from "./types";

/**
 * 资产列表的**本地筛选**。
 *
 * ## 为什么在前端做，而不是加一条新的后端查询
 *
 * 内核的 `list_assets` 只接受 kind / text / tag / min_authority 这几个查询维度
 * （见 apps/desktop/src-tauri/src/lib.rs 的 ListQuery 与 kernel AssetQuery）。
 * 本轮任务明确要求「不新增后端」——而来源 origin_kind、注册方式
 * registration_kind、生命周期 lifecycle、作者 author 这四个字段**已经在
 * AssetSummary 里返回了**（kernel schema v8），只是还没有成为 SQL 过滤条件。
 *
 * 所以这里对**已经加载到内存的列表**做过滤：
 *  - 不额外发请求，点一下立刻出结果；
 *  - 代价是**只覆盖已加载的部分**（AssetKindPanel 每次最多拉 200 条）。
 *
 * 因此界面上必须原样写出「本地筛选（当前已加载 N 条）」：用户要能一眼看出
 * 这不是全库查询。把本地过滤说成全库筛选，是本项目最忌讳的那种假承诺。
 *
 * 等后端哪天把这四个维度下推到 SQL，这里可以整体替换成传参查询，
 * 届时「本地筛选」字样必须同步去掉 —— 两种语义不能共用一套文案。
 */

/** 一条可选项：key 是落库值，label 是界面文字，count 是**当前已加载列表**里的条数。 */
export interface FilterOption {
  key: string;
  label: string;
  count: number;
}

export interface AssetFilterState {
  /** 关键词（名称 / 描述 / id 的包含匹配，大小写不敏感） */
  text: string;
  /** 来源 origin_kind；空串 = 全部 */
  origin: string;
  /** 注册方式 registration_kind；空串 = 全部 */
  registration: string;
  /** 生命周期 lifecycle；空串 = 全部 */
  lifecycle: string;
  /** 作者 author；空串 = 全部 */
  author: string;
}

export const EMPTY_ASSET_FILTER: AssetFilterState = {
  text: "",
  origin: "",
  registration: "",
  lifecycle: "",
  author: "",
};

/**
 * 来源维度：与内核 origin.rs 的判定结果一一对应。
 *
 * 标签直接取 {@link ORIGIN_META} 的 filterLabel，**不在本文件另写一份** ——
 * 行上标签与筛选下拉标签必须是同一套词，两处各写一份迟早漂移
 * （行上写「厂商」、下拉写「厂商随包」还算好，写反了就成了 bug）。
 *
 * 顺序固定为 vendor → self_built → nurtured：这是「从别人那来的」
 * 到「我们自己长的」的语义顺序，也是用户在需求里写的顺序。
 */
export const ORIGIN_FILTER_OPTIONS = ORIGIN_META.map((o) => ({
  key: o.key,
  label: o.filterLabel,
}));

/** 注册方式维度：embedded / mounted / remote（同样复用展示元数据）。 */
export const REGISTRATION_FILTER_OPTIONS = REGISTRATION_META.map((r) => ({
  key: r.key,
  label: r.filterLabel,
}));

/**
 * 生命周期在列表行上的实际取值。
 *
 * 内核建表时 `lifecycle TEXT NOT NULL DEFAULT 'active'`，查询里也写了
 * `COALESCE(a.lifecycle, 'active')`，所以真实数据一定非空。
 * 但老 preview.json / 测试夹具可能没有这个键 —— 那里按 **active** 理解，
 * 与库的默认值一致，而不是把「字段缺失」当成第四种状态。
 */
export function lifecycleOf(it: Pick<AssetSummary, "lifecycle">): string {
  const v = it.lifecycle;
  return typeof v === "string" && v !== "" ? v : "active";
}

/** 是否设了任何筛选条件（关键词也算）。 */
export function isFilterActive(f: AssetFilterState): boolean {
  return (
    f.text.trim() !== "" ||
    f.origin !== "" ||
    f.registration !== "" ||
    f.lifecycle !== "" ||
    f.author !== ""
  );
}

/** 设了几个维度的筛选（不含关键词）；用于界面上的「已筛 N 项」。 */
export function activeFilterCount(f: AssetFilterState): number {
  let n = 0;
  if (f.origin !== "") n += 1;
  if (f.registration !== "") n += 1;
  if (f.lifecycle !== "") n += 1;
  if (f.author !== "") n += 1;
  return n;
}

/**
 * 对**已加载**的列表做筛选。纯函数，便于单测。
 *
 * 空串 = 该维度不筛。作者是精确匹配（下拉里的值来自实际数据，
 * 不是用户手输，因此不需要模糊匹配）。
 */
export function filterAssets(
  items: AssetSummary[],
  f: AssetFilterState,
): AssetSummary[] {
  const q = f.text.trim().toLowerCase();
  return items.filter((it) => {
    if (q) {
      const haystack = (it.name + " " + it.description + " " + it.id).toLowerCase();
      if (!haystack.includes(q)) return false;
    }
    if (f.origin !== "" && (it.origin_kind ?? "") !== f.origin) return false;
    if (f.registration !== "" && (it.registration_kind ?? "") !== f.registration) {
      return false;
    }
    if (f.lifecycle !== "" && lifecycleOf(it) !== f.lifecycle) return false;
    if (f.author !== "" && (it.author ?? "") !== f.author) return false;
    return true;
  });
}

function countOf(
  items: AssetSummary[],
  pick: (it: AssetSummary) => string,
  key: string,
): number {
  let n = 0;
  for (const it of items) if (pick(it) === key) n += 1;
  return n;
}

/**
 * 各维度的下拉选项：**按当前已加载列表里的实际值**生成。
 *
 *  - 来源 / 注册方式 / 生命周期：先给固定的三档（即使 count=0 也保留，
 *    让用户知道这个维度有哪几种取值），再补上数据里出现过的未知值
 *    （后端将来加新值时不会在界面上凭空消失）；
 *  - 作者：完全由数据决定（用户要求「下拉列出实际出现过的作者」），
 *    空作者不是一个选项 —— 判不出来就不该被筛成一个人。
 */
export function buildFilterOptions(items: AssetSummary[]): {
  origins: FilterOption[];
  registrations: FilterOption[];
  lifecycles: FilterOption[];
  authors: FilterOption[];
} {
  const origins: FilterOption[] = ORIGIN_FILTER_OPTIONS.map((o) => ({
    key: o.key,
    label: o.label,
    count: countOf(items, (it) => it.origin_kind ?? "", o.key),
  }));
  const seenOrigin = new Set(origins.map((o) => o.key));
  for (const it of items) {
    const k = it.origin_kind ?? "";
    if (k !== "" && !seenOrigin.has(k)) {
      seenOrigin.add(k);
      origins.push({ key: k, label: k, count: countOf(items, (x) => x.origin_kind ?? "", k) });
    }
  }

  const registrations: FilterOption[] = REGISTRATION_FILTER_OPTIONS.map((o) => ({
    key: o.key,
    label: o.label,
    count: countOf(items, (it) => it.registration_kind ?? "", o.key),
  }));
  const seenReg = new Set(registrations.map((o) => o.key));
  for (const it of items) {
    const k = it.registration_kind ?? "";
    if (k !== "" && !seenReg.has(k)) {
      seenReg.add(k);
      registrations.push({
        key: k,
        label: k,
        count: countOf(items, (x) => x.registration_kind ?? "", k),
      });
    }
  }

  const lifecycles: FilterOption[] = ASSET_LIFECYCLES.map((l) => ({
    key: l.key,
    label: l.short,
    count: countOf(items, (it) => lifecycleOf(it), l.key),
  }));

  const authorCount = new Map<string, number>();
  for (const it of items) {
    const a = (it.author ?? "").trim();
    if (a === "") continue;
    authorCount.set(a, (authorCount.get(a) ?? 0) + 1);
  }
  const authors: FilterOption[] = [...authorCount.entries()]
    .map(([key, count]) => ({ key, label: key, count }))
    .sort((a, b) => b.count - a.count || a.label.localeCompare(b.label));

  return { origins, registrations, lifecycles, authors };
}

/** 当前筛选条件的人话摘要，用于「本地筛选无匹配」时告诉用户到底筛了什么。 */
export function describeFilter(f: AssetFilterState): string {
  const parts: string[] = [];
  if (f.text.trim() !== "") parts.push("关键词=" + f.text.trim());
  if (f.origin !== "") {
    parts.push("来源=" + (ORIGIN_FILTER_OPTIONS.find((o) => o.key === f.origin)?.label ?? f.origin));
  }
  if (f.registration !== "") {
    parts.push(
      "注册方式=" +
        (REGISTRATION_FILTER_OPTIONS.find((o) => o.key === f.registration)?.label ??
          f.registration),
    );
  }
  if (f.lifecycle !== "") {
    parts.push(
      "生命周期=" + (ASSET_LIFECYCLES.find((l) => l.key === f.lifecycle)?.short ?? f.lifecycle),
    );
  }
  if (f.author !== "") parts.push("作者=" + f.author);
  return parts.join(" · ");
}
