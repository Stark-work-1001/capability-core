import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";

/**
 * 三个面板的渲染测试 + RenderBoundary 降级路径。
 *
 * 全部用最小 mock 数据，**不依赖真实 exe**，因此是秒级的，
 * 以后每次改前端都能立刻回归。
 *
 * bridge 被打桩：面板只认 bridge 导出的函数形状，
 * 打桩的是模块而不是网络层，所以测试的是「面板自己的逻辑」——
 * 这正是点击测试验证不了、而最容易出错的部分。
 */

type Json = Record<string, unknown>;

/** mock 状态全部走宽松类型：测试关心的是「面板拿到什么就渲染什么」。 */
const state: {
  agents: Json[];
  usage: Json[];
  staticRep: Json | null;
  hbRep: Json | null;
  diag: Json;
  matrix: Json;
  llm: Json;
  candidates: Json[];
  history: Json[];
  /** list_agent_hosts 的返回；missingHosts=true 时模拟「命令还没注册」 */
  hosts: Json[];
  missingHosts: boolean;
} = {
  agents: [],
  usage: [],
  staticRep: null,
  hbRep: null,
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
  matrix: {
    rows: [],
    total_assets: 0,
    total_capabilities: 0,
    total_passed: 0,
    total_failed: 0,
  },
  llm: {
    config_path: "D:/x/llm-providers.json",
    config_exists: false,
    providers: [],
    implemented: 0,
    note: "尚未配置任何模型 provider。",
  },
  candidates: [],
  history: [],
  hosts: [],
  missingHosts: false,
};

const decided: string[] = [];

vi.mock("../bridge", () => ({
  isTauri: false,
  listAssets: vi.fn(async () => ({items: [], total: 0})),
  runtime: "tauri",
  listAgents: vi.fn(async () => state.agents),
  capabilityUsage: vi.fn(async () => state.usage),
  registerStaticAgents: vi.fn(async () => state.staticRep),
  scanHeartbeats: vi.fn(async () => state.hbRep),
  diagnostics: vi.fn(async () => state.diag),
  protocolMatrix: vi.fn(async () => state.matrix),
  llmStatus: vi.fn(async () => state.llm),
  listDedupCandidates: vi.fn(async () => state.candidates),
  dedupHistory: vi.fn(async () => state.history),
  listAgentHosts: vi.fn(async () => {
    if (state.missingHosts) {
      throw "Command list_agent_hosts not found";
    }
    return state.hosts;
  }),
  setAssetLifecycle: vi.fn(async () => true),
  isMissingCommandError: (e: unknown) =>
    /not found|unknown command/i.test(String(e)),
  decideDedup: vi.fn(async (g: string, l: string, d: string) => {
    decided.push(g + "|" + l + "|" + d);
    return true;
  }),
}));

function resetState() {
  state.agents = [];
  state.usage = [];
  state.staticRep = null;
  state.hbRep = null;
  state.candidates = [];
  state.history = [];
  state.hosts = [];
  state.missingHosts = false;
  decided.length = 0;
}

beforeEach(() => {
  resetState();
});

describe("OverviewPanel · 总览", () => {
  it("空数据时显示「暂无数据 + 原因」，而不是占位数字", async () => {
    const { OverviewPanel } = await import("../panels/OverviewPanel");
    render(<OverviewPanel stats={null} summary={null} faultCounts={null} onGoRepair={() => {}} />);

    await waitFor(() => {
      expect(screen.getByText(/库里还没有任何智能体登记/)).toBeInTheDocument();
    });
    expect(screen.getByText(/还没有任何智能体声明或拉取能力/)).toBeInTheDocument();
    expect(screen.getByText(/还没有任何能力完成宿主归属/)).toBeInTheDocument();
    expect(screen.getByText(/尚未运行诊断/)).toBeInTheDocument();
  });

  it("有真实数据时渲染在线智能体与能力用量", async () => {
    state.agents = [
      {
        agent_id: "dsh",
        display_name: "DeepSeek Harness",
        ecosystem: "dsh",
        kind: "host",
        transport: "heartbeat",
        endpoint: "http://127.0.0.1:1",
        first_seen: 1,
        last_seen: 2,
        meta: "",
        heartbeat: {
          host: "dsh", version: "0.2.1", pid: 42, last_beat: 2, age_ms: 1000,
          source_file: "hb.json", state: "online", capabilities: ["read"],
        },
        capability_count: 1,
        call_total: 0,
        last_error: "",
      },
    ];
    state.usage = [
      {
        agent_id: "dsh", agent_name: "DeepSeek Harness", ecosystem: "dsh",
        capability_id: "dsh.read", asset_id: "skill::x", asset_name: "读文件",
        source: "mcp", call_count: 7, first_used_at: 1, last_used_at: 2, last_error: "",
      },
    ];
    const { OverviewPanel } = await import("../panels/OverviewPanel");
    render(<OverviewPanel stats={null} summary={null} faultCounts={{ critical: 1, error: 2, warning: 3 }} onGoRepair={() => {}} />);

    await waitFor(() => {
      // 名称会同时出现在「在线列表」与「能力用量列表」两处，故用 getAllByText
      expect(screen.getAllByText("DeepSeek Harness").length).toBeGreaterThan(0);
    });
    expect(screen.getByText(/在线 1 \/ 登记 1/)).toBeInTheDocument();
    expect(screen.getByText(/1 条/)).toBeInTheDocument();
    expect(screen.getByText("dsh.read")).toBeInTheDocument();
    // 故障计数来自 props 的真实值，定位到该卡片内避免歧义
    const faultCard = screen.getByText("④ 故障能力报错").closest("section")!;
    expect(faultCard).toHaveTextContent("严重");
    expect(faultCard).toHaveTextContent("1");
    expect(faultCard).toHaveTextContent("2");
    expect(faultCard).toHaveTextContent("3");
  });

  it("点「在维修站查看详情」触发回调", async () => {
    let hit = false;
    const { OverviewPanel } = await import("../panels/OverviewPanel");
    render(<OverviewPanel stats={null} summary={null} faultCounts={null} onGoRepair={() => { hit = true; }} />);
    await waitFor(() => expect(screen.getByText(/在维修站查看详情/)).toBeInTheDocument());
    (screen.getByText(/在维修站查看详情/) as HTMLElement).click();
    expect(hit).toBe(true);
  });
});

describe("RepairPanel · 维修站", () => {
  it("空诊断时给出空态，且去重区显示「暂无去重候选」", async () => {
    const { RepairPanel } = await import("../panels/RepairPanel");
    render(<RepairPanel />);
    await waitFor(() => {
      expect(screen.getByText(/暂无去重候选/)).toBeInTheDocument();
    });
    expect(screen.getByText("故障清单")).toBeInTheDocument();
    expect(screen.getByText("去重裁决")).toBeInTheDocument();
  });

  it("诊断有故障时按分类分组渲染，并标出自修档位", async () => {
    state.diag = {
      diagnostics: [
        {
          id: "d1",
          category: "full_text_index",
          severity: "warning",
          title: "全文索引缺失",
          subject: "tantivy 索引",
          detail: "索引目录为空",
          plan: { action: "rebuild_search_index", scope: "hub_owned", destructive: false, summary: "重建索引", changes: ["重建 tantivy 索引"] },
          needs_llm: false,
          needs_llm_reason: "",
        },
        {
          id: "d2",
          category: "asset_parse",
          severity: "error",
          title: "解析失败",
          subject: "vendor/x",
          detail: "YAML 语法错误",
          plan: { action: "repair_source_file", scope: "user_files", destructive: false, summary: "改你的源文件", changes: ["修 vendor/x/SKILL.md"] },
          needs_llm: true,
          needs_llm_reason: "需要判断该保留哪一处",
        },
      ],
      critical: 0, error: 1, warning: 1, info: 0,
      auto_fixable: 1, needs_confirm: 1, manual_only: 0, needs_llm: 1,
    };
    const { RepairPanel } = await import("../panels/RepairPanel");
    render(<RepairPanel />);
    await waitFor(() => expect(screen.getByText("全文索引缺失或损坏")).toBeInTheDocument());
    expect(screen.getByText("资产解析失败")).toBeInTheDocument();

    // 自修档位：hub_owned + 非破坏性 → auto；user_files → confirm
    const autoTag = document.querySelector(".fix-tag.auto");
    expect(autoTag).not.toBeNull();
    expect(autoTag).toHaveTextContent("可自动修");
    const confirmTag = document.querySelector(".fix-tag.confirm");
    expect(confirmTag).not.toBeNull();
    expect(confirmTag).toHaveTextContent("需你确认");

    // 作用域必须显式呈现：中枢自己的数据 vs 用户的源文件
    expect(screen.getByText(/作用域：中枢自己的数据/)).toBeInTheDocument();
    expect(screen.getByText(/作用域：用户的源文件/)).toBeInTheDocument();
    expect(screen.getByText("重建 tantivy 索引")).toBeInTheDocument();
    expect(screen.getByText("修 vendor/x/SKILL.md")).toBeInTheDocument();
    expect(screen.getByText("为什么求援：需要判断该保留哪一处")).toBeInTheDocument();
  });

  it("去重候选渲染三个裁决按钮，点击真的上报", async () => {
    state.candidates = [
      {
        group_key: "l1-abc123",
        match_level: "L1",
        suggested_canonical: "loc-1",
        location_ids: ["loc-1", "loc-2"],
      },
    ];
    const { RepairPanel } = await import("../panels/RepairPanel");
    render(<RepairPanel />);
    await waitFor(() => expect(screen.getByText(/54 组待确认|1 组待确认/)).toBeInTheDocument());

    const merge = screen.getByText("合并") as HTMLElement;
    const keep = screen.getByText("共存") as HTMLElement;
    const ign = screen.getByText("忽略") as HTMLElement;
    expect(merge).toBeInTheDocument();
    expect(keep).toBeInTheDocument();
    expect(ign).toBeInTheDocument();

    merge.click();
    await waitFor(() => expect(decided).toContain("l1-abc123|L1|merge"));
    keep.click();
    await waitFor(() => expect(decided).toContain("l1-abc123|L1|keep_both"));
    ign.click();
    await waitFor(() => expect(decided).toContain("l1-abc123|L1|ignore"));
  });
});

describe("ModelsPanel · 模型接入", () => {
  it("空 provider 时显示「暂无数据 + 原因」", async () => {
    const { ModelsPanel } = await import("../panels/ModelsPanel");
    render(<ModelsPanel />);
    await waitFor(() => {
      expect(screen.getByText(/还没有配置任何模型 provider/)).toBeInTheDocument();
    });
    expect(screen.getByText("LLM 接入位")).toBeInTheDocument();
    // 「协议支持矩阵」这张卡片本轮搬到了总览分区（见下面的 HostsPanel 用例）。
    // 模型配置面板里只留一句指路，不再放第二份表 —— 断言这句话确实在。
    expect(screen.getByText(/已移到「总览」分区/)).toBeInTheDocument();
  });
});

describe("HostsPanel · 协议支持矩阵（从模型配置搬到总览）", () => {
  it("协议矩阵表格搬到这里：同样的数据、同样的成功率口径", async () => {
    state.matrix = {
      rows: [
        {
          protocol: "skill_md", supported: true, assets: 460, capabilities: 480,
          passed_assets: 460, failed_assets: 0, passed_capabilities: 480, failed_capabilities: 0,
          success_rate: 1, top_reasons: [],
        },
        {
          protocol: "mcp_json", supported: true, assets: 11, capabilities: 0,
          passed_assets: 11, failed_assets: 0, passed_capabilities: 0, failed_capabilities: 0,
          success_rate: 0, top_reasons: ["无能力条目"],
        },
      ],
      total_assets: 506, total_capabilities: 495, total_passed: 491, total_failed: 4,
    };
    const { HostsPanel } = await import("../panels/HostsPanel");
    render(<HostsPanel />);
    await waitFor(() => expect(screen.getByText("SKILL.md（frontmatter）")).toBeInTheDocument());
    expect(screen.getByText(".mcp.json")).toBeInTheDocument();
    expect(screen.getByText("100%")).toBeInTheDocument();
    // 没有能力样本时显示「无样本」，不是 100%
    expect(screen.getByText("无样本")).toBeInTheDocument();
    expect(screen.getByText("506")).toBeInTheDocument();
    expect(screen.getByText("495")).toBeInTheDocument();
  });

  it("有宿主数据时画出 智能体 × 协议 表格，三档支持状态各不相同", async () => {
    state.hosts = [
      {
        id: "claude-code",
        display_name: "Claude Code",
        vendor: "Anthropic",
        kind: "cli",
        detected: true,
        connected: true,
        protocols: [
          { name: "skill-md", transport: "skill-md", support: "supported" },
          { name: "mcp-http", transport: "mcp-http", support: "partial" },
        ],
        evidence: ["~/.claude/plugins 存在"],
      },
      {
        id: "qoder",
        display_name: "Qoder",
        vendor: "Alibaba",
        kind: "plugin",
        detected: false,
        connected: false,
        protocols: [{ name: "skill-md", transport: "skill-md", support: "planned" }],
        evidence: ["未找到插件目录"],
      },
    ];
    const { HostsPanel } = await import("../panels/HostsPanel");
    const { container } = render(<HostsPanel />);
    await waitFor(() =>
      expect(container.querySelectorAll(".hosts-matrix tbody tr")).toHaveLength(2),
    );
    // 每个智能体一行，协议是列（两个宿主声明过的协议并集）
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
    expect(screen.getByText("Qoder")).toBeInTheDocument();
    expect(container.querySelectorAll(".hosts-matrix thead .proto-col")).toHaveLength(2);
    // 三档状态真的分别渲染出来，不是一个是非勾
    expect(container.querySelector(".sup-cell.sup-ok")!.textContent).toBe("支持");
    expect(container.querySelector(".sup-cell.sup-partial")!.textContent).toBe("部分");
    expect(container.querySelector(".sup-cell.sup-planned")!.textContent).toBe("计划");
    // 证据列**只放摘要**：完整证据在右侧属性栏（本轮改动）。
    // 表格里不能再出现那条长证据原文 —— 它正是把行撑成多行的原因。
    expect(container.querySelector(".hosts-matrix .evidence-count")!.textContent).toBe(
      "1 条",
    );
    expect(
      container.querySelector(".hosts-matrix")!.textContent,
    ).not.toContain("~/.claude/plugins 存在");
    // 每个宿主都有一格证据摘要；两个宿主各 1 条
    expect(container.querySelectorAll(".hosts-matrix .evidence-cell")).toHaveLength(2);
    expect(
      [...container.querySelectorAll(".hosts-matrix .evidence-count")].map(
        (n) => n.textContent,
      ),
    ).toEqual(["1 条", "1 条"]);
  });

  it("点某一行 → onSelect 收到该宿主；选中行高亮（右侧属性栏据此显示证据）", async () => {
    state.hosts = [
      {
        id: "claude-code",
        display_name: "Claude Code",
        vendor: "Anthropic",
        kind: "cli",
        detected: true,
        connected: true,
        protocols: [{ name: "mcp-http", transport: "mcp-http", support: "supported" }],
        evidence: ["~/.claude/plugins 存在", "PATH 里有 claude 命令"],
      },
      {
        id: "qoder",
        display_name: "Qoder",
        vendor: "Alibaba",
        kind: "plugin",
        detected: false,
        connected: false,
        protocols: [],
        evidence: [],
      },
    ];
    const picked: { id: string }[] = [];
    const { HostsPanel } = await import("../panels/HostsPanel");
    const { container, rerender } = render(
      <HostsPanel onSelect={(h) => picked.push({ id: h.id })} selectedId={null} />,
    );
    await waitFor(() =>
      expect(container.querySelectorAll(".hosts-matrix tbody tr")).toHaveLength(2),
    );
    expect(container.querySelectorAll(".hosts-matrix .evidence-count")).toHaveLength(1);
    expect(container.querySelector(".hosts-matrix .evidence-count")!.textContent).toBe(
      "2 条",
    );

    (container.querySelector(".hosts-matrix tbody tr") as HTMLElement).click();
    expect(picked.map((p) => p.id)).toEqual(["claude-code"]);

    // 选中行高亮：.active 类挂在对应的 tr 上
    rerender(<HostsPanel onSelect={() => {}} selectedId="claude-code" />);
    await waitFor(() =>
      expect(
        container.querySelector(".hosts-matrix tbody tr.active"),
      ).not.toBeNull(),
    );
    expect(
      container.querySelector(".hosts-matrix tbody tr.active")!.getAttribute("aria-selected"),
    ).toBe("true");
  });

  it("后端没有 list_agent_hosts 时：明说缺什么，一行表格都不画（不伪造数据）", async () => {
    state.missingHosts = true;
    const { HostsPanel } = await import("../panels/HostsPanel");
    const { container } = render(<HostsPanel />);
    await waitFor(() => {
      expect(screen.getByText(/后端尚未提供 list_agent_hosts/)).toBeInTheDocument();
    });
    // 关键：不许画一张假表
    expect(container.querySelector(".hosts-matrix")).toBeNull();
    expect(container.querySelectorAll(".sup-cell")).toHaveLength(0);
    // 但要说清「本轮正在接入」而不是干瘪报错
    expect(screen.getByText(/本轮正在接入/)).toBeInTheDocument();
  });
});

/**
 * RenderBoundary 的降级路径 —— 上一轮 506 条点击**从未验证过**这个分支。
 * 下面用一个必然在 render 期抛错的组件把它真正触发一次。
 */
describe("RenderBoundary · 降级路径（上一轮从未被验证过）", () => {
  it("子组件抛错时显示可见错误卡片，而不是整窗白屏", async () => {
    const { RenderBoundary } = await import("../App");

    function Boom(): JSX.Element {
      throw new Error("故意抛错：模拟渲染期异常");
    }

    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <RenderBoundary label="测试面板">
        <Boom />
      </RenderBoundary>,
    );

    await waitFor(() => {
      expect(screen.getByRole("alert")).toBeInTheDocument();
    });
    expect(screen.getByText(/测试面板渲染失败/)).toBeInTheDocument();
    expect(screen.getByText(/故意抛错：模拟渲染期异常/)).toBeInTheDocument();
    expect(screen.getByText(/其余面板不受影响/)).toBeInTheDocument();
    spy.mockRestore();
  });

  it("不抛错时正常渲染子组件", async () => {
    const { RenderBoundary } = await import("../App");
    render(
      <RenderBoundary label="测试面板">
        <div>一切正常</div>
      </RenderBoundary>,
    );
    expect(screen.getByText("一切正常")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
