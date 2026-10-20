import { describe, expect, it } from "vitest";
import {
  UNASSIGNED,
  buildFleetGraph,
  fleetLinks,
  layoutFleet,
  type FleetGeometry,
} from "../fleet-model";
import type { AgentRecord, UsageRow } from "../panel-types";

/**
 * 图形化分派关系的派生逻辑测试。
 *
 * 核心纪律：**图必须真的从数据来**。这组用例钉住三件事：
 *  1. 没有数据 → 一个分组都没有（界面据此显示「暂无数据 + 原因」）；
 *  2. 有数据 → 节点 / chip / 数量全部逐条对应真实行；
 *  3. 没有 chip 就不生成连线（不画假线）。
 */

function agent(over: Partial<AgentRecord>): AgentRecord {
  return {
    agent_id: "a1",
    display_name: "智能体一",
    ecosystem: "dsh",
    kind: "host",
    transport: "heartbeat",
    endpoint: "",
    first_seen: 0,
    last_seen: 0,
    meta: "",
    heartbeat: null,
    capability_count: 0,
    call_total: 0,
    last_error: "",
    ...over,
  };
}

function usage(over: Partial<UsageRow>): UsageRow {
  return {
    agent_id: "a1",
    agent_name: "智能体一",
    ecosystem: "dsh",
    capability_id: "cap.one",
    asset_id: "skill::x",
    asset_name: "读文件",
    source: "mcp",
    call_count: 0,
    first_used_at: 0,
    last_used_at: 0,
    last_error: "",
    ...over,
  };
}

describe("buildFleetGraph · 空数据不画假图", () => {
  it("既没有智能体也没有用量时，分组为空", () => {
    const g = buildFleetGraph([], []);
    expect(g.groups).toEqual([]);
    expect(g.capabilityTotal).toBe(0);
    expect(g.reportedTotal).toBe(0);
  });

  it("有智能体但没有用量时，节点存在但不产生任何能力 chip", () => {
    const g = buildFleetGraph([agent({})], []);
    expect(g.groups).toHaveLength(1);
    expect(g.groups[0].agents).toHaveLength(1);
    expect(g.groups[0].agents[0].capabilities).toEqual([]);
    expect(g.capabilityTotal).toBe(0);
  });
});

describe("buildFleetGraph · 从真实行派生", () => {
  it("每条 usage 变成对应智能体的一个能力 chip", () => {
    const g = buildFleetGraph(
      [agent({}), agent({ agent_id: "a2", display_name: "智能体二" })],
      [
        usage({ capability_id: "cap.one" }),
        usage({ capability_id: "cap.two", call_count: 5 }),
      ],
    );
    expect(g.groups).toHaveLength(1);
    expect(g.groups[0].ecosystem).toBe("dsh");
    const a1 = g.groups[0].agents.find((a) => a.agentId === "a1")!;
    expect(a1.capabilities.map((c) => c.label)).toEqual(["cap.two", "cap.one"]);
    expect(a1.calls).toBe(5);
    expect(g.capabilityTotal).toBe(2);
  });

  it("同一 (agent, capability) 的多行累加调用次数，而不是重复成两个 chip", () => {
    const g = buildFleetGraph(
      [agent({})],
      [
        usage({ capability_id: "cap.one", call_count: 2 }),
        usage({ capability_id: "cap.one", call_count: 3 }),
      ],
    );
    const caps = g.groups[0].agents[0].capabilities;
    expect(caps).toHaveLength(1);
    expect(caps[0].calls).toBe(5);
  });

  it("只出现在 usage 里的 agent_id 也建节点（历史行不从图上消失）", () => {
    const g = buildFleetGraph(
      [],
      [usage({ agent_id: "ghost-agent", agent_name: "已退场宿主" })],
    );
    expect(g.groups).toHaveLength(1);
    expect(g.groups[0].agents[0].agentId).toBe("ghost-agent");
    expect(g.groups[0].agents[0].state).toBe("unknown");
  });

  it("生态为空时归入 unassigned，而不是猜一个宿主", () => {
    const g = buildFleetGraph([agent({ ecosystem: "" })], []);
    expect(g.groups[0].ecosystem).toBe(UNASSIGNED);
  });

  it("reportedTotal 只数 A 通道实报，static 推断不算实测", () => {
    const g = buildFleetGraph(
      [agent({})],
      [
        usage({ capability_id: "c1", source: "mcp" }),
        usage({ capability_id: "c2", source: "static" }),
      ],
    );
    expect(g.reportedTotal).toBe(1);
    expect(g.capabilityTotal).toBe(2);
  });

  it("错误计数按 chip 的 last_error 统计", () => {
    const g = buildFleetGraph(
      [agent({})],
      [
        usage({ capability_id: "c1", last_error: "boom" }),
        usage({ capability_id: "c2" }),
      ],
    );
    expect(g.errorTotal).toBe(1);
    expect(g.healthyTotal).toBe(1);
  });

  it("心跳状态被映射到节点的 state", () => {
    const online = buildFleetGraph(
      [
        agent({
          heartbeat: {
            host: "h",
            version: "1",
            pid: 1,
            last_beat: 1,
            age_ms: 10,
            source_file: "x",
            state: "online",
            capabilities: [],
          },
        }),
      ],
      [],
    );
    expect(online.groups[0].agents[0].state).toBe("online");
  });
});

describe("layoutFleet / fleetLinks · 连线只画真实存在的关系", () => {
  it("没有 chip 时连线为空", () => {
    const g = buildFleetGraph([agent({})], []);
    const layout = layoutFleet(g.groups);
    expect(layout.groups[0].links).toEqual([]);
  });

  it("每个 chip 恰好一条连线，坐标落在布局范围内", () => {
    const g = buildFleetGraph(
      [agent({})],
      [
        usage({ capability_id: "c1" }),
        usage({ capability_id: "c2" }),
        usage({ capability_id: "c3" }),
      ],
    );
    const layout = layoutFleet(g.groups);
    const group = layout.groups[0];
    expect(group.links).toHaveLength(3);
    expect(group.blocks[0].chips).toHaveLength(3);
    // chip 的 y 单调递增，且都在分组高度内
    const ys = group.blocks[0].chips.map((c) => c.top);
    expect(ys).toEqual([...ys].sort((a, b) => a - b));
    for (const y of ys) {
      expect(y).toBeGreaterThanOrEqual(0);
      expect(y + layout.chipHeight).toBeLessThanOrEqual(group.height + 1);
    }
    for (const l of group.links) {
      expect(l.d.startsWith("M ")).toBe(true);
      expect(l.d).toContain(" C ");
    }
  });

  it("多智能体的块不重叠（每个块的 chips 落在自己的纵向区间）", () => {
    const g = buildFleetGraph(
      [
        agent({ agent_id: "a1" }),
        agent({ agent_id: "a2", display_name: "二" }),
      ],
      [
        usage({ agent_id: "a1", capability_id: "c1" }),
        usage({ agent_id: "a2", capability_id: "c2" }),
        usage({ agent_id: "a2", capability_id: "c3" }),
      ],
    );
    const layout = layoutFleet(g.groups);
    const [b0, b1] = layout.groups[0].blocks;
    expect(b1.top).toBeGreaterThanOrEqual(b0.top + b0.height);
  });

  it("fleetLinks 是纯函数：给定空 chips 一定返回空数组", () => {
    expect(fleetLinks([{ cardY: 10, chips: [] }], 100, 140, "#fff")).toEqual([]);
  });

  it("几何参数可注入（便于将来做密度三档而不改布局代码）", () => {
    const geo: FleetGeometry = {
      cardWidth: 200,
      gutter: 20,
      chipHeight: 24,
      chipGap: 2,
      cardHeight: 48,
      blockGap: 8,
    };
    const g = buildFleetGraph([agent({})], [usage({ capability_id: "c1" })]);
    const layout = layoutFleet(g.groups, geo);
    expect(layout.cardWidth).toBe(200);
    expect(layout.chipLeft).toBe(220);
    expect(layout.chipHeight).toBe(24);
  });
});
