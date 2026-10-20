/**
 * 图形化「智能体 → 能力」分派关系的**纯派生**逻辑。
 *
 * 为什么单独一个文件：图必须**真的从数据来**。
 * 布局计算抽在这里，就能用最小 mock 数据钉成单测 ——
 * 界面上画的每一条连线都对应一条真实数据行，没数据就一个节点都不画。
 *
 * 视觉形态参考本机 dsh-context 插件的 Fleet（编队）tab 线框图
 * —— 分组行 + 组内卡片 + SVG 贝塞尔连线 + chip + 状态点。
 * 只借形态：本文件不 import 它、不读它的数据、不用它的 CSS 令牌。
 */

import type { AgentRecord, UsageRow } from "./panel-types";

/** 图上一个能力 chip。 */
export interface FleetChip {
  label: string;
  calls: number;
  source: string;
  lastError: string;
  assetName: string;
}

/** 图上一个智能体节点。 */
export interface FleetAgentNode {
  agentId: string;
  label: string;
  ecosystem: string;
  transport: string;
  state: "online" | "stale" | "offline" | "unknown";
  capabilities: FleetChip[];
  declaredCapabilities: number;
  calls: number;
}

/** 图上一行：同一个生态（ecosystem）的智能体归一组。 */
export interface FleetGroup {
  ecosystem: string;
  agents: FleetAgentNode[];
}

export interface FleetGraph {
  groups: FleetGroup[];
  capabilityTotal: number;
  reportedTotal: number;
  healthyTotal: number;
  errorTotal: number;
}

const MAX_CHIPS_PER_AGENT = 24;

/** 无法归属生态时的分组键。界面上会显式写「未归属」，不硬塞给某个宿主。 */
export const UNASSIGNED = "unassigned";

function stateOf(a: AgentRecord): FleetAgentNode["state"] {
  if (a.transport === "mcp" && a.last_seen > 0) {
    const age = Date.now() / 1000 - a.last_seen;
    if (age < -30) return "unknown";
    return age <= 90 ? "online" : age <= 900 ? "stale" : "offline";
  }
  if (!a.heartbeat) return "unknown";
  const s = a.heartbeat.state;
  return s === "online" || s === "stale" || s === "offline" ? s : "unknown";
}

/**
 * 从 list_agents + capability_usage 派生分组图。
 *
 * 规则：
 * 1. 每条 usage 行归到 (agent_id, capability_id)；同一对出现多次只累加调用次数。
 * 2. 智能体即使没有 usage 行也保留节点（显示「未拉取」），不隐藏。
 * 3. 生态取自 AgentRecord.ecosystem；为空的归入 UNASSIGNED，不猜。
 * 4. 只出现在 usage 里、不在 list_agents 里的 agent_id（历史行 / 已退场宿主）
 *    也建节点并标 unknown —— 否则那部分调用会从图上凭空消失。
 */
export function buildFleetGraph(
  agents: AgentRecord[],
  usage: UsageRow[],
): FleetGraph {
  const nodes = new Map<string, FleetAgentNode>();
  const chipIndex = new Map<string, Map<string, FleetChip>>();

  const ensure = (
    agentId: string,
    seed: Partial<FleetAgentNode>,
  ): FleetAgentNode => {
    let n = nodes.get(agentId);
    if (!n) {
      n = {
        agentId,
        label: seed.label || agentId,
        ecosystem: seed.ecosystem || UNASSIGNED,
        transport: seed.transport || "static",
        state: seed.state || "unknown",
        capabilities: [],
        declaredCapabilities: seed.declaredCapabilities ?? 0,
        calls: 0,
      };
      nodes.set(agentId, n);
      chipIndex.set(agentId, new Map());
    }
    return n;
  };

  for (const a of agents) {
    ensure(a.agent_id, {
      label: a.display_name || a.agent_id,
      ecosystem: a.ecosystem,
      transport: a.transport,
      state: stateOf(a),
      declaredCapabilities: a.capability_count ?? 0,
    });
  }

  for (const u of usage) {
    const agentId = u.agent_id || "(未命名智能体)";
    const node = ensure(agentId, {
      label: u.agent_name || agentId,
      ecosystem: u.ecosystem,
      transport: u.source,
    });
    const capId = u.capability_id || "(未命名能力)";
    const perAgent = chipIndex.get(agentId)!;
    const chip = perAgent.get(capId);
    if (chip) {
      chip.calls += u.call_count ?? 0;
      if (!chip.lastError && u.last_error) chip.lastError = u.last_error;
      if (!chip.assetName && u.asset_name) chip.assetName = u.asset_name;
    } else {
      perAgent.set(capId, {
        label: capId,
        calls: u.call_count ?? 0,
        source: u.source,
        lastError: u.last_error || "",
        assetName: u.asset_name || "",
      });
    }
    node.calls += u.call_count ?? 0;
  }

  for (const node of nodes.values()) {
    const chips = [...(chipIndex.get(node.agentId)?.values() ?? [])];
    // 实报调用多的排前面；同次数按能力 id 排序，保证渲染顺序稳定（截图可复现）。
    chips.sort((a, b) => b.calls - a.calls || a.label.localeCompare(b.label));
    node.capabilities = chips.slice(0, MAX_CHIPS_PER_AGENT);
  }

  const groupMap = new Map<string, FleetAgentNode[]>();
  for (const node of nodes.values()) {
    const key = node.ecosystem || UNASSIGNED;
    const arr = groupMap.get(key) ?? [];
    arr.push(node);
    groupMap.set(key, arr);
  }

  const groups: FleetGroup[] = [...groupMap.entries()]
    .map(([ecosystem, list]) => ({
      ecosystem,
      agents: list.sort(
        (a, b) =>
          b.capabilities.length - a.capabilities.length ||
          a.agentId.localeCompare(b.agentId),
      ),
    }))
    .sort(
      (a, b) =>
        b.agents.length - a.agents.length ||
        a.ecosystem.localeCompare(b.ecosystem),
    );

  const allCaps = new Set<string>();
  let reportedTotal = 0;
  let healthyTotal = 0;
  let errorTotal = 0;
  for (const node of nodes.values()) {
    for (const chip of node.capabilities) {
      allCaps.add(chip.label);
      if (chip.source === "mcp") reportedTotal += 1;
      if (chip.lastError) errorTotal += 1;
      else healthyTotal += 1;
    }
  }

  return {
    groups,
    capabilityTotal: allCaps.size,
    reportedTotal,
    healthyTotal,
    errorTotal,
  };
}

/** 一条 SVG 贝塞尔连线。 */
export interface FleetLink {
  d: string;
  color: string;
  calls: number;
}

/**
 * 生成「智能体 → 能力」的贝塞尔连线。
 *
 * 坐标系统：SVG 覆盖在整组上。卡片在左（右缘 x = cardRight），
 * 能力 chip 在右（左缘 x = chipLeft）。连线从卡片右缘中线出发，
 * 向 chip 纵向发散，纵向切线 + 曲率随落差增大但夹在 [18, 64]。
 *
 * 纯函数，方便单测钉住「不画假线」：没有 chip 就没有 path。
 */
export function fleetLinks(
  rows: { cardY: number; chips: { y: number; calls: number }[] }[],
  cardRight: number,
  chipLeft: number,
  color: string,
): FleetLink[] {
  const out: FleetLink[] = [];
  for (const row of rows) {
    for (const chip of row.chips) {
      const x1 = cardRight;
      const y1 = row.cardY;
      const x2 = chipLeft;
      const y2 = chip.y;
      const k = Math.min(64, Math.max(18, Math.abs(y2 - y1) * 0.5 + 12));
      out.push({
        d: "M " + x1 + " " + y1 + " C " + (x1 + k) + " " + y1 + ", " + (x2 - k) + " " + y2 + ", " + x2 + " " + y2,
        color,
        calls: chip.calls,
      });
    }
  }
  return out;
}

// ── 图形化布局 ─────────────────────────────────────────────
//
// 布局是**纯函数**：没有任何测量、没有 ResizeObserver。
// 每个块的 y 坐标由「它有多少个能力 chip」直接算出来，
// 所以图上的每根连线都能追溯到一条真实 usage 行；
// 没有数据时 groups 为空，一根线都不画。

export interface FleetGeometry {
  /** 智能体卡片宽 */
  cardWidth: number;
  /** 卡片右缘到 chip 左缘的连线区宽 */
  gutter: number;
  chipHeight: number;
  chipGap: number;
  cardHeight: number;
  blockGap: number;
}

export const FLEET_GEOMETRY: FleetGeometry = {
  cardWidth: 232,
  gutter: 40,
  chipHeight: 28,
  chipGap: 4,
  cardHeight: 60,
  blockGap: 12,
};

export interface FleetLayoutChip {
  /** chip 相对分组顶部的 y */
  top: number;
  label: string;
  calls: number;
  source: string;
  hasError: boolean;
  assetName: string;
}

export interface FleetLayoutBlock {
  agent: FleetAgentNode;
  top: number;
  height: number;
  chips: FleetLayoutChip[];
}

export interface FleetLayoutGroup {
  ecosystem: string;
  color: string;
  height: number;
  blocks: FleetLayoutBlock[];
  links: FleetLink[];
}

export interface FleetLayout {
  groups: FleetLayoutGroup[];
  cardWidth: number;
  cardRight: number;
  chipLeft: number;
  chipHeight: number;
}

/** 分组配色。按分组序号取色，**不来自数据**，只用于区分行。 */
export const FLEET_GROUP_COLORS = [
  "#5aa9ff",
  "#43e0a6",
  "#ffc94d",
  "#c084fc",
  "#f472b6",
  "#fb923c",
];

export function groupColor(index: number): string {
  return FLEET_GROUP_COLORS[index % FLEET_GROUP_COLORS.length];
}

export function layoutFleet(
  groups: FleetGroup[],
  geo: FleetGeometry = FLEET_GEOMETRY,
): FleetLayout {
  const cardRight = geo.cardWidth;
  const chipLeft = geo.cardWidth + geo.gutter;
  const step = geo.chipHeight + geo.chipGap;
  const laid: FleetLayoutGroup[] = [];

  groups.forEach((group, gi) => {
    const color = groupColor(gi);
    const blocks: FleetLayoutBlock[] = [];
    let y = 0;
    for (const agent of group.agents) {
      const chips: FleetLayoutChip[] = agent.capabilities.map((c, ci) => ({
        top: y + ci * step,
        label: c.label,
        calls: c.calls,
        source: c.source,
        hasError: c.lastError !== "",
        assetName: c.assetName,
      }));
      const chipsHeight =
        chips.length > 0 ? chips.length * step - geo.chipGap : 0;
      const height = Math.max(geo.cardHeight, chipsHeight);
      blocks.push({ agent, top: y, height, chips });
      y += height + geo.blockGap;
    }
    const height = blocks.length > 0 ? y - geo.blockGap : 0;
    const links = fleetLinks(
      blocks.map((b) => ({
        cardY: b.top + geo.cardHeight / 2,
        chips: b.chips.map((c) => ({ y: c.top + geo.chipHeight / 2, calls: c.calls })),
      })),
      cardRight,
      chipLeft,
      color,
    );
    laid.push({ ecosystem: group.ecosystem, color, height, blocks, links });
  });

  return { groups: laid, cardWidth: geo.cardWidth, cardRight, chipLeft, chipHeight: geo.chipHeight };
}


