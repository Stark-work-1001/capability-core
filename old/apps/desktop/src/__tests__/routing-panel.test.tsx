import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import type { CapabilityTask, TaskCatalog } from "../panel-types";

/**
 * 「模型分工」侧栏的回归网。
 *
 * 这一栏以前写的是「分派未接」的空态。本轮它接上了真实命令
 * list_capability_tasks（内核 ac_registry::tasks 静态目录），
 * 所以测试要钉住四件事：
 *
 * 1. **有数据才画图**：节点数、交叉线条数都来自数据，不多画一条；
 * 2. **没数据明说原因**：命令缺失 / 调用失败 / 预览模式，三种原因各不相同的文案，
 *    且一个节点都不画 —— 绝不内置示例路由表；
 * 3. **状态三档照实显示**：model 型节点显示「计划中」，不许渲染成已接线；
 * 4. **交叉线只画真实声明过的**：crosses_with 摊平去重后的条数 = 曲线条数。
 *
 * bridge 全量打桩：本测试关心的是「拿到数据后渲染成什么」与「拿不到时怎么说话」。
 */

const state: {
  catalog: TaskCatalog | null;
  error: string | null;
} = { catalog: null, error: null };

vi.mock("../bridge", () => ({
  isTauri: false,
  listAssets: vi.fn(async () => ({items: [], total: 0})),
  runtime: "tauri",
  isMissingCommandError: (e: unknown) =>
    /not found|unknown command/i.test(String(e)),
  listCapabilityTasks: vi.fn(async () => {
    if (state.error !== null) throw new Error(state.error);
    return state.catalog;
  }),
}));

function task(over: Partial<CapabilityTask> = {}): CapabilityTask {
  return {
    id: "capability.index",
    display_name: "能力索引",
    description: "扫描入库",
    executor: "script",
    implementation: "ingest::ingest(registry, root)",
    status: "implemented",
    depends_on: [],
    crosses_with: [],
    inputs: ["扫描根目录"],
    outputs: ["IngestReport"],
    evidence: ["确定性流水线"],
    script_entry: ["ac_registry::ingest::ingest"],
    ...over,
  };
}

function catalogOf(tasks: CapabilityTask[]): TaskCatalog {
  const count = (f: (t: CapabilityTask) => boolean) => tasks.filter(f).length;
  return {
    tasks,
    requirements: [
      {
        id: "r1",
        title: "接入 · 识别 · 索引 · 调用",
        statement: "对话智能体能够通过相应插件与能力中枢连接，来识别、索引、调用相关能力",
        task_ids: tasks.map((t) => t.id),
        note: "测试数据",
      },
    ],
    counts: {
      total: tasks.length,
      script: count((t) => t.executor === "script"),
      model: count((t) => t.executor === "model"),
      hybrid: count((t) => t.executor === "hybrid"),
      implemented: count((t) => t.status === "implemented"),
      partial: count((t) => t.status === "partial"),
      planned: count((t) => t.status === "planned"),
    },
    script_share: tasks.length === 0 ? 0 : count((t) => t.executor === "script") / tasks.length,
    note: "静态目录，不读库",
  };
}

beforeEach(() => {
  state.catalog = null;
  state.error = null;
});

describe("模型分工 · 有数据时按执行者分组并画真实交叉", () => {
  it("三组都出现（空组也留），节点数与计数一致", async () => {
    state.catalog = catalogOf([
      task({ id: "capability.index", display_name: "能力索引" }),
      task({
        id: "capability.create",
        display_name: "新增能力",
        executor: "hybrid",
        status: "partial",
        implementation: "asset_store::install_skill + 模型生成清单",
        script_entry: ["ac_registry::asset_store::install_skill"],
        model_contract: {
          capabilities: ["生成 Markdown"],
          input_schema: "{requirement}",
          output_schema: "{files:[]}",
          why_not_script: "模板填出来的东西解析能过、用起来是空壳，所以必须由模型生成正文。",
        },
      }),
      task({
        id: "solution.explain",
        display_name: "故障解释",
        executor: "model",
        status: "planned",
        implementation: "模型解释",
        script_entry: [],
        model_contract: {
          capabilities: ["中文技术解释"],
          input_schema: "{diagnostic}",
          output_schema: "{summary, steps[]}",
          why_not_script: "分类级模板拼成段落不等于解释，解释需要理解这条具体故障的因果。",
        },
      }),
    ]);

    const { RoutingPanel } = await import("../panels/RoutingPanel");
    const { container } = render(<RoutingPanel />);

    await waitFor(() => expect(container.querySelectorAll(".task-row").length).toBe(3));

    // 三个执行者分组标题都在（空组也保留，所以这里正好三条 card h3 里的分组名）
    const heads = [...container.querySelectorAll(".card-head h3")].map(
      (h) => h.textContent ?? "",
    );
    expect(heads.some((h) => h.includes("脚本 / 算法"))).toBe(true);
    expect(heads.some((h) => h.includes("混合（脚本 + 模型）"))).toBe(true);
    expect(heads.some((h) => h.includes("模型"))).toBe(true);

    // 徽标与状态三档照实显示
    expect(container.querySelectorAll(".exec-badge.exec-script").length).toBeGreaterThan(0);
    expect(container.querySelectorAll(".exec-badge.exec-hybrid").length).toBeGreaterThan(0);
    expect(container.querySelectorAll(".exec-badge.exec-model").length).toBeGreaterThan(0);
    expect(screen.getByText("已实现")).toBeInTheDocument();
    expect(screen.getByText("部分实现")).toBeInTheDocument();
    expect(screen.getByText("计划中")).toBeInTheDocument();
  });

  it("交叉线只画真实声明过的：对称声明去重后 = 曲线条数", async () => {
    state.catalog = catalogOf([
      task({ id: "capability.index", crosses_with: ["repair.verify"] }),
      task({
        id: "repair.verify",
        display_name: "修复后复检",
        crosses_with: ["capability.index"],
      }),
      task({ id: "fault.detect", display_name: "故障识别" }),
    ]);

    const { RoutingPanel } = await import("../panels/RoutingPanel");
    const { container } = render(<RoutingPanel />);

    // 一对对称交叉只能画一条线（否则图上会叠两条重合的曲线）
    await waitFor(() => expect(container.querySelectorAll(".routing-link").length).toBe(1));
    expect(container.querySelectorAll(".routing-box").length).toBe(3);
    expect(screen.getByText("1 条交叉")).toBeInTheDocument();
  });

  it("点节点展开完整属性：证明、输入输出、依赖、交叉、模型契约", async () => {
    state.catalog = catalogOf([
      task({ id: "a.index", display_name: "索引", evidence: ["证据甲"] }),
      task({
        id: "b.model",
        display_name: "模型任务",
        executor: "model",
        status: "planned",
        script_entry: [],
        evidence: ["证据乙"],
        depends_on: ["a.index"],
        crosses_with: ["a.index"],
        model_contract: {
          capabilities: ["长上下文"],
          input_schema: "{x}",
          output_schema: "{y}",
          why_not_script: "写不出为什么脚本做不了的节点，通常说明它其实是脚本能做的。",
        },
      }),
    ]);
    // 让 a.index 也声明交叉，保证图能画
    state.catalog.tasks[0].crosses_with = ["b.model"];

    const { RoutingPanel } = await import("../panels/RoutingPanel");
    const { container } = render(<RoutingPanel />);
    await waitFor(() => expect(container.querySelectorAll(".task-row").length).toBe(2));

    (container.querySelectorAll(".task-row")[1] as HTMLElement).click();

    await waitFor(() =>
      expect(container.querySelector(".task-detail")).not.toBeNull(),
    );
    const detail = container.querySelector(".task-detail")!;
    expect(detail.textContent).toContain("证据乙");
    expect(detail.textContent).toContain("长上下文");
    expect(detail.textContent).toContain("模型契约");
    expect(detail.textContent).toContain("{y}");
    // 依赖与交叉都解析成显示名，而不是裸 id
    expect(detail.textContent).toContain("索引");
  });
});

describe("模型分工 · 没数据时明说原因，一个节点都不画", () => {
  it("命令未注册：写清缺 list_capability_tasks，且不画任何节点", async () => {
    state.error = "Command list_capability_tasks not found";

    const { RoutingPanel } = await import("../panels/RoutingPanel");
    const { container } = render(<RoutingPanel />);

    await waitFor(() =>
      expect(screen.getByText(/后端尚未提供 list_capability_tasks/)).toBeInTheDocument(),
    );
    expect(container.querySelectorAll(".task-row").length).toBe(0);
    expect(container.querySelectorAll(".routing-box").length).toBe(0);
    expect(container.querySelectorAll(".routing-link").length).toBe(0);
    // 绝不出现「示例 / 占位」节点
    expect(screen.queryByText("能力索引")).toBeNull();
  });

  it("其它失败：给出错误原文，不画节点", async () => {
    state.error = "内核返回了非法 JSON";

    const { RoutingPanel } = await import("../panels/RoutingPanel");
    const { container } = render(<RoutingPanel />);

    await waitFor(() =>
      expect(screen.getByText(/读取 list_capability_tasks 失败/)).toBeInTheDocument(),
    );
    expect(container.querySelectorAll(".task-row").length).toBe(0);
    expect(container.querySelectorAll(".routing-box").length).toBe(0);
  });

  it("空目录（真实返回 0 个节点）：交叉图明说画不出来", async () => {
    state.catalog = catalogOf([]);

    const { RoutingPanel } = await import("../panels/RoutingPanel");
    const { container } = render(<RoutingPanel />);

    await waitFor(() =>
      expect(screen.getByText(/内核返回了 0 个任务节点/)).toBeInTheDocument(),
    );
    expect(container.querySelectorAll(".routing-box").length).toBe(0);
  });
});
