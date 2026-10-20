import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";

/**
 * 图形化 / 列表 切换 + 资产六类列表 + 通用属性栏 的行为测试。
 *
 * 这里不点真实 exe：bridge 被打桩，测的是「面板拿到数据后渲染成什么」。
 * 关键是钉住两条纪律：
 *  1. 有数据 → 图上真的出现节点与连线（不是装饰）；
 *  2. 没数据 → 只有空态文案，一个节点都不画。
 */

type Json = Record<string, unknown>;

const state: {
  assets: Json[];
  manifest: Json | null;
  agents: Json[];
  usage: Json[];
} = { assets: [], manifest: null, agents: [], usage: [] };

vi.mock("../bridge", () => ({
  runtime: "tauri",
  listAssets: vi.fn(async () => ({ total: state.assets.length, items: state.assets })),
  getAsset: vi.fn(async () => state.manifest),
  listAgents: vi.fn(async () => state.agents),
  capabilityUsage: vi.fn(async () => state.usage),
  registerStaticAgents: vi.fn(async () => null),
  scanHeartbeats: vi.fn(async () => null),
}));

function agent(over: Json = {}): Json {
  return {
    agent_id: "dsh",
    display_name: "DeepSeek Harness",
    ecosystem: "dsh",
    kind: "host",
    transport: "heartbeat",
    endpoint: "",
    first_seen: 1,
    last_seen: 2,
    meta: "",
    heartbeat: {
      host: "dsh",
      version: "0.2.1",
      pid: 1,
      last_beat: 2,
      age_ms: 900,
      source_file: "hb.json",
      state: "online",
      capabilities: [],
    },
    capability_count: 2,
    call_total: 0,
    last_error: "",
    ...over,
  };
}

function usage(over: Json = {}): Json {
  return {
    agent_id: "dsh",
    agent_name: "DeepSeek Harness",
    ecosystem: "dsh",
    capability_id: "dsh.read",
    asset_id: "skill::x",
    asset_name: "读文件",
    source: "mcp",
    call_count: 7,
    first_used_at: 1,
    last_used_at: 2,
    last_error: "",
    ...over,
  };
}

beforeEach(() => {
  state.assets = [];
  state.manifest = null;
  state.agents = [];
  state.usage = [];
});

describe("FleetPanel · 图形化 / 列表 可切换", () => {
  it("有数据时图形化画出节点与连线，且连线数 = chip 数", async () => {
    state.agents = [agent({})];
    state.usage = [usage({ capability_id: "dsh.read" }), usage({ capability_id: "dsh.write" })];
    const { FleetPanel } = await import("../panels/FleetPanel");
    const { container } = render(<FleetPanel faultCounts={null} onGoRepair={() => {}} />);

    await waitFor(() => {
      expect(container.querySelectorAll(".fleet-node")).toHaveLength(1);
    });
    expect(container.querySelectorAll(".fleet-cap")).toHaveLength(2);
    // 每个 chip 一条贝塞尔连线
    await waitFor(() => {
      expect(container.querySelectorAll(".fleet-links path")).toHaveLength(2);
    });
    expect(screen.getByText("DeepSeek Harness")).toBeInTheDocument();
    expect(screen.getByText("dsh.read")).toBeInTheDocument();
  });

  it("切到列表：同样的数据变成表格行，图形节点消失", async () => {
    state.agents = [agent({})];
    state.usage = [usage({ capability_id: "dsh.read" })];
    const { FleetPanel } = await import("../panels/FleetPanel");
    const { container } = render(<FleetPanel faultCounts={null} onGoRepair={() => {}} />);

    await waitFor(() => expect(container.querySelectorAll(".fleet-node")).toHaveLength(1));

    (screen.getByText("列表") as HTMLElement).click();
    await waitFor(() => {
      expect(container.querySelectorAll(".fleet-table tbody tr")).toHaveLength(1);
    });
    expect(container.querySelectorAll(".fleet-node")).toHaveLength(0);
    // 表格里也能看到这条能力
    expect(screen.getByText("dsh.read")).toBeInTheDocument();

    // 切回图形化
    (screen.getByText("图形化") as HTMLElement).click();
    await waitFor(() => {
      expect(container.querySelectorAll(".fleet-node")).toHaveLength(1);
    });
  });

  it("没有智能体时：不画任何节点或连线，只写空态原因", async () => {
    const { FleetPanel } = await import("../panels/FleetPanel");
    const { container } = render(<FleetPanel faultCounts={null} onGoRepair={() => {}} />);

    await waitFor(() => {
      expect(screen.getByText(/库里还没有任何智能体登记/)).toBeInTheDocument();
    });
    expect(container.querySelectorAll(".fleet-node")).toHaveLength(0);
    expect(container.querySelectorAll(".fleet-links")).toHaveLength(0);
    expect(container.querySelectorAll(".fleet-table")).toHaveLength(0);
  });

});

describe("AssetKindPanel · 六类资产列表", () => {
  it("按 kind 拉取并渲染状态列（权限 / 能力数 / 重复位置）", async () => {
    state.assets = [
      {
        id: "skill::a",
        kind: "skill",
        name: "读文件",
        version: "1.2",
        description: "读取本地文件",
        authority: "read",
        authority_label: "只读",
        tool_posture: "reader",
        side_effect: "none",
        role: null,
        capability_count: 3,
        source_path: "vendor/skill/a/SKILL.md",
        location_count: 2,
        location_role: "canonical",
      },
    ];
    const { AssetKindPanel } = await import("../panels/AssetKindPanel");
    const { container } = render(
      <AssetKindPanel kinds={["skill"]} onSelect={() => {}} selectedId={null} />,
    );

    await waitFor(() => expect(screen.getByText("读文件")).toBeInTheDocument());
    expect(screen.getByText("只读")).toBeInTheDocument();
    expect(screen.getByText(/⚙ 3/)).toBeInTheDocument();
    expect(screen.getByText(/⧉ 2/)).toBeInTheDocument();
    expect(container.querySelector(".asset-src")?.textContent).toContain("SKILL.md");
    // 标题里有分类色块
    expect(container.querySelector(".kind-pip")).not.toBeNull();
  });

  it("点一行触发 onSelect，且当前选中行高亮", async () => {
    state.assets = [
      {
        id: "skill::a",
        kind: "skill",
        name: "读文件",
        version: "1",
        description: "",
        authority: "read",
        authority_label: "只读",
        tool_posture: "reader",
        side_effect: "none",
        role: null,
        capability_count: 0,
        source_path: null,
      },
    ];
    const picked: string[] = [];
    const { AssetKindPanel } = await import("../panels/AssetKindPanel");
    const { container, rerender } = render(
      <AssetKindPanel
        kinds={["skill"]}
        onSelect={(item) => picked.push(item.id)}
        selectedId={null}
      />,
    );
    await waitFor(() => expect(screen.getByText("读文件")).toBeInTheDocument());

    (container.querySelector(".asset-row") as HTMLElement).click();
    expect(picked).toEqual(["skill::a"]);

    rerender(
      <AssetKindPanel kinds={["skill"]} onSelect={() => {}} selectedId="skill::a" />,
    );
    expect(container.querySelector(".asset-row.active")).not.toBeNull();
  });

  it("该类没有资产时写「暂无数据 + 原因」，不渲染空行", async () => {
    const { AssetKindPanel } = await import("../panels/AssetKindPanel");
    const { container } = render(
      <AssetKindPanel kinds={["ghost"]} onSelect={() => {}} selectedId={null} />,
    );
    await waitFor(() => {
      expect(screen.getByText(/库里没有 Ghost 类资产/)).toBeInTheDocument();
    });
    expect(container.querySelectorAll(".asset-row")).toHaveLength(0);
  });
});

describe("DetailPane · 通用属性栏", () => {
  it("按资产类型渲染身份维度、能力与来源", async () => {
    const { DetailPane } = await import("../components/DetailPane");
    render(
      <DetailPane
        loading={false}
        manifest={{
          id: "skill::a",
          kind: "skill",
          name: "读文件",
          version: "1.2",
          description: "读取本地文件",
          identity: {
            role: "reader",
            authority: "read",
            side_effect: "none",
            tool_posture: "reader",
            idempotent: true,
            open_world: false,
            dimensions: { 层: "L1" },
          },
          provides: [
            {
              capability_id: "a.read",
              name: "读取",
              summary: "读一个文件",
              tags: ["fs"],
            },
          ],
          provenance: {
            source_path: "vendor/skill/a/SKILL.md",
            source_format: "skill_md",
            content_hash: "abc123",
          },
        }}
        sources={[{ agentId: "dsh", agentName: "DeepSeek Harness", calls: 7 }]}
      />,
    );

    expect(screen.getByText("读文件")).toBeInTheDocument();
    expect(screen.getByText("只读")).toBeInTheDocument(); // authority 中文标签
    expect(screen.getByText("无（none）")).toBeInTheDocument(); // side_effect none
    expect(screen.getByText("只读（reader）")).toBeInTheDocument(); // tool_posture
    expect(screen.getByText("a.read")).toBeInTheDocument();
    expect(screen.getByText("fs")).toBeInTheDocument();
    expect(screen.getByText(/skill_md/)).toBeInTheDocument();
    expect(screen.getByText("DeepSeek Harness")).toBeInTheDocument();
    // 「实报调用 7 次」现在会在两处出现：「是否正在使用」汇总与「来源智能体」明细 ——
    // 同一个真实数字（capability_usage 反查结果）在两处呈现，不是两份数据。
    expect(screen.getAllByText(/实报调用 7 次/).length).toBeGreaterThanOrEqual(1);
    // 「是否正在使用」由 capability_usage 反查得出：有记录 → 正在使用
    expect(screen.getByText("正在使用")).toBeInTheDocument();
    expect(screen.getByText(/层: L1/)).toBeInTheDocument();
    expect(screen.getByText(/abc123/)).toBeInTheDocument();
  });

  it("零能力资产（缺 provides 键）不炸，显示「未声明任何能力」", async () => {
    const { DetailPane } = await import("../components/DetailPane");
    render(<DetailPane loading={false} manifest={{ id: "mcp::x", kind: "mcp", name: "空" }} />);
    expect(screen.getByText("该资产未声明任何能力。")).toBeInTheDocument();
  });

  it("没有选中资产时显示提示而不是空白", async () => {
    const { DetailPane } = await import("../components/DetailPane");
    render(<DetailPane loading={false} manifest={null} />);
    expect(screen.getByText(/在左侧选中一条资产/)).toBeInTheDocument();
  });
});
