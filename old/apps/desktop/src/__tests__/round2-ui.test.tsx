import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";

/**
 * 第二轮界面改动的回归网。
 *
 * 用户这一轮点名了 7 条，这里覆盖其中「能靠渲染断言钉死」的部分：
 *  - 资产行显示作者（author），空串就不显示
 *  - 属性栏：支持的智能体类型 / 是否正在使用 / 三态生命周期按钮
 *  - 生命周期命令不存在时**降级**（按钮置灰 + 一行明确提示，不白屏）
 *  - 合并栏 Plugin / Suite 的拆分计数来自两次独立查询
 *  - 顶栏版本徽标 devel / standalone / 取不到就不渲染
 *
 * 后端本轮才接入的命令（list_agent_hosts / set_asset_lifecycle）在这里
 * 用 mock 分别模拟「就绪」和「未就绪」两种状态 —— 这正是需要验证的分支。
 */

type Json = Record<string, unknown>;

const state: {
  /** 每个 kind 的返回，键是 kind */
  assetsByKind: Record<string, { total: number; items: Json[] }>;
  /** set_asset_lifecycle 是否抛「命令不存在」 */
  lifecycleMissing: boolean;
  /** setAssetLifecycle 的调用记录 */
  lifecycleCalls: { id: string; state: string }[];
  kernel: Json;
} = {
  assetsByKind: {},
  lifecycleMissing: false,
  lifecycleCalls: [],
  kernel: {},
};

const listAssets = vi.fn(async (q: { kind?: string | null }) => {
  const k = q?.kind ?? "";
  return state.assetsByKind[k] ?? { total: 0, items: [] };
});

vi.mock("../bridge", () => ({
  runtime: "tauri",
  isTauri: true,
  isMissingCommandError: (e: unknown) =>
    /not found|unknown command/i.test(String(e)),
  listAssets: (q: { kind?: string | null }) => listAssets(q),
  setAssetLifecycle: vi.fn(async (id: string, s: string) => {
    if (state.lifecycleMissing) {
      throw "Command set_asset_lifecycle not found";
    }
    state.lifecycleCalls.push({ id, state: s });
    return true;
  }),
  listAgentHosts: vi.fn(async () => []),
  getAsset: vi.fn(async () => null),
  capabilityUsage: vi.fn(async () => []),
  diagnostics: vi.fn(async () => ({
    diagnostics: [],
    critical: 0,
    error: 0,
    warning: 0,
    info: 0,
    auto_fixable: 0,
    needs_confirm: 0,
    manual_only: 0,
    needs_llm: 0,
  })),
}));

beforeEach(() => {
  state.assetsByKind = {};
  state.lifecycleMissing = false;
  state.lifecycleCalls = [];
  state.kernel = {};
  listAssets.mockClear();
});

function asset(over: Json = {}): Json {
  return {
    id: "plugin::demo",
    kind: "plugin",
    name: "演示插件",
    version: "1.0",
    description: "",
    authority: "read",
    authority_label: "只读",
    tool_posture: "reader",
    side_effect: "none",
    role: null,
    capability_count: 1,
    source_path: "vendor/plugin/demo/plugin.json",
    ...over,
  };
}

describe("AssetKindPanel · 作者 / 合并栏拆分计数", () => {
  it("行上显示作者；author 为空串时不渲染作者标签", async () => {
    state.assetsByKind = {
      skill: {
        total: 2,
        items: [
          asset({ id: "skill::a", kind: "skill", name: "有作者", author: "张三" }),
          asset({ id: "skill::b", kind: "skill", name: "无作者", author: "" }),
        ],
      },
    };
    const { AssetKindPanel } = await import("../panels/AssetKindPanel");
    const { container } = render(
      <AssetKindPanel kinds={["skill"]} onSelect={() => {}} selectedId={null} />,
    );
    await waitFor(() => expect(screen.getByText("有作者")).toBeInTheDocument());

    // 只有一条带作者标签
    const authors = [...container.querySelectorAll(".tag-author")];
    expect(authors).toHaveLength(1);
    expect(authors[0].textContent).toBe("张三");
  });

  it("作者出处三档：行上带出处 class，悬停标题写明出处；无出处时不编", async () => {
    state.assetsByKind = {
      skill: {
        total: 3,
        items: [
          asset({ id: "skill::a", kind: "skill", name: "自声明", author: "甲", author_source: "manifest" }),
          asset({ id: "skill::b", kind: "skill", name: "front", author: "乙", author_source: "frontmatter" }),
          asset({ id: "skill::c", kind: "skill", name: "包推", author: "丙", author_source: "package" }),
          asset({ id: "skill::d", kind: "skill", name: "无出处", author: "丁" }),
        ],
      },
    };
    const { AssetKindPanel } = await import("../panels/AssetKindPanel");
    const { container } = render(
      <AssetKindPanel kinds={["skill"]} onSelect={() => {}} selectedId={null} />,
    );
    await waitFor(() => expect(screen.getByText("无出处")).toBeInTheDocument());

    const tags = [...container.querySelectorAll(".tag-author")];
    expect(tags.map((t) => t.textContent)).toEqual(["甲", "乙", "丙", "丁"]);
    expect(tags[0].classList.contains("src-manifest")).toBe(true);
    expect(tags[1].classList.contains("src-frontmatter")).toBe(true);
    expect(tags[2].classList.contains("src-package")).toBe(true);
    // 没有 author_source：不编一个出处，但也不隐藏作者本身
    expect(tags[3].className).not.toMatch(/src-(manifest|frontmatter|package)/);
    expect(tags[3].getAttribute("title")).toContain("出处未提供");
  });

  it("合并栏对 plugin 与 suite 各查一次，计数照实拆开显示", async () => {
    state.assetsByKind = {
      plugin: { total: 20, items: [asset({ id: "plugin::p1", kind: "plugin", name: "插件一" })] },
      suite: { total: 0, items: [] },
    };
    const { AssetKindPanel } = await import("../panels/AssetKindPanel");
    const { container } = render(
      <AssetKindPanel kinds={["plugin", "suite"]} onSelect={() => {}} selectedId={null} />,
    );
    await waitFor(() =>
      expect(container.querySelector(".kind-split")).not.toBeNull(),
    );

    // 两次独立查询，不是一次拉全量再前端过滤
    const kindsAsked = listAssets.mock.calls.map((c) => c[0]?.kind);
    expect(kindsAsked).toEqual(expect.arrayContaining(["plugin", "suite"]));
    expect(listAssets).toHaveBeenCalledTimes(2);

    // 「Plugin 20 · Suite 0 = 20 项」—— suite 的 0 是真实结果，不是加载失败
    const split = container.querySelector(".kind-split")!.textContent!;
    expect(split).toContain("Plugin 20");
    expect(split).toContain("Suite 0");
    expect(split).toContain("20 项");
  });

  it("合并栏里每行标明它来自哪个 kind 查询", async () => {
    state.assetsByKind = {
      plugin: { total: 1, items: [asset({ id: "plugin::p1", kind: "plugin", name: "插件一" })] },
      suite: { total: 1, items: [asset({ id: "suite::s1", kind: "suite", name: "套件一" })] },
    };
    const { AssetKindPanel } = await import("../panels/AssetKindPanel");
    const { container } = render(
      <AssetKindPanel kinds={["plugin", "suite"]} onSelect={() => {}} selectedId={null} />,
    );
    await waitFor(() => expect(screen.getByText("套件一")).toBeInTheDocument());

    const tags = [...container.querySelectorAll(".kind-tag")].map((n) => n.textContent);
    expect(tags).toEqual(["Plugin", "Suite"]);
  });

  it("单类栏不显示合并说明，也不显示 kind 标记", async () => {
    state.assetsByKind = {
      skill: { total: 1, items: [asset({ id: "skill::a", kind: "skill", name: "读文件" })] },
    };
    const { AssetKindPanel } = await import("../panels/AssetKindPanel");
    const { container } = render(
      <AssetKindPanel kinds={["skill"]} onSelect={() => {}} selectedId={null} />,
    );
    await waitFor(() => expect(screen.getByText("读文件")).toBeInTheDocument());
    expect(container.querySelector(".kind-tag")).toBeNull();
    expect(container.querySelector(".kind-merged-note")).toBeNull();
  });
});

describe("DetailPane · 支持的智能体 / 是否使用 / 三态切换", () => {
  const manifest = {
    id: "plugin::demo",
    kind: "plugin",
    name: "演示插件",
    identity: { authority: "read", side_effect: "none", tool_posture: "reader" },
    provides: [],
  };

  it("host_support 有值 → 逐个渲染；为空 → 明说「未判出归属宿主（不是错误）」", async () => {
    const { DetailPane } = await import("../components/DetailPane");
    const { rerender } = render(
      <DetailPane
        loading={false}
        manifest={manifest}
        summary={{
          id: "plugin::demo",
          kind: "plugin",
          name: "演示插件",
          authority_label: "只读",
          tool_posture: "reader",
          side_effect: "none",
          source_path: null,
          capability_count: 0,
          host_support: ["qoder", "claude", "codex"],
        }}
      />,
    );
    const chips = [...document.querySelectorAll(".host-chip")].map((n) => n.textContent);
    expect(chips).toEqual(["qoder", "claude", "codex"]);

    rerender(
      <DetailPane
        loading={false}
        manifest={manifest}
        summary={{
          id: "plugin::demo",
          kind: "plugin",
          name: "演示插件",
          authority_label: "只读",
          tool_posture: "reader",
          side_effect: "none",
          source_path: null,
          capability_count: 0,
          host_support: [],
        }}
      />,
    );
    expect(screen.getByText(/未判出归属宿主（不是错误）/)).toBeInTheDocument();
  });

  it("有调用记录 → 「正在使用」+ 次数；没有 → 「暂无使用记录」", async () => {
    const { DetailPane } = await import("../components/DetailPane");
    const { rerender } = render(
      <DetailPane
        loading={false}
        manifest={manifest}
        sources={[{ agentId: "dsh", agentName: "DSH", calls: 12, lastUsedAt: 1700000000 }]}
      />,
    );
    expect(screen.getByText("正在使用")).toBeInTheDocument();
    expect(screen.getAllByText(/实报调用 12 次/).length).toBeGreaterThanOrEqual(1);

    rerender(<DetailPane loading={false} manifest={manifest} sources={[]} />);
    expect(screen.getByText(/暂无使用记录/)).toBeInTheDocument();
  });

  it("属性栏作者行按出处标注：自声明 / frontmatter / 包推断；无出处只写「作者」", async () => {
    const { DetailPane } = await import("../components/DetailPane");
    const baseSummary = {
      id: "plugin::demo",
      kind: "plugin",
      name: "演示插件",
      authority_label: "只读",
      tool_posture: "reader",
      side_effect: "none",
      source_path: null,
      capability_count: 0,
    };
    const { rerender } = render(
      <DetailPane
        loading={false}
        manifest={manifest}
        summary={{ ...baseSummary, author: "甲", author_source: "manifest" }}
      />,
    );
    expect(screen.getByText("作者（自声明）")).toBeInTheDocument();

    rerender(
      <DetailPane
        loading={false}
        manifest={manifest}
        summary={{ ...baseSummary, author: "乙", author_source: "frontmatter" }}
      />,
    );
    expect(screen.getByText("作者（frontmatter）")).toBeInTheDocument();

    rerender(
      <DetailPane
        loading={false}
        manifest={manifest}
        summary={{ ...baseSummary, author: "丙", author_source: "package" }}
      />,
    );
    expect(screen.getByText("作者（包推断）")).toBeInTheDocument();

    // 有作者但没有出处字段：退回「作者」，不猜来源
    rerender(
      <DetailPane
        loading={false}
        manifest={manifest}
        summary={{ ...baseSummary, author: "丁" }}
      />,
    );
    expect(screen.getByText("作者")).toBeInTheDocument();

    // 没有作者：整行不出现，也不写「未知作者」
    rerender(<DetailPane loading={false} manifest={manifest} summary={{ ...baseSummary, author: "" }} />);
    const labels = [...document.querySelectorAll(".kv dt")].map((n) => n.textContent);
    expect(labels.some((l) => l && l.includes("作者"))).toBe(false);
    expect(screen.queryByText(/未知作者/)).toBeNull();
  });

  it("三个生命周期按钮：当前态高亮，点另一个会真的调后端", async () => {
    const calls: string[] = [];
    const { DetailPane } = await import("../components/DetailPane");
    const { container } = render(
      <DetailPane
        loading={false}
        manifest={manifest}
        summary={{
          id: "plugin::demo",
          kind: "plugin",
          name: "演示插件",
          authority_label: "只读",
          tool_posture: "reader",
          side_effect: "none",
          source_path: null,
          capability_count: 0,
          lifecycle: "active",
        }}
        onLifecycleChanged={(s) => calls.push(s)}
      />,
    );

    // 三态切换按钮 + 独立的「删除」按钮（第四颗，但**不是第四种状态**：
    // 删除 = 置 archived 加二次确认，符合项目铁律「增量不删」）。
    const btns = [...container.querySelectorAll(".lifecycle-actions .life-btn")];
    expect(btns.map((b) => b.textContent?.replace(" ✓", ""))).toEqual([
      "激活",
      "暂停",
      "归档",
    ]);
    const del = container.querySelector(".life-btn.delete") as HTMLButtonElement;
    expect(del.textContent).toBe("删除");
    expect(del.getAttribute("title")).toContain("不物理删除");
    // 当前是 active：激活按钮高亮且禁用
    expect(btns[0].classList.contains("is-current")).toBe(true);
    expect((btns[0] as HTMLButtonElement).disabled).toBe(true);

    (btns[1] as HTMLButtonElement).click(); // 暂停
    await waitFor(() => expect(calls).toEqual(["paused"]));
    await waitFor(() => expect(screen.getByText(/已切到「暂停」/)).toBeInTheDocument());
  });

  it("命令不存在时优雅降级：三个按钮置灰 + 一行明确提示，不白屏", async () => {
    state.lifecycleMissing = true;
    const { DetailPane } = await import("../components/DetailPane");
    const { container } = render(
      <DetailPane
        loading={false}
        manifest={manifest}
        summary={{
          id: "plugin::demo",
          kind: "plugin",
          name: "演示插件",
          authority_label: "只读",
          tool_posture: "reader",
          side_effect: "none",
          source_path: null,
          capability_count: 0,
          lifecycle: "active",
        }}
      />,
    );

    const btns = [...container.querySelectorAll(".life-btn")] as HTMLButtonElement[];
    (btns[1] as HTMLButtonElement).click();

    await waitFor(() =>
      expect(screen.getByText(/内核尚未提供 set_asset_lifecycle/)).toBeInTheDocument(),
    );
    // 三个按钮全部禁用（点了不会改动任何数据）
    for (const b of btns) expect(b.disabled).toBe(true);
    // 界面还在：正文仍然渲染（没有整块白屏）
    expect(screen.getByText("演示插件")).toBeInTheDocument();
  });
});

describe("App · 顶栏版本徽标", () => {
  it("edition=devel → 橙色开发版徽标；edition=standalone → 正式版；取不到不渲染", async () => {
    const { EditionBadge } = await import("../App");
    const { rerender, container } = render(<EditionBadge info={null} />);
    expect(container.querySelector(".edition-badge")).toBeNull();

    rerender(
      <EditionBadge
        info={{
          app_version: "0.1.0",
          schema_version: "8",
          rustc_version: "rustc",
          os: "windows",
          arch: "x86_64",
          edition: "devel",
        }}
      />,
    );
    expect(container.querySelector(".edition-badge.devel")!.textContent).toBe(
      "开发版 DEVEL",
    );
    expect(container.querySelector(".edition-badge.devel")!.getAttribute("title")).toContain(
      "开发测试版",
    );

    rerender(
      <EditionBadge
        info={{
          app_version: "0.1.0",
          schema_version: "8",
          rustc_version: "rustc",
          os: "windows",
          arch: "x86_64",
          edition: "standalone",
        }}
      />,
    );
    expect(container.querySelector(".edition-badge.standalone")!.textContent).toBe(
      "正式版 STANDALONE",
    );

    // 未知值不猜：原样显示并标 unknown
    rerender(
      <EditionBadge
        info={{
          app_version: "0.1.0",
          schema_version: "8",
          rustc_version: "rustc",
          os: "windows",
          arch: "x86_64",
          edition: "weird",
        }}
      />,
    );
    expect(container.querySelector(".edition-badge.unknown")!.textContent).toBe("weird");
  });
});
