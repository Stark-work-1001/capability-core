import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import realCatalog from "./fixtures/task-catalog.real.json";
import type { TaskCatalog } from "../panel-types";

/**
 * 「模型分工」的**真实数据**端到端断言。
 *
 * 与 `routing-panel.test.tsx` 的分工：
 * - 那个用最小 mock 数据钉**渲染逻辑**（分组、交叉线、空态文案）；
 * - 这个用**内核真实输出**（fixture 由
 *   `cargo run -p ac-registry --example tasks_probe` 直接导出）钉**契约一致性**：
 *   内核现在给的 25 个节点、六项需求、19/3/3 分布，前端能不能原样呈现。
 *
 * 为什么必须有一条这样的测试：
 * mock 测试全绿只证明「前端自洽」，不证明「前端与内核对得上」。
 * 一旦内核改了字段名或分布，mock 测试仍会绿着 —— 而界面已经空了。
 * 这条测试的数据源是内核产物，改了内核忘了同步 fixture 就会红。
 */

const state: { catalog: TaskCatalog | null; error: string | null } = {
  catalog: null,
  error: null,
};

vi.mock("../bridge", () => ({
  isTauri: false,
  listAssets: vi.fn(async () => ({items: [], total: 0})),
  runtime: "tauri",
  isMissingCommandError: (e: unknown) => /not found|unknown command/i.test(String(e)),
  listCapabilityTasks: vi.fn(async () => {
    if (state.error !== null) throw new Error(state.error);
    return state.catalog;
  }),
}));

const REAL = realCatalog as unknown as TaskCatalog;

beforeEach(() => {
  state.catalog = REAL;
  state.error = null;
});

describe("模型分工 · 用内核真实目录渲染", () => {
  it("fixture 本身是内核真实分布：25 节点 / 19 脚本 / 3 混合 / 3 模型", () => {
    expect(REAL.tasks).toHaveLength(25);
    expect(REAL.requirements).toHaveLength(6);
    expect(REAL.counts.script).toBe(19);
    expect(REAL.counts.hybrid).toBe(3);
    expect(REAL.counts.model).toBe(3);
    expect(REAL.counts.total).toBe(25);
  });

  it("每个节点的 executor 都是三档之一，没有第四种", () => {
    for (const t of REAL.tasks) {
      expect(["script", "hybrid", "model"]).toContain(t.executor);
    }
  });

  it("脚本型节点必须带 script_entry，模型型节点必须带 model_contract", () => {
    for (const t of REAL.tasks) {
      if (t.executor === "script") {
        // script_entry 在类型上是可选的（旧数据可能没有），
        // 但对 script 型节点它是硬要求：没有函数入口的"脚本型"是空话。
        expect(t.script_entry, `${t.id} 是脚本型却没有 script_entry`).toBeTruthy();
        expect(t.script_entry!.length).toBeGreaterThan(0);
      }
      if (t.executor === "model") {
        expect(t.model_contract).toBeTruthy();
        expect(t.model_contract!.why_not_script.length).toBeGreaterThan(10);
      }
    }
  });

  it("模型节点按当前执行入口标记实现状态", () => {
    const models = REAL.tasks.filter((t) => t.executor === "model");
    expect(models).toHaveLength(3);
    for (const m of models) {
      expect(m.status).toBe(m.id === "solution.explain" ? "implemented" : "planned");
    }
  });

  it("六项需求都能映射到至少一个真实存在的节点", () => {
    const ids = new Set(REAL.tasks.map((t) => t.id));
    for (const r of REAL.requirements) {
      expect(r.task_ids.length).toBeGreaterThan(0);
      for (const id of r.task_ids) {
        expect(ids.has(id)).toBe(true);
      }
    }
  });

  it("交叉关系双向自洽：A 声明与 B 交叉时，B 也声明与 A 交叉", () => {
    const byId = new Map(REAL.tasks.map((t) => [t.id, t]));
    for (const t of REAL.tasks) {
      for (const other of t.crosses_with) {
        const o = byId.get(other);
        expect(o, `${t.id} 声明与 ${other} 交叉，但 ${other} 不存在`).toBeTruthy();
        expect(
          o!.crosses_with.includes(t.id),
          `交叉关系不对称：${t.id} → ${other}，但 ${other} 没有反向声明`,
        ).toBe(true);
      }
    }
  });

  it("依赖关系不成环（拓扑可解）", () => {
    const byId = new Map(REAL.tasks.map((t) => [t.id, t]));
    const WHITE = 0;
    const GRAY = 1;
    const BLACK = 2;
    const color = new Map<string, number>();
    for (const t of REAL.tasks) color.set(t.id, WHITE);
    const visit = (id: string): void => {
      if (color.get(id) === GRAY) throw new Error(`依赖成环于 ${id}`);
      if (color.get(id) === BLACK) return;
      color.set(id, GRAY);
      for (const d of byId.get(id)?.depends_on ?? []) {
        if (byId.has(d)) visit(d);
      }
      color.set(id, BLACK);
    };
    for (const t of REAL.tasks) visit(t.id);
  });

  it("界面把真实目录渲染出来：三组齐全且节点数等于数据", async () => {
    const { RoutingPanel } = await import("../panels/RoutingPanel");
    const { container } = render(<RoutingPanel />);
    // 节点 = 分组列表里的 .task-row（每个任务一行）
    await waitFor(() => {
      expect(container.querySelectorAll(".task-row").length).toBe(25);
    });
    // SVG 图里的盒子 = .routing-box（每个任务一个）
    expect(container.querySelectorAll(".routing-box").length).toBe(25);
    // 交叉线：摊平去重后的真实条数
    const edges = new Set<string>();
    for (const t of REAL.tasks) {
      for (const c of t.crosses_with) {
        edges.add(t.id < c ? `${t.id}|${c}` : `${c}|${t.id}`);
      }
    }
    // 交叉线 = .routing-link 的 path（只画真实声明过的交叉）
    expect(container.querySelectorAll(".routing-link").length).toBe(edges.size);
  });

  it("真实目录里六项需求全部渲染出来（req-row 行数 = 6）", async () => {
    const { RoutingPanel } = await import("../panels/RoutingPanel");
    const { container } = render(<RoutingPanel />);
    await waitFor(() => {
      expect(container.querySelectorAll(".req-row").length).toBe(6);
    });
    // 逐条核对需求 id 都出现（r1..r6）
    for (const r of REAL.requirements) {
      expect(container.querySelector(".req-id")?.ownerDocument.body.textContent).toContain(r.id);
    }
  });
});
