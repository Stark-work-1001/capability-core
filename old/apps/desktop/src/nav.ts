/**
 * 两级导航：分区（Section）→ 侧栏（Sidebar）。
 *
 * 用户原话：「侧栏新增分区，分区下级为侧栏……分区分别包含总览分区、
 * 维修站分区、资产分区、模型配置分区、新增能力」。
 *
 * 视觉形态参考本机 dsh-context 插件的 Fleet（编队）tab 线框图
 * —— 分组行、状态点、chip、层级连线。这里**只借视觉形态**：
 * 不 import 它的任何模块、不读它的运行时数据、不使用它的 CSS 令牌。
 *
 * 每条侧栏都带 `dataPath`：界面右侧属性栏会原样显示它接的是哪条
 * 真实数据通路；本轮没有后端命令的，写「未接」，绝不拿假数据冒充分派结果。
 *
 * ## 第二轮（本轮）的结构变化
 *
 * 1. **分区顺序**：总览 → 资产 → 模型配置 → 新增能力 → 维修站。
 *    用户要求「维修站分区及其侧栏分组移到新增能力分区及其侧栏分组下面」。
 * 2. **资产分区合并 plugin + suite**：两条合成一条 `asset:plugin+suite`，
 *    列表内部对两个 kind 各发一次 `list_assets`，total 照实拆开显示。
 * 3. **维修站新增「去重」**：原左下角「待确认去重 27」的具体内容搬到这里，
 *    界面上的去重裁决按钮只此一份。
 * 4. **总览新增「协议支持矩阵」**：原「模型配置」里那张协议支持矩阵卡片
 *    独立成组搬过来，并扩成「各智能体的协议清单 + 支持状态」。
 */

import { ASSET_KINDS, type AssetKindKey } from "./types";

export type SectionId = "overview" | "assets" | "models" | "create" | "repair";

/** 非资产类侧栏的 id。资产类用 `asset:<kind>` / `asset:plugin+suite` 模板 id。 */
export type StaticSidebarId =
  | "fleet"
  | "hosts"
  | "pull"
  | "search"
  | "faults"
  | "solutions"
  | "fixes"
  | "dedup"
  | "routing"
  | "providers"
  | "new-capability";

/**
 * 合并后的资产侧栏 id：Plugin 与 Suite 共用一个列表（两个 kind 都列）。
 *
 * 用常量而不是裸字符串，避免两处写错一个字就静默多出一条空侧栏。
 */
export const COMBINED_PLUGIN_SUITE_ID = "asset:plugin+suite" as const;

/** 合并栏包含的 kind，顺序即列表里两段的顺序。 */
export const COMBINED_PLUGIN_SUITE_KINDS: AssetKindKey[] = ["plugin", "suite"];

export type AssetSidebarId =
  | `asset:${AssetKindKey}`
  | typeof COMBINED_PLUGIN_SUITE_ID;

export type SidebarId = StaticSidebarId | AssetSidebarId;

/**
 * live = 已接真实内核命令，界面上的数字全部来自它；
 * stub = 本轮未接后端，界面必须写「未接」并说明缺什么，不许填占位数据。
 */
export type DataPathState = "live" | "stub";

export interface SidebarDef {
  id: SidebarId;
  label: string;
  /** 侧栏导航里的一行说明 */
  hint: string;
  /** 右侧属性栏直接显示的「数据通路」原文 */
  dataPath: string;
  state: DataPathState;
  /** 对应的 Tauri 命令名；空数组 = 本轮没有任何可用命令 */
  commands: string[];
}

export interface SectionDef {
  id: SectionId;
  label: string;
  icon: string;
  hint: string;
  sidebars: SidebarDef[];
}

function assetSidebar(k: (typeof ASSET_KINDS)[number]): SidebarDef {
  return {
    id: `asset:${k.key}`,
    label: k.label,
    hint: `${k.label} 资产清单 · 状态 · 来源`,
    dataPath: `list_assets({ kind: "${k.key}" }) → get_asset(id)；「来源智能体」来自 capability_usage.asset_id 关联`,
    state: "live",
    commands: ["list_assets", "get_asset", "capability_usage"],
  };
}

/** Plugin + Suite 合并栏。两个 kind 各查一次，不做事后假过滤。 */
function combinedPluginSuiteSidebar(): SidebarDef {
  return {
    id: COMBINED_PLUGIN_SUITE_ID,
    label: "Plugin / Suite",
    hint: "Plugin 与 Suite 合并一栏 · 状态 · 来源",
    dataPath:
      'list_assets({ kind: "plugin" }) + list_assets({ kind: "suite" }) —— ' +
      "两个请求各自返回 total，界面照实拆成「Plugin n · Suite m」；不做前端假过滤",
    state: "live",
    commands: ["list_assets", "get_asset", "capability_usage"],
  };
}

/**
 * 资产分区的侧栏：六类资产，但 plugin 与 suite 出现在同一栏里。
 *
 * 用 ASSET_KINDS 顺序生成，合并组只在第一次遇到时插入一次 ——
 * 以后 ASSET_KINDS 增删，这里不会漂移。
 */
function assetSidebars(): SidebarDef[] {
  const out: SidebarDef[] = [];
  let combined = false;
  for (const k of ASSET_KINDS) {
    if ((COMBINED_PLUGIN_SUITE_KINDS as string[]).includes(k.key)) {
      if (!combined) {
        out.push(combinedPluginSuiteSidebar());
        combined = true;
      }
      continue;
    }
    out.push(assetSidebar(k));
  }
  return out;
}

export const SECTIONS: SectionDef[] = [
  {
    id: "overview",
    label: "总览",
    icon: "◈",
    hint: "在线智能体 · 能力拉取 · 能力检索 · 协议支持矩阵",
    sidebars: [
      {
        id: "fleet",
        label: "在线智能体",
        hint: "智能体 → 能力的分派关系",
        dataPath:
          "list_agents（C 静态登记 / B 心跳 / A 端点）+ capability_usage；图形化的每条连线都来自真实数据行",
        state: "live",
        commands: ["list_agents", "capability_usage", "register_static_agents", "scan_heartbeats"],
      },
      {
        id: "pull",
        label: "能力拉取",
        hint: "谁拉了什么能力 · 跨宿主分布",
        dataPath: "capability_usage（A 通道实报）+ registry_stats / rescan 摘要",
        state: "live",
        commands: ["capability_usage", "registry_stats"],
      },
      {
        id: "search",
        label: "能力检索",
        hint: "多路检索：关键词 / 语义 / 向量",
        dataPath:
          "关键词、Python BM25、Rust Tantivy、本地和云端向量均有执行入口；向量和重排按模型配置启用",
        state: "live",
        commands: ["list_assets", "search_capabilities"],
      },
      {
        id: "hosts",
        label: "协议支持矩阵",
        hint: "各智能体的协议与支持状态",
        dataPath:
          "list_agent_hosts（本机智能体协议注册表：claude-code / codex / qoder / minimax-code / workbuddy / kimi-code / deepseek-harness …）" +
          " + protocol_matrix（原「模型配置」里那张协议兼容性检查卡片，随卡片一并搬来）",
        state: "live",
        commands: ["list_agent_hosts", "protocol_matrix"],
      },
    ],
  },
  {
    id: "assets",
    label: "资产",
    icon: "▦",
    hint: "六层资产：Skill / MCP / Plugin·Suite / Ghost / Org（Plugin 与 Suite 合并一栏）",
    sidebars: assetSidebars(),
  },
  {
    id: "models",
    label: "模型配置",
    icon: "⚙",
    hint: "模型分工 · 模型配置",
    sidebars: [
      {
        id: "routing",
        label: "模型分工",
        hint: "任务节点 → 执行者（脚本 / 混合 / 模型）",
        dataPath:
          "list_capability_tasks —— 内核 ac_registry::tasks 的静态目录：25 个任务节点（执行者 / 状态 / 依赖 / 交叉 / 判定证据）+ 六项需求映射。" +
          "十六项工作流通过专项槽位、通用槽位和备用槽位执行，运行历史记录完成、待审核、缺配置和失败",
        state: "live",
        commands: ["list_capability_tasks", "workflow_catalog", "run_workflow", "workflow_history"],
      },
      {
        id: "providers",
        label: "模型配置",
        hint: "provider 接入位 · LLM 配置",
        dataPath:
          "十个专项模型槽位，保存配置、连接测试、输入输出契约与未保存状态",
        state: "live",
        commands: ["model_slots", "save_model_slot", "test_model_slot"],
      },
    ],
  },
  {
    id: "create",
    label: "新增能力",
    icon: "＋",
    hint: "新建资产 / 接入新能力",
    sidebars: [
      {
        id: "new-capability",
        label: "新增能力",
        hint: "预览导入、版本发布、生产与回滚",
        dataPath:
          "prepare_capability → 摘要与边界预览 → commit_capability；生产、验收和版本回滚通过工作流执行",
        state: "live",
        commands: ["prepare_capability", "commit_capability", "run_workflow"],
      },
    ],
  },
  {
    id: "repair",
    label: "维修站",
    icon: "⛑",
    hint: "故障清单 · 解决方案 · 修复 · 去重",
    sidebars: [
      {
        id: "faults",
        label: "故障清单",
        hint: "按分类与严重度列出故障",
        dataPath: "diagnostics（只读诊断，本地算法优先）",
        state: "live",
        commands: ["diagnostics"],
      },
      {
        id: "solutions",
        label: "故障解决方案",
        hint: "修复计划（去重裁决已移到「去重」侧栏）",
        dataPath: "diagnostics.plan（动作 / 作用域 / 逐条改动）；去重裁决在「去重」侧栏",
        state: "live",
        commands: ["diagnostics"],
      },
      {
        id: "fixes",
        label: "故障修复",
        hint: "中枢自修与受控能力修订",
        dataPath:
          "repair_hub 执行索引重建和重新扫描；修复工作流生成、验收修订包，待审核发布",
        state: "live",
        commands: ["diagnostics", "repair_hub", "run_workflow"],
      },
      {
        id: "dedup",
        label: "去重",
        hint: "待处理重复资产 · 合并/共存/忽略",
        dataPath:
          "list_dedup_candidates（每组带 match_level 与位置 id，类型由位置 id 的资产前缀判定）+ decide_dedup + dedup_history",
        state: "live",
        commands: ["list_dedup_candidates", "decide_dedup", "dedup_history"],
      },
    ],
  },
];

const SECTION_BY_ID = new Map<SectionId, SectionDef>(
  SECTIONS.map((s) => [s.id, s]),
);

const SIDEBAR_INDEX = new Map<
  SidebarId,
  { section: SectionDef; sidebar: SidebarDef }
>();
for (const section of SECTIONS) {
  for (const sidebar of section.sidebars) {
    SIDEBAR_INDEX.set(sidebar.id, { section, sidebar });
  }
}

export const DEFAULT_SECTION: SectionId = "overview";

export function sectionById(id: SectionId): SectionDef {
  return SECTION_BY_ID.get(id) ?? SECTIONS[0];
}

/** 该分区的默认侧栏（进入分区时落在第一条）。 */
export function defaultSidebarOf(section: SectionId): SidebarId {
  return sectionById(section).sidebars[0].id;
}

export function sectionOfSidebar(id: SidebarId): SectionDef {
  return SIDEBAR_INDEX.get(id)?.section ?? SECTIONS[0];
}

export function sidebarDef(id: SidebarId): SidebarDef {
  return SIDEBAR_INDEX.get(id)?.sidebar ?? SECTIONS[0].sidebars[0];
}

export function isAssetSidebar(id: SidebarId): id is AssetSidebarId {
  return id.startsWith("asset:");
}

/**
 * 该资产侧栏要列哪些 kind。
 *
 * 合并栏返回 ["plugin", "suite"]，单类返回一个元素，非资产侧栏返回 []
 * （返回数组而不是 null，调用方不用再写 `?? []`）。
 */
export function assetKindsOf(id: SidebarId): AssetKindKey[] {
  if (!isAssetSidebar(id)) return [];
  if (id === COMBINED_PLUGIN_SUITE_ID) return [...COMBINED_PLUGIN_SUITE_KINDS];
  const kind = id.slice("asset:".length);
  return ASSET_KINDS.some((k) => k.key === kind) ? [kind as AssetKindKey] : [];
}

/** `asset:skill` → `skill`；合并栏与非资产侧栏返回 null（需要多 kind 用 {@link assetKindsOf}）。 */
export function assetKindOf(id: SidebarId): AssetKindKey | null {
  const kinds = assetKindsOf(id);
  return kinds.length === 1 ? kinds[0] : null;
}
