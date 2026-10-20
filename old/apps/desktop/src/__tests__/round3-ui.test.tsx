import { describe, expect, it, vi, beforeEach } from "vitest";
import { useState } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

/**
 * 第三轮界面改动的回归网。四条需求各一组：
 *
 * 1. 资产分区加筛选（来源 / 注册方式 / 生命周期 / 作者）——
 *    关键断言是「本地筛选（当前已加载 N 条）」这句话必须在，
 *    因为筛选只覆盖已加载的列表，界面上不能让人误以为是全库筛选。
 * 2. standalone 版左上角品牌纵向三行（Capability Core / 能力中枢 ·
 *    Capability Nexus / 正式版 STANDALONE），devel 版同结构但第三行不同。
 * 3. 协议支持矩阵：证据列改成「N 条」摘要，点行 → 右侧属性栏显示逐条证据。
 * 4. 资产属性栏生命周期加「删除」按钮：二次确认，且删除 = 归档（不物理删）。
 *
 * 断言只加不减：第二轮已有的断言如果因为结构调整而不成立，改成
 * 断言**新结构**（例如 .life-btn 里多了「删除」），绝不放宽成「存在即可」。
 */

type Json = Record<string, unknown>;

const state: {
  assetsByKind: Record<string, { total: number; items: Json[] }>;
  lifecycleCalls: { id: string; state: string }[];
  hosts: Json[];
} = {
  assetsByKind: {},
  lifecycleCalls: [],
  hosts: [],
};

vi.mock("../bridge", () => ({
  runtime: "tauri",
  isTauri: true,
  isMissingCommandError: (e: unknown) =>
    /not found|unknown command/i.test(String(e)),
  listAssets: vi.fn(async (q: { kind?: string | null }) => {
    const k = q?.kind ?? "";
    return state.assetsByKind[k] ?? { total: 0, items: [] };
  }),
  setAssetLifecycle: vi.fn(async (id: string, s: string) => {
    state.lifecycleCalls.push({ id, state: s });
    return true;
  }),
  listAgentHosts: vi.fn(async () => state.hosts),
  protocolMatrix: vi.fn(async () => ({
    rows: [],
    total_assets: 0,
    total_capabilities: 0,
    total_passed: 0,
    total_failed: 0,
  })),
  getAsset: vi.fn(async () => null),
  capabilityUsage: vi.fn(async () => []),
  // App 外壳测试要用到下面这几条；放在同一个 mock 里，
  // 避免在测试中途 doMock / resetModules 污染后续用例的模块实例。
  kernelInfo: vi.fn(async () => ({
    app_version: "0.1.0",
    schema_version: "8",
    rustc_version: "rustc test",
    os: "test",
    arch: "x86_64",
    edition: "standalone",
  })),
  openRegistry: vi.fn(async () => ({
    db_path: "D:/x/registry.db",
    scan_root: "D:/x/vendor",
    total: 0,
    counts: [],
    ready: true,
  })),
  rescan: vi.fn(async () => ({
    summary: {
      parsed: 0, inserted: 0, updated: 0, unchanged: 0, removed: 0, skipped: 0,
      has_changes: false, has_cycle: false, elapsed_ms: 0,
      logical_created: 0, alias_count: 0, dedup_pending: 0,
    },
    stats: {
      db_path: "D:/x/registry.db",
      scan_root: "D:/x/vendor",
      total: 0,
      counts: [],
      ready: true,
    },
  })),
  diagnostics: vi.fn(async () => ({
    diagnostics: [], critical: 0, error: 0, warning: 0, info: 0,
    auto_fixable: 0, needs_confirm: 0, manual_only: 0, needs_llm: 0,
  })),
  listDedupCandidates: vi.fn(async () => []),
  listAgents: vi.fn(async () => []),
}));

beforeEach(() => {
  state.assetsByKind = {};
  state.lifecycleCalls = [];
  state.hosts = [];
});

function asset(over: Json = {}): Json {
  return {
    id: "skill::a",
    kind: "skill",
    name: "资产 A",
    version: "1.0",
    description: "",
    authority: "read",
    authority_label: "只读",
    tool_posture: "reader",
    side_effect: "none",
    role: null,
    capability_count: 1,
    source_path: null,
    ...over,
  };
}

// ── 1. 资产筛选 ───────────────────────────────────────────────

describe("AssetKindPanel · 本地筛选（来源 / 注册方式 / 生命周期 / 作者）", () => {
  const four = {
    total: 4,
    items: [
      asset({
        id: "skill::vendor",
        name: "厂商资产",
        origin_kind: "vendor",
        registration_kind: "embedded",
        lifecycle: "active",
        author: "甲",
      }),
      asset({
        id: "skill::self",
        name: "自建资产",
        origin_kind: "self_built",
        registration_kind: "mounted",
        lifecycle: "paused",
        author: "乙",
      }),
      asset({
        id: "skill::nurtured",
        name: "自长资产",
        origin_kind: "nurtured",
        registration_kind: "remote",
        lifecycle: "archived",
        author: "甲",
      }),
      asset({ id: "skill::bare", name: "无量产字段资产" }),
    ],
  };

  async function mount() {
    state.assetsByKind = { skill: four };
    const { AssetKindPanel } = await import("../panels/AssetKindPanel");
    const utils = render(
      <AssetKindPanel kinds={["skill"]} onSelect={() => {}} selectedId={null} />,
    );
    await waitFor(() => expect(screen.getByText("厂商资产")).toBeInTheDocument());
    return utils;
  }

  it("界面上必须写清「本地筛选（当前已加载 N 条）」——不能冒充全库筛选", async () => {
    const { container } = await mount();
    const note = container.querySelector(".asset-filters-note")!;
    expect(note).not.toBeNull();
    expect(note.textContent).toContain("本地筛选");
    expect(note.textContent).toContain("当前已加载");
    // 4 条全部加载 = 就是这 4 条
    expect(note.textContent).toContain("4");
    expect(note.textContent).toContain("显示");
    // 说明里写明为什么在本地筛（后端 list_assets 只收 kind/text/tag）
    expect(note.textContent).toContain("list_assets");
  });

  it("四个维度各有一个下拉，选项来自实际数据并带条数", async () => {
    const { container } = await mount();
    const selects = [...container.querySelectorAll(".asset-filters-row select")];
    expect(selects).toHaveLength(4);
    const labels = [...container.querySelectorAll(".filter-select-label")].map(
      (n) => n.textContent,
    );
    expect(labels).toEqual(["来源", "注册方式", "生命周期", "作者"]);

    // 作者下拉只列**实际出现过**的作者，空作者不是选项
    const authorSelect = selects[3] as HTMLSelectElement;
    const authorOptions = [...authorSelect.options].map((o) => o.textContent);
    expect(authorOptions).toContain("甲（2）");
    expect(authorOptions).toContain("乙（1）");
    expect(authorOptions.some((t) => t!.includes("（0）") && !t!.includes("全部"))).toBe(false);
  });

  it("按来源筛：只剩 vendor 那一行；再按作者筛：组合条件同时生效", async () => {
    const { container } = await mount();
    const selects = [...container.querySelectorAll(".asset-filters-row select")];

    const setSelect = (i: number, v: string) => {
      const el = selects[i] as HTMLSelectElement;
      el.value = v;
      el.dispatchEvent(new Event("change", { bubbles: true }));
    };

    // 来源 = vendor
    setSelect(0, "vendor");
    await waitFor(() =>
      expect(screen.queryByText("自建资产")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("厂商资产")).toBeInTheDocument();
    expect(screen.queryByText("自长资产")).not.toBeInTheDocument();

    // 生命周期 = active（vendor 那条正是 active）→ 仍然只剩它
    setSelect(2, "active");
    await waitFor(() => expect(screen.getByText("厂商资产")).toBeInTheDocument());
    expect(screen.queryByText("自建资产")).not.toBeInTheDocument();

    // 已筛维度计数：2 个维度生效
    expect(container.querySelector(".asset-filters-note")!.textContent).toContain(
      "已筛 2 个维度",
    );
  });

  it("作者维度：精确匹配该作者的资产（甲有两条）", async () => {
    await mount();
    const selects = [...document.querySelectorAll(".asset-filters-row select")];
    const authorSelect = selects[3] as HTMLSelectElement;
    authorSelect.value = "甲";
    authorSelect.dispatchEvent(new Event("change", { bubbles: true }));
    await waitFor(() =>
      expect(screen.queryByText("自建资产")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("厂商资产")).toBeInTheDocument();
    expect(screen.getByText("自长资产")).toBeInTheDocument();
  });

  it("「清空筛选」一键复位：四个维度 + 关键词全部清掉", async () => {
    const { container } = await mount();
    const selects = [...container.querySelectorAll(".asset-filters-row select")];
    const clear = container.querySelector(".filter-clear") as HTMLButtonElement;
    // 初始没有筛选条件：清空按钮禁用
    expect(clear.disabled).toBe(true);

    (selects[0] as HTMLSelectElement).value = "vendor";
    selects[0].dispatchEvent(new Event("change", { bubbles: true }));
    await waitFor(() => expect(clear.disabled).toBe(false));

    const kw = container.querySelector(".kind-search") as HTMLInputElement;
    kw.value = "厂商";
    kw.dispatchEvent(new Event("input", { bubbles: true }));
    await waitFor(() => expect(kw.value).toBe("厂商"));

    clear.click();
    await waitFor(() => {
      expect((selects[0] as HTMLSelectElement).value).toBe("");
      expect((container.querySelector(".kind-search") as HTMLInputElement).value).toBe(
        "",
      );
      expect(screen.getByText("自建资产")).toBeInTheDocument();
      expect(screen.getByText("自长资产")).toBeInTheDocument();
    });
    expect(clear.disabled).toBe(true);
  });

  it("本地筛选无匹配时明说「只在这 N 条里找」，不让人以为库里没有", async () => {
    const { container } = await mount();
    const selects = [...container.querySelectorAll(".asset-filters-row select")];
    fireEvent.change(selects[0], { target: { value: "self_built" } });
    fireEvent.change(container.querySelector(".kind-search")!, {
      target: { value: "绝对不存在的关键词" },
    });

    await waitFor(() =>
      expect(container.querySelector(".empty")!.textContent).toContain(
        "本地筛选无匹配",
      ),
    );
    expect(container.querySelector(".empty")!.textContent).toContain(
      "不匹配不等于库内没有",
    );
  });
});

// ── 2. 品牌纵向三行 ───────────────────────────────────────────

describe("App · 左上角品牌纵向三行", () => {
  it("三行依次是 Capability Core / 能力中枢 · Capability Nexus / 版本徽标", async () => {
    const { EditionBadge } = await import("../App");
    const { container } = render(
      <div className="topbar">
        <div className="brand">
          <span className="logo">◈</span>
          <div className="brand-text">
            <h1>Capability Core</h1>
            <p className="tagline">能力中枢 · Capability Nexus</p>
            <div className="brand-edition">
              <EditionBadge
                info={{
                  app_version: "0.1.0",
                  schema_version: "8",
                  rustc_version: "rustc",
                  os: "windows",
                  arch: "x86_64",
                  edition: "standalone",
                }}
              />
            </div>
          </div>
        </div>
      </div>,
    );
    const text = container.querySelector(".brand-text")!;
    const kids = [...text.children].map((n) => n.className || n.tagName.toLowerCase());
    expect(kids).toEqual(["h1", "tagline", "brand-edition"]);
    expect(text.children[0].textContent).toBe("Capability Core");
    expect(text.children[1].textContent).toBe("能力中枢 · Capability Nexus");
    // 第三行：standalone → 正式版 STANDALONE
    expect(
      text.querySelector(".brand-edition .edition-badge.standalone")!.textContent,
    ).toBe("正式版 STANDALONE");
  });

  it("devel 版同样是三行，只是第三行换成开发版 DEVEL", async () => {
    const { EditionBadge } = await import("../App");
    const { container } = render(
      <div className="brand-text">
        <h1>Capability Core</h1>
        <p className="tagline">能力中枢 · Capability Nexus</p>
        <div className="brand-edition">
          <EditionBadge
            info={{
              app_version: "0.1.0",
              schema_version: "8",
              rustc_version: "rustc",
              os: "windows",
              arch: "x86_64",
              edition: "devel",
            }}
          />
        </div>
      </div>,
    );
    // brand-text 未包在容器里，直接断言它的三行顺序
    expect(container.querySelectorAll("h1, .tagline, .brand-edition")).toHaveLength(3);
    expect(
      container.querySelector(".brand-edition .edition-badge.devel")!.textContent,
    ).toBe("开发版 DEVEL");
  });

  it("真实 App 外壳：品牌块是纵向三行结构，且「能力中枢」仍然只出现一次", async () => {
    const { default: App } = await import("../App");
    const { container } = render(<App />);
    const brand = container.querySelector(".topbar .brand")!;
    expect(brand).not.toBeNull();
    expect(brand.querySelector(".brand-text")).not.toBeNull();
    expect(brand.querySelector("h1")!.textContent).toBe("Capability Core");
    expect(brand.querySelector(".tagline")!.textContent).toBe(
      "能力核心 · 能力中枢",
    );
    expect(brand.querySelector(".brand-edition")).not.toBeNull();
    expect(container.textContent!.split("能力中枢").length - 1).toBe(1);
  });
});

// ── 3. 协议支持矩阵：证据进属性栏 ────────────────────────────

describe("HostsPanel + HostDetail · 证据移入属性栏", () => {
  const hosts = [
    {
      id: "claude-code",
      display_name: "Claude Code",
      vendor: "Anthropic",
      kind: "cli",
      detected: true,
      connected: true,
      last_seen: 1700000000,
      protocols: [
        { name: "skill-md", transport: "skill-md", support: "supported", note: "读 SKILL.md" },
        { name: "mcp-http", transport: "mcp-http", support: "partial", note: "需手动配 endpoint" },
      ],
      evidence: ["~/.claude/plugins 存在", "PATH 里有 claude 命令"],
    },
  ];

  it("表格只放「2 条」，长证据原文不在表格里", async () => {
    state.hosts = hosts;
    const { HostsPanel } = await import("../panels/HostsPanel");
    const { container } = render(<HostsPanel />);
    await waitFor(() =>
      expect(container.querySelector(".evidence-count")!.textContent).toBe("2 条"),
    );
    expect(container.querySelector(".hosts-matrix")!.textContent).not.toContain(
      "~/.claude/plugins 存在",
    );
  });

  it("HostDetail 逐条列出全部证据 + 协议清单（support + note）", async () => {
    const { HostDetail } = await import("../components/HostDetail");
    const { container } = render(
      <HostDetail
        host={{
          id: "claude-code",
          display_name: "Claude Code",
          vendor: "Anthropic",
          kind: "cli",
          detected: true,
          connected: true,
          last_seen: 1700000000,
          protocols: [
            { name: "skill-md", transport: "skill-md", support: "supported", note: "读 SKILL.md" },
            { name: "mcp-http", transport: "mcp-http", support: "partial", note: "需手动配 endpoint" },
          ],
          evidence: ["~/.claude/plugins 存在", "PATH 里有 claude 命令"],
        }}
      />,
    );
    // 证据逐条，一条一个 li，原文照录
    const ev = [...container.querySelectorAll(".evidence-list li")].map(
      (n) => n.textContent,
    );
    expect(ev).toEqual(["~/.claude/plugins 存在", "PATH 里有 claude 命令"]);
    expect(screen.getByText(/全部证据（2 条）/)).toBeInTheDocument();

    // 协议清单：两档状态 + 各自的 note 都在
    expect(container.querySelectorAll(".protocol-list li")).toHaveLength(2);
    expect(screen.getByText("读 SKILL.md")).toBeInTheDocument();
    expect(screen.getByText("需手动配 endpoint")).toBeInTheDocument();
    expect(container.querySelector(".sup-cell.sup-ok")!.textContent).toBe("支持");
    expect(container.querySelector(".sup-cell.sup-partial")!.textContent).toBe("部分");

    // 关键元信息：厂商 / 类型 / 检测 / 连接 / last_seen
    const dts = [...container.querySelectorAll(".kv dt")].map((n) => n.textContent);
    expect(dts).toEqual(["厂商", "类型", "检测到", "已连接", "最后出现"]);
    expect(screen.getByText("Anthropic")).toBeInTheDocument();
    // last_seen 是真实时间，不是「从未见过」
    expect(container.querySelector(".kv")!.textContent).not.toContain("从未见过");
  });

  it("没有证据 / 没有协议时不编内容：明说注册表里没有这些字段", async () => {
    const { HostDetail } = await import("../components/HostDetail");
    render(
      <HostDetail
        host={{
          id: "qoder",
          display_name: "Qoder",
          vendor: "",
          kind: "plugin",
          detected: false,
          connected: false,
          last_seen: null,
          protocols: [],
          evidence: [],
        }}
      />,
    );
    expect(screen.getByText(/后端没有给出任何判定证据/)).toBeInTheDocument();
    expect(screen.getByText(/没有声明任何协议/)).toBeInTheDocument();
    expect(screen.getByText("—（从未见过）")).toBeInTheDocument();
  });
});

describe("App 集成 · 点协议矩阵行 → 右侧属性栏切到宿主详情", () => {
  it("从侧栏进入协议支持矩阵，点一行，右侧标题变「宿主详情」并列出证据", async () => {
    state.hosts = [
      {
        id: "deepseek-harness",
        display_name: "DeepSeek Harness",
        vendor: "DeepSeek",
        kind: "harness",
        detected: true,
        connected: true,
        protocols: [
          { name: "mcp-stdio", transport: "mcp-stdio", support: "supported", note: "标准输入输出" },
        ],
        evidence: ["插件目录 C:\\external-assets\\.plugin 存在", "心跳文件已写"],
      },
    ];
    const { default: App } = await import("../App");
    const { container } = render(<App />);
    // 等外壳起来，再点侧栏里的「协议支持矩阵」
    await waitFor(() =>
      expect(document.querySelectorAll(".nav-item").length).toBeGreaterThan(0),
    );
    const nav = [...document.querySelectorAll(".nav-item")].find((n) =>
      n.textContent?.includes("协议支持矩阵"),
    ) as HTMLElement;
    nav.click();
    await waitFor(() =>
      expect(container.querySelector(".hosts-matrix tbody tr")).not.toBeNull(),
    );
    // 右侧还没有选中宿主：标题是「侧栏数据通路」
    expect(container.querySelector(".detail h2")!.textContent).toBe("侧栏数据通路");

    (container.querySelector(".hosts-matrix tbody tr") as HTMLElement).click();
    await waitFor(() =>
      expect(container.querySelector(".detail h2")!.textContent).toBe("宿主详情"),
    );
    // 证据逐条在属性栏里，表格里只有「2 条」
    const ev = [...container.querySelectorAll(".detail .evidence-list li")].map(
      (n) => n.textContent,
    );
    expect(ev).toHaveLength(2);
    expect(ev[0]).toContain("插件目录");
    expect(container.querySelector(".hosts-matrix .evidence-count")!.textContent).toBe(
      "2 条",
    );
    // 选中行高亮
    expect(container.querySelector(".hosts-matrix tbody tr.active")).not.toBeNull();
  });
});

// ── 4. 属性栏「删除」按钮 ────────────────────────────────────

describe("DetailPane · 生命周期里的「删除」", () => {
  const manifest = {
    id: "plugin::demo",
    kind: "plugin",
    name: "演示插件",
    identity: { authority: "read", side_effect: "none", tool_posture: "reader" },
    provides: [],
  };
  const summary = {
    id: "plugin::demo",
    kind: "plugin",
    name: "演示插件",
    authority_label: "只读",
    tool_posture: "reader",
    side_effect: "none",
    source_path: null,
    capability_count: 0,
    lifecycle: "active",
  };

  it("删除是独立一颗按钮，且明写「删除 = 归档（不物理删除）」", async () => {
    const { DetailPane } = await import("../components/DetailPane");
    const { container } = render(
      <DetailPane loading={false} manifest={manifest} summary={summary} />,
    );
    const del = container.querySelector(".life-btn.delete")!;
    expect(del).not.toBeNull();
    expect(del.textContent).toBe("删除");
    // 三态按钮与删除分在两个容器里：删除不是第四种状态
    expect(
      container.querySelectorAll(".lifecycle-actions .life-btn"),
    ).toHaveLength(3);
    expect(container.querySelectorAll(".lifecycle-delete .life-btn")).toHaveLength(1);
    expect(container.querySelector(".lifecycle-delete-note")!.textContent).toContain(
      "删除 = 归档",
    );
    expect(container.querySelector(".lifecycle-delete-note")!.textContent).toContain(
      "不物理删除",
    );
  });

  it("第一次点只进入确认态（不调后端），取消后回到原样", async () => {
    const { DetailPane } = await import("../components/DetailPane");
    const { container } = render(
      <DetailPane loading={false} manifest={manifest} summary={summary} />,
    );
    (container.querySelector(".life-btn.delete") as HTMLButtonElement).click();
    await waitFor(() =>
      expect(container.querySelector(".life-btn.delete.confirm")).not.toBeNull(),
    );
    expect(container.querySelector(".life-btn.delete.confirm")!.textContent).toBe(
      "确认删除？",
    );
    // 两次点击之间**一次后端调用都没有**
    expect(state.lifecycleCalls).toEqual([]);

    (container.querySelector(".life-btn.cancel") as HTMLButtonElement).click();
    await waitFor(() =>
      expect(container.querySelector(".life-btn.delete.confirm")).toBeNull(),
    );
    expect(state.lifecycleCalls).toEqual([]);
  });

  it("二次确认后调 set_asset_lifecycle(id, 'archived')，并告知「已从默认列表移除」", async () => {
    const changed: string[] = [];
    const { DetailPane } = await import("../components/DetailPane");
    const { container } = render(
      <DetailPane
        loading={false}
        manifest={manifest}
        summary={summary}
        onLifecycleChanged={(s) => changed.push(s)}
      />,
    );
    (container.querySelector(".life-btn.delete") as HTMLButtonElement).click();
    await waitFor(() =>
      expect(container.querySelector(".life-btn.delete.confirm")).not.toBeNull(),
    );
    (container.querySelector(".life-btn.delete.confirm") as HTMLButtonElement).click();

    await waitFor(() => expect(state.lifecycleCalls).toEqual([
      { id: "plugin::demo", state: "archived" },
    ]));
    await waitFor(() =>
      expect(screen.getByText(/已从默认列表移除/)).toBeInTheDocument(),
    );
    expect(screen.getByText(/未物理删除任何文件/)).toBeInTheDocument();
    // 父组件收到归档状态 → 资产列表重拉，该行从默认列表消失
    expect(changed).toEqual(["archived"]);
  });

  it("删除成功后：列表重拉（archived 默认被后端过滤），该资产从当前列表消失", async () => {
    // 后端 list_assets 默认 include_archived=false（kernel AssetQuery 默认值），
    // 所以把这条资产置 archived 之后，再查就查不到它了。这里按真实行为打桩：
    // 第一次返回 2 条，切完 lifecycle 之后返回 1 条。
    state.assetsByKind = {
      skill: { total: 2, items: [asset({ id: "skill::keep", name: "留下的资产" }), asset({ id: "skill::gone", name: "要删的资产" })] },
    };
    let deleted = false;
    const listAssets = (await import("../bridge")).listAssets as unknown as {
      mockImplementation: (fn: (q: { kind?: string | null }) => Promise<unknown>) => void;
    };
    listAssets.mockImplementation(async () => {
      const items = [
        asset({ id: "skill::keep", name: "留下的资产" }),
        ...(deleted ? [] : [asset({ id: "skill::gone", name: "要删的资产" })]),
      ];
      return { total: items.length, items };
    });

    const { DetailPane } = await import("../components/DetailPane");
    const { AssetKindPanel } = await import("../panels/AssetKindPanel");
    // 模拟 App 的接线：属性栏 onLifecycleChanged → listVersion+1 → 列表重拉
    function Harness() {
      const [version, setVersion] = useState(0);
      return (
        <>
          <AssetKindPanel
            kinds={["skill"]}
            onSelect={() => {}}
            selectedId={null}
            refreshKey={version}
          />
          <DetailPane
            loading={false}
            manifest={manifest}
            summary={{ ...summary, id: "skill::gone", name: "要删的资产" }}
            onLifecycleChanged={() => {
              deleted = true;
              setVersion((v) => v + 1);
            }}
          />
        </>
      );
    }
    const { container } = render(<Harness />);
    await waitFor(() => expect(screen.getByText("要删的资产")).toBeInTheDocument());

    (container.querySelector(".life-btn.delete") as HTMLButtonElement).click();
    await waitFor(() =>
      expect(container.querySelector(".life-btn.delete.confirm")).not.toBeNull(),
    );
    (container.querySelector(".life-btn.delete.confirm") as HTMLButtonElement).click();

    // 该资产从列表里消失（不是变灰、不是打标，是真的不在当前列表了）
    await waitFor(() =>
      expect(screen.queryByText("要删的资产")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("留下的资产")).toBeInTheDocument();
    // 后端收到的确实是 archived，不是物理删除命令
    expect(state.lifecycleCalls).toEqual([
      { id: "skill::gone", state: "archived" },
    ]);
  });

  it("已归档的资产在属性栏顶部明说「默认列表已不再列出它」", async () => {
    const { DetailPane } = await import("../components/DetailPane");
    const { container } = render(
      <DetailPane
        loading={false}
        manifest={manifest}
        summary={{ ...summary, lifecycle: "archived" }}
      />,
    );
    const banner = container.querySelector(".archive-banner")!;
    expect(banner).not.toBeNull();
    expect(banner.textContent).toContain("已归档");
    expect(banner.textContent).toContain("行仍在库里");
    // 已归档时删除按钮禁用（它已经是删除后的状态）
    expect((container.querySelector(".life-btn.delete") as HTMLButtonElement).disabled).toBe(
      true,
    );
  });
});
