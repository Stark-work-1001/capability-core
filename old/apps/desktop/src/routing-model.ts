/**
 * 模型分工面板的**纯派生**逻辑：分组、交叉去重、连线几何。
 *
 * 为什么单独一个文件：这张图必须真的从内核数据来。
 * 分组与连线的计算抽在这里，就能用最小 mock 数据钉成单测 ——
 * 界面上画的每一条交叉连线都对应一对真实的 crosses_with（内核保证双向对称），
 * 没数据时一根线都不画。
 *
 * 与 fleet-model.ts 的分工：那边画的是「智能体 → 能力」的调用关系，
 * 这里画的是「任务 ↔ 任务」的交叉关系。两套几何互不复用，因为语义不同：
 * 调用是单向的（谁拉了谁），交叉是双向的（内核测试钉住对称）。
 */

import type { CapabilityTask, TaskExecutor } from "./panel-types";

/** 执行者分组的展示顺序：脚本优先。 */
export const EXECUTOR_ORDER: TaskExecutor[] = ["script", "hybrid", "model"];

export const EXECUTOR_LABEL: Record<TaskExecutor, string> = {
  script: "脚本 / 算法",
  hybrid: "混合（脚本 + 模型）",
  model: "模型",
};

/**
 * 执行者配色。**只用于区分，不来自数据**。
 * 绿 = 脚本（本地、确定、无外部依赖）/ 橙 = 模型（需外部调用）/ 蓝 = 混合。
 */
export const EXECUTOR_COLOR: Record<TaskExecutor, string> = {
  script: "#43e0a6",
  hybrid: "#5aa9ff",
  model: "#ff9f43",
};

export const STATUS_LABEL: Record<string, string> = {
  implemented: "已实现",
  partial: "部分实现",
  planned: "计划中",
};

export const STATUS_CLASS: Record<string, string> = {
  implemented: "is-implemented",
  partial: "is-partial",
  planned: "is-planned",
};

export interface ExecutorGroup {
  executor: TaskExecutor;
  label: string;
  color: string;
  tasks: CapabilityTask[];
}

/**
 * 按执行者分组。
 *
 * 返回**固定三组**（脚本 / 混合 / 模型），空组也保留 —— 界面上「模型 0 个」
 * 是一句有信息量的话（说明本轮没有接模型），而少一组会让人以为是漏渲染。
 */
export function groupByExecutor(tasks: CapabilityTask[]): ExecutorGroup[] {
  return EXECUTOR_ORDER.map((executor) => ({
    executor,
    label: EXECUTOR_LABEL[executor],
    color: EXECUTOR_COLOR[executor],
    tasks: tasks.filter((t) => t.executor === executor),
  }));
}

/** 一条交叉关系（已按字典序去重，A < B）。 */
export interface Crossing {
  a: string;
  b: string;
}

/**
 * 把每个节点的 crosses_with 摊平成一组**无向**交叉关系。
 *
 * 内核保证交叉对称，所以 (a,b) 与 (b,a) 会同时出现；这里去重成一条。
 * 若数据里出现单边交叉（不该发生），本函数**不会**替它补另一半 ——
 * 那会掩盖内核的对称性 bug。界面只画真实声明过的方向。
 */
export function crossingsOf(tasks: CapabilityTask[]): Crossing[] {
  const seen = new Set<string>();
  const out: Crossing[] = [];
  for (const t of tasks) {
    for (const other of t.crosses_with ?? []) {
      const [a, b] = t.id < other ? [t.id, other] : [other, t.id];
      const key = a + "|" + b;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({ a, b });
    }
  }
  out.sort((x, y) => (x.a === y.a ? x.b.localeCompare(y.b) : x.a.localeCompare(y.a)));
  return out;
}

/** 一个节点的交叉数（去重后按对方 id 去重，避免同一条被数两次）。 */
export function crossingCount(tasks: CapabilityTask[]): Map<string, number> {
  const m = new Map<string, number>();
  for (const { a, b } of crossingsOf(tasks)) {
    m.set(a, (m.get(a) ?? 0) + 1);
    m.set(b, (m.get(b) ?? 0) + 1);
  }
  return m;
}

// ── 交叉图几何 ────────────────────────────────────────────────
//
// 纯函数：没有任何测量、没有 ResizeObserver。
// 三列（脚本 / 混合 / 模型）× 每列按输入顺序竖排；
// 每个节点的 y 由它在列内的序号直接算出，所以每条连线都能追溯到一对真实交叉。

export interface CrossingBox {
  id: string;
  label: string;
  executor: TaskExecutor;
  /** 盒子左上角 */
  x: number;
  y: number;
  w: number;
  h: number;
  /** 中心点，连线端点用 */
  cx: number;
  cy: number;
}

export interface CrossingLink {
  a: string;
  b: string;
  d: string;
}

export interface CrossingGeometry {
  boxes: CrossingBox[];
  links: CrossingLink[];
  width: number;
  height: number;
  boxWidth: number;
  boxHeight: number;
}

export const CROSSING_GEOMETRY = {
  boxWidth: 156,
  boxHeight: 26,
  rowGap: 8,
  colGap: 96,
  padX: 12,
  padY: 12,
};

/** 节点显示名：太长的截断，避免把盒子撑破（截断只影响标签，不影响数据）。 */
function shortLabel(t: CapabilityTask): string {
  const raw = t.display_name || t.id;
  return raw.length > 12 ? raw.slice(0, 12) + "…" : raw;
}

/**
 * 算出交叉图的盒子与曲线。
 *
 * 连线用三次贝塞尔：从来源盒子**离目标更近的那一侧**出发，
 * 水平切线控制在两列间距的 40%，避免线从盒子底下穿过去。
 */
export function crossingGeometry(
  tasks: CapabilityTask[],
  geo = CROSSING_GEOMETRY,
): CrossingGeometry {
  const groups = groupByExecutor(tasks);
  const boxes: CrossingBox[] = [];
  let maxRows = 0;
  groups.forEach((group, ci) => {
    maxRows = Math.max(maxRows, group.tasks.length);
    group.tasks.forEach((t, ri) => {
      const x = geo.padX + ci * (geo.boxWidth + geo.colGap);
      const y = geo.padY + ri * (geo.boxHeight + geo.rowGap);
      boxes.push({
        id: t.id,
        label: shortLabel(t),
        executor: t.executor,
        x,
        y,
        w: geo.boxWidth,
        h: geo.boxHeight,
        cx: x + geo.boxWidth / 2,
        cy: y + geo.boxHeight / 2,
      });
    });
  });

  const byId = new Map(boxes.map((b) => [b.id, b]));
  const links: CrossingLink[] = [];
  for (const { a, b } of crossingsOf(tasks)) {
    const ba = byId.get(a);
    const bb = byId.get(b);
    // 交叉指向了不在本次数据里的节点（不该发生）：**不画半截线**。
    if (!ba || !bb) continue;
    const forward = ba.cx <= bb.cx;
    const x1 = forward ? ba.x + ba.w : ba.x;
    const x2 = forward ? bb.x : bb.x + bb.w;
    const k = Math.max(18, Math.abs(x2 - x1) * 0.4);
    const dir = forward ? 1 : -1;
    links.push({
      a,
      b,
      d:
        "M " + x1 + " " + ba.cy +
        " C " + (x1 + dir * k) + " " + ba.cy + ", " +
        (x2 - dir * k) + " " + bb.cy + ", " + x2 + " " + bb.cy,
    });
  }

  const columns = groups.filter((g) => g.tasks.length > 0).length;
  // 没有盒子时**宽高就是 0**：留出 padding 会凭空撑出一张空画布，
  // 而空的画布在界面上会被当成「图在但没画出来」。
  const width =
    columns === 0
      ? 0
      : geo.padX * 2 + columns * geo.boxWidth + Math.max(0, columns - 1) * geo.colGap;
  const height =
    maxRows === 0
      ? 0
      : geo.padY * 2 + maxRows * geo.boxHeight + Math.max(0, maxRows - 1) * geo.rowGap;

  return {
    boxes,
    links,
    width,
    height,
    boxWidth: geo.boxWidth,
    boxHeight: geo.boxHeight,
  };
}

/** implementation 的一段摘要：列表里只放一行，完整文本在属性栏。 */
export function implementationBrief(text: string, max = 60): string {
  const t = (text ?? "").replace(/\s+/g, " ").trim();
  return t.length > max ? t.slice(0, max) + "…" : t;
}
