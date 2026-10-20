import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";

/**
 * App 外壳测试：一条侧栏 + 分区标题 + 侧栏行。
 *
 * 用户给的结构参考（danmaku-frame / LiveControl 侧栏）是
 * 「一条侧栏里用分组标题分区」，所以这里直接断言 DOM 结构：
 *  - 五个分区标题都在（顺序固定：总览 → 资产 → 模型配置 → 新增能力 → 维修站）
 *  - 每个分区下面是 .nav-item 行
 *  - 点某条侧栏行 → 该行 is-active，内容区切换
 *  - 未接后端的侧栏打开后在内容区明确写「未接」
 *  - **左上角品牌只出现一次**（侧栏里的 .sidebar-top 已删除）
 *  - **「库内记录」在资产分区里**，footer 里不再有它，也没有「待确认去重」
 *
 * bridge 全量打桩：外壳测试关心的是导航结构，不是真实内核。
 */

type Json = Record<string, unknown>;

const state: {
  agents: Json[];
  usage: Json[];
  diag: Json;
  /** list_dedup_candidates 的返回（App 启动时的徽标统计与 DedupPanel 共用） */
  candidates: Json[];
} = {
  agents: [],
  usage: [],
  candidates: [],
  diag: {
    diagnostics: [],
    critical: 0,
    error: 0,
    warning: 0,
    info: 0,
    auto_fixable: 0,
    needs_confirm: 0,
    manual_only: 0,
    needs_llm: 0,
  },
};

vi.mock("../bridge", () => ({
  runtime: "tauri",
  kernelInfo: vi.fn(async () => ({
    app_version: "0.1.0",
    schema_version: "1.0",
    rustc_version: "rustc test",
    os: "test",
    arch: "x86_64",
  })),
  openRegistry: vi.fn(async () => ({
    db_path: "D:/x/registry.db",
    scan_root: "D:/x/vendor",
    total: 506,
    counts: [
      { kind: "skill", count: 460 },
      { kind: "mcp", count: 11 },
      { kind: "plugin", count: 20 },
      { kind: "suite", count: 3 },
      { kind: "ghost", count: 3 },
      { kind: "organization", count: 2 },
    ],
    ready: true,
  })),
  rescan: vi.fn(async () => ({
    summary: {
      parsed: 506,
      inserted: 0,
      updated: 0,
      unchanged: 506,
      removed: 0,
      skipped: 0,
      has_changes: false,
      has_cycle: false,
      elapsed_ms: 12,
      logical_created: 506,
      alias_count: 0,
      dedup_pending: 0,
    },
    stats: {
      db_path: "D:/x/registry.db",
      scan_root: "D:/x/vendor",
      total: 506,
      counts: [
        { kind: "skill", count: 460 },
        { kind: "mcp", count: 11 },
      ],
      ready: true,
    },
  })),
  listAssets: vi.fn(async () => ({ total: 0, items: [] })),
  getAsset: vi.fn(async () => null),
  listAgentHosts: vi.fn(async () => []),
  setAssetLifecycle: vi.fn(async () => true),
  isMissingCommandError: vi.fn(() => false),
  listAgents: vi.fn(async () => state.agents),
  capabilityUsage: vi.fn(async () => state.usage),
  diagnostics: vi.fn(async () => state.diag),
  registerStaticAgents: vi.fn(async () => null),
  scanHeartbeats: vi.fn(async () => null),
  listDedupCandidates: vi.fn(async () => state.candidates),
  dedupHistory: vi.fn(async () => []),
  decideDedup: vi.fn(async () => true),
  protocolMatrix: vi.fn(async () => ({
    rows: [],
    total_assets: 0,
    total_capabilities: 0,
    total_passed: 0,
    total_failed: 0,
  })),
  llmStatus: vi.fn(async () => ({
    config_path: "D:/x/llm-providers.json",
    config_exists: false,
    providers: [],
    implemented: 0,
    note: "",
  })),
}));

beforeEach(() => {
  state.agents = [];
  state.usage = [];
  state.candidates = [];
});

describe("App · 一条侧栏里的分区 → 侧栏", () => {
  it("五个分区标题按顺序渲染，且它们是分组标题而不是第二列", async () => {
    const { default: App } = await import("../App");
    const { container } = render(<App />);

    const labels = [...container.querySelectorAll(".nav-label")].map(
      (n) => n.textContent?.replace(/^[^\p{L}]+/u, "").trim(),
    );
    // 本轮顺序：维修站移到新增能力**下面**
    expect(labels).toEqual(["总览", "资产", "模型配置", "新增能力", "维修站"]);

    // 分区标题与侧栏行在**同一个** .sidebar 容器里（不是两列）
    const rails = container.querySelectorAll(".nav-rail");
    expect(rails).toHaveLength(1);
    expect(rails[0].querySelectorAll(".nav-label")).toHaveLength(5);
    // 总览 4 + 资产 5（plugin/suite 合并后由 6 → 5）+ 模型配置 2 + 新增能力 1 + 维修站 4
    expect(rails[0].querySelectorAll(".nav-item").length).toBe(
      4 + 5 + 2 + 1 + 4,
    );
  });

  it("「能力中枢」品牌只出现一次：侧栏里的 .sidebar-top 已删除，顶栏 .brand 保留", async () => {
    const { default: App } = await import("../App");
    const { container } = render(<App />);

    // 被圈掉的那块（侧栏顶部品牌）必须不存在
    expect(container.querySelector(".sidebar-top")).toBeNull();
    expect(container.querySelector(".rail-logo")).toBeNull();
    expect(container.querySelector(".rail-title")).toBeNull();
    // 保留的那块（顶栏 brand）必须存在，且顶部第一块内容就是分区标题
    expect(container.querySelector(".topbar .brand")).not.toBeNull();
    const rail = container.querySelector(".nav-rail")!;
    expect(rail.firstElementChild?.classList.contains("nav-section")).toBe(true);

    // 全文档里「能力中枢」只出现 1 次（顶栏 tagline）
    expect(container.textContent!.split("能力中枢").length - 1).toBe(1);
  });

  it("库内记录 506 显示在资产分区，footer 不再有它，也没有待确认去重", async () => {
    const { default: App } = await import("../App");
    const { container } = render(<App />);

    await waitFor(() => expect(container.querySelector(".section-summary")).not.toBeNull());
    const assetSection = [...container.querySelectorAll(".nav-section")].find((s) =>
      s.querySelector(".nav-label")?.textContent?.includes("资产"),
    )!;
    expect(assetSection.querySelector(".section-summary")).not.toBeNull();
    expect(assetSection.querySelector(".section-summary")!.textContent).toContain("506");

    // 左下角 footer：没有「库内记录」，也没有「待确认去重」（已移到维修站「去重」侧栏）
    const footer = container.querySelector(".sidebar-footer")!;
    expect(footer.textContent).not.toContain("库内记录");
    expect(footer.textContent).not.toContain("待确认去重");
    // 「本地内核 · 分区」这条运行时提示保留
    expect(footer.querySelector(".rail-foot")).not.toBeNull();
  });

  it("默认落在总览 / 在线智能体，且首屏不画假图", async () => {
    const { default: App } = await import("../App");
    render(<App />);

    // 「在线智能体」同时出现在侧栏行与内容区标题里，先定位到侧栏行
    await waitFor(() => {
      const active = document.querySelector(".nav-item.is-active");
      expect(active).toHaveTextContent("在线智能体");
    });

    // 没有 list_agents 数据 → 明确写「暂无数据 + 原因」，不画节点
    await waitFor(() => {
      expect(screen.getByText(/库里还没有任何智能体登记/)).toBeInTheDocument();
    });
    expect(document.querySelector(".fleet-node")).toBeNull();
    expect(document.querySelector(".fleet-links")).toBeNull();
  });

  it("点「故障解决方案」切到该侧栏，高亮随之移动", async () => {
    const { default: App } = await import("../App");
    render(<App />);
    await waitFor(() => expect(screen.getByText("故障解决方案")).toBeInTheDocument());

    (screen.getByText("故障解决方案") as HTMLElement).click();
    await waitFor(() => {
      const active = document.querySelector(".nav-item.is-active");
      expect(active).toHaveTextContent("故障解决方案");
    });
  });

  it("资产分区六类侧栏都在侧栏里，且属于同一个分组", async () => {
    const { default: App } = await import("../App");
    render(<App />);

    const labels = [...document.querySelectorAll(".nav-label")];
    const assetSection = labels.find((n) => n.textContent?.includes("资产"));
    expect(assetSection).toBeTruthy();
    const nav = assetSection!.nextElementSibling!;
    const texts = [...nav.querySelectorAll(".nav-text")].map((n) => n.textContent);
    // 本轮：plugin 与 suite 合并成一条侧栏（用户要求「合并 plugin 和 suite 组别」）
    expect(texts).toEqual(["Skill", "MCP", "Plugin / Suite", "Ghost", "Org"]);
  });

  it("新增能力侧栏展示受控导入表单", async () => {
    const { default: App } = await import("../App");
    render(<App />);
    await waitFor(() =>
      expect(document.querySelectorAll(".nav-item").length).toBeGreaterThan(0),
    );

    const target = [...document.querySelectorAll(".nav-item")].find((n) =>
      n.textContent?.includes("新增能力"),
    ) as HTMLElement;
    target.click();

    await waitFor(() => {
      expect(document.querySelector(".nav-item.is-active")).toHaveTextContent(
        "新增能力",
      );
    });
    // 内容区与属性栏都会写「未接」，这里只要求出现且不出现假表单
    expect(screen.getByRole("button", {name: /检查并生成收录预览/})).toBeInTheDocument();
    expect(document.querySelector(".detail-body")).not.toBeNull();
  });

  it("维修站「去重」侧栏：待处理资产带上类型，裁决按钮只有一份", async () => {
    // 一组 L1 候选，位置 id 带 skill:: 前缀 → 类型应显示为 Skill
    state.candidates = [
      {
        group_key: "l1-abc",
        match_level: "L1",
        suggested_canonical: "loc-skill::a",
        location_ids: ["loc-skill::a", "loc-skill::b"],
      },
    ];

    const { default: App } = await import("../App");
    const { container } = render(<App />);
    await waitFor(() =>
      expect(document.querySelectorAll(".nav-item").length).toBeGreaterThan(0),
    );

    const dedupNav = [...document.querySelectorAll(".nav-item")].find((n) =>
      n.textContent?.includes("去重"),
    ) as HTMLElement;
    dedupNav.click();

    await waitFor(() => {
      expect(document.querySelector(".nav-item.is-active")).toHaveTextContent("去重");
    });
    await waitFor(() =>
      expect(container.querySelectorAll(".dedup-item").length).toBe(1),
    );
    // 每组必须标明类型（从位置 id 前缀读出，不是猜的）
    expect(container.querySelector(".dedup-kind")!.textContent).toBe("Skill");
    // 三个裁决按钮，且整页只有这一份
    expect(container.querySelectorAll(".dedup-actions button")).toHaveLength(3);
    expect(container.querySelectorAll(".dedup-actions").length).toBe(1);
  });

  it("能力检索侧栏：五种检索方式都在且有真实执行入口", async () => {
    const { default: App } = await import("../App");
    render(<App />);
    await waitFor(() =>
      expect(document.querySelectorAll(".nav-item").length).toBeGreaterThan(0),
    );
    const target = [...document.querySelectorAll(".nav-item")].find((n) =>
      n.textContent?.includes("能力检索"),
    ) as HTMLElement;
    target.click();

    await waitFor(() => {
      expect(document.querySelectorAll(".mode-card").length).toBe(5);
    });
    const names = [...document.querySelectorAll(".mode-name")].map(
      (n) => n.textContent,
    );
    expect(names).toEqual(["关键词", "Python", "Rust", "本地向量", "云端向量"]);
    // 五种方式都有执行入口，向量检索仍需配置模型。
    const stubCount = document.querySelectorAll(".mode-card.stub").length;
    expect(stubCount).toBe(0);
    expect(screen.getByText(/5 种执行通路/)).toBeInTheDocument();
  });
});
