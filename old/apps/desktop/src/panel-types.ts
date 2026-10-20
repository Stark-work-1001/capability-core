/**
 * M3 三个新侧栏的数据类型。与 Rust 侧 ops / commands 一一对应。
 *
 * **所有字段都是可选或有值的真实数据**：查不到时内核返回空数组或 0，
 * 由界面显示「暂无数据 + 原因」，绝不用占位数字冒充。
 */

/** 故障分类。字符串枚举，与 ac_registry::diagnostics::DiagnosticCategory 对应 */
export type DiagnosticCategory =
  | "structured_store"
  | "full_text_index"
  | "vector_layer"
  | "llm_layer"
  | "asset_parse"
  | "missing_dependency"
  | "db_version_ahead";

/** 能否自修三档 */
export type Repairability = "auto" | "confirm" | "manual";

/** 修复动作。与内核 RepairAction 对应 */
export type RepairAction =
  | "none"
  | "rebuild_search_index"
  | "clear_preview_cache"
  | "rescan_assets"
  | "repair_source_file"
  | "upgrade_program";

export interface RepairPlan {
  action: RepairAction;
  /** hub_owned = 中枢自己的数据；user_files = 用户的源文件 */
  scope: "none" | "hub_owned" | "user_files";
  destructive: boolean;
  summary: string;
  /** 逐条列出将要改什么。需确认档位不允许为空 */
  changes: string[];
}

export interface Diagnostic {
  id: string;
  category: DiagnosticCategory;
  severity: "critical" | "error" | "warning" | "info";
  title: string;
  /** 具体哪个资产 / 哪个阶段 / 哪个文件 */
  subject: string;
  /** 错误原文 */
  detail: string;
  plan: RepairPlan;
  /** 本地算法判不了时为 true = 建议求援主对话模型 */
  needs_llm: boolean;
  needs_llm_reason: string;
}

export interface DiagnosticReport {
  diagnostics: Diagnostic[];
  critical: number;
  error: number;
  warning: number;
  info: number;
  /** 可自动修（只动中枢自己的数据） */
  auto_fixable: number;
  /** 需用户确认 */
  needs_confirm: number;
  /** 不可自修 */
  manual_only: number;
  needs_llm: number;
}

// ── 总览 ──────────────────────────────────────────────────────

export type Transport = "static" | "heartbeat" | "mcp";

export interface HeartbeatState {
  host: string;
  version: string;
  pid: number | null;
  last_beat: number;
  age_ms: number;
  source_file: string;
  state: "online" | "stale" | "offline";
  capabilities: string[];
}

export interface AgentRecord {
  agent_id: string;
  display_name: string;
  ecosystem: string;
  kind: string;
  transport: Transport;
  endpoint: string;
  first_seen: number;
  last_seen: number;
  meta: string;
  heartbeat: HeartbeatState | null;
  capability_count: number;
  /** 实报调用次数。static 通道恒为 0 —— 推断不是实测 */
  call_total: number;
  last_error: string;
}

export interface UsageRow {
  agent_id: string;
  agent_name: string;
  ecosystem: string;
  capability_id: string;
  asset_id: string;
  asset_name: string;
  source: Transport;
  call_count: number;
  first_used_at: number;
  last_used_at: number;
  last_error: string;
}

export interface EcosystemCount {
  ecosystem: string;
  agents: number;
  assets: number;
  capabilities: number;
  /** 实报过调用的条目数 */
  reported: number;
}

export interface StaticRegistrationReport {
  ecosystems: number;
  capability_rows: number;
  assets: number;
  unassigned_assets: number;
  per_ecosystem: EcosystemCount[];
}

export interface HeartbeatScanReport {
  dir: string;
  /** false = 还没有任何宿主接入（不是错误） */
  dir_exists: boolean;
  files_seen: number;
  hosts_registered: number;
  rejected: { file: string; reason: string }[];
}

// ── 模型接入 ──────────────────────────────────────────────────

export interface ProtocolRow {
  protocol: string;
  supported: boolean;
  assets: number;
  capabilities: number;
  passed_assets: number;
  failed_assets: number;
  passed_capabilities: number;
  failed_capabilities: number;
  /** 能力级成功率 0–1。没有样本时是 0，不是 1 */
  success_rate: number;
  top_reasons: string[];
}

export interface ProtocolMatrix {
  rows: ProtocolRow[];
  total_assets: number;
  total_capabilities: number;
  total_passed: number;
  total_failed: number;
}

export type ProviderStatus =
  | { status: "not_configured" }
  | { status: "disabled" }
  | { status: "not_implemented"; reason: string }
  | { status: "ready"; detail: string };

export interface ProviderView {
  id: string;
  name: string;
  kind: string;
  endpoint: string;
  model: string;
  enabled: boolean;
  status: ProviderStatus;
  /** 协议类型：openai（通用/本地）或 anthropic */
  protocol: string;
  /**
   * **掩码后的 key**，例如 `****1a2b`。
   *
   * 明文永远到不了前端：内核的 `ProviderView` 只回掩码。
   * 界面上那个「显示」开关展开的是这个掩码的说明，
   * 真正填 key 只能在表单里重新输入。
   */
  masked_key: string;
  /** 是否已配置 key（本地模型通常没有，不算异常） */
  has_key: boolean;
}

export interface LlmProviderReport {
  config_path: string;
  config_exists: boolean;
  providers: ProviderView[];
  /** 已实现真实调用的 provider 数。本轮恒为 0 */
  implemented: number;
  note: string;
}

// ── 协议支持矩阵（总览分区）─────────────────────────────────

/**
 * 一个协议条目在某宿主上的支持状态。
 *
 * `supported` = 已验证可用；`partial` = 部分可用 / 需要额外配置；
 * `planned` = 计划支持但本轮没有实现。**三者都不是布尔值**，
 * 因为「不支持」和「还没做」对用户的含义完全不同。
 */
export type HostProtocolSupport = "supported" | "partial" | "planned";

/** 传输方式。与内核 ac_registry::hosts::HostTransport 对应。 */
export type HostTransport =
  | "mcp-stdio"
  | "mcp-http"
  | "cli"
  | "json-config"
  | "skill-md";

export interface HostProtocol {
  /** 协议名，如 mcp / skill-md / plugin-api */
  name: string;
  transport: HostTransport | string;
  support: HostProtocolSupport | string;
  /** 为什么是这个支持状态；空串表示没有额外说明 */
  note?: string;
}

/**
 * 一个智能体宿主及其协议支持清单。
 *
 * 数据来自新命令 `list_agent_hosts`（内核 hosts.rs + agent_hosts 表）。
 * `detected` = 本机找到证据（插件目录 / CLI / 已知路径）；
 * `connected` = 已接入中枢（心跳或端点注册）。判不出来一律 false + evidence 说明，
 * **界面不能把 false 渲染成「不支持」** —— 那两件事不一样。
 */
export interface AgentHost {
  id: string;
  display_name: string;
  vendor: string;
  /** plugin / cli / mcp / harness */
  kind: string;
  detected: boolean;
  connected: boolean;
  /** unix 秒；0 或缺失 = 从未见过 */
  last_seen?: number | null;
  protocols?: HostProtocol[] | null;
  /** 判定证据（为什么 detected 是 true 或 false），界面原样展示 */
  evidence?: string[] | null;
}

/**
 * 导航结构的**唯一来源**已经搬到 {@link ./nav.ts}（分区 → 侧栏 两级模型）。
 *
 * 这里原来有一份平级的 `VIEWS` 四视图列表（overview / repair / models / assets），
 * 与 nav.ts 并存会出现两份互相漂移的导航定义，所以删掉了：
 * 界面只认 nav.ts 的 SECTIONS。
 */

// ── 模型分工（任务路由表）─────────────────────────────────────

/**
 * 任务执行者三档。与内核 `ac_registry::tasks::TaskExecutor` 的 snake_case 一致。
 *
 * **不是布尔值「要不要模型」**：hybrid 是「脚本做能做的、模型只做脚本做不了的
 * 那一段」，把它归进任何一边都会让用户对成本与可靠性产生错误预期。
 */
export type TaskExecutor = "script" | "model" | "hybrid";

/** 任务当前做到哪一步。 */
export type TaskStatus = "implemented" | "partial" | "planned";

/** 一个 model 型节点需要模型提供什么。纯脚本节点没有这个字段。 */
export interface ModelContract {
  /** 需要的模型能力（长上下文 / 代码理解 / 结构化输出 …） */
  capabilities: string[];
  input_schema: string;
  output_schema: string;
  /** 为什么脚本 / 算法做不到。这一条是判定理由，不是宣传语。 */
  why_not_script: string;
}

/** 一个任务节点。与内核 `CapabilityTask` 一一对应。 */
export interface CapabilityTask {
  id: string;
  display_name: string;
  description: string;
  executor: TaskExecutor;
  /** script 型：调哪个函数；model 型：需要什么能力；hybrid：两半分别说 */
  implementation: string;
  status: TaskStatus;
  depends_on: string[];
  /** 交叉关系。内核保证**双向对称**，界面据此画连线。 */
  crosses_with: string[];
  inputs: string[];
  outputs: string[];
  /** 为什么判成这个执行者。**每条都来自真实证据**，界面原样展示。 */
  evidence: string[];
  /** 真实存在的内核函数符号（crate::模块::函数）；空/缺省 = 没有可执行入口 */
  script_entry?: string[];
  model_contract?: ModelContract | null;
}

/** 六项需求 → 任务节点的映射。statement 是用户原话。 */
export interface RequirementMapping {
  id: string;
  title: string;
  statement: string;
  task_ids: string[];
  note: string;
}

export interface TaskCounts {
  total: number;
  script: number;
  model: number;
  hybrid: number;
  implemented: number;
  partial: number;
  planned: number;
}

/** 整份任务路由目录（内核 `list_capability_tasks` 的返回形状）。 */
export interface TaskCatalog {
  tasks: CapabilityTask[];
  requirements: RequirementMapping[];
  counts: TaskCounts;
  /**
   * 脚本型占比 = script / total。**不是**「工作量占比」——
   * 本表没有工作量字段，所以这个数只说明节点数，界面文案不能把它说成人力占比。
   */
  script_share: number;
  /** 目录性质说明（含「模型节点为计划状态」这句话），界面直接展示。 */
  note: string;
}


// ── 模型接入三槽位（Embedding / Judge / Rerank）─────────────────

/** 槽位名。与内核 `ac_llm::slots::ModelSlot` 的序列化值一致（小写）。 */
export type ModelSlotName = "embedding" | "judge" | "rerank" | "diagnose" | "repair" | "dedup" | "generate" | "verify" | "fallback" | "localembedding";

/**
 * 槽位配置视图。**不含明文密钥** —— 内核只回 `masked_key`。
 *
 * `has_key` 与 `masked_key` 是分开的两个事实：
 * 「配了 key」和「key 长什么样」不是一回事，界面需要前者来决定是否显示
 * 「已配置」绿字，需要后者来让用户确认自己填的是哪一把。
 */
export interface ModelSlotView {
  slot: ModelSlotName;
  enabled: boolean;
  /** cloud-openai / local-openai / disabled */
  mode: string;
  /** API 根地址，含 /v1 */
  endpoint: string;
  model: string;
  /** 密钥环境变量名（可选） */
  api_key_env: string;
  /** 掩码后的 key，如 `****mqunn`。未配置为空串。 */
  masked_key: string;
  has_key: boolean;
  timeout_ms: number;
  /** embedding 用：qwen3-embedding / openai-generic */
  input_template: string;
  /** judge 用：json_schema / text */
  output_schema: string;
  /** 是否具备调用条件（enabled + 有 endpoint + 有 model）。**不代表 key 对** */
  callable: boolean;
}

/**
 * 保存槽位时提交的配置。
 *
 * `api_key` 用 `null | string` 三态：
 * - 字符串 → 用这个 key
 * - `""` → 清空
 * - `null` / 省略 → **不改动现有 key**
 *
 * 这是必须的三态：前端只拿到掩码，如果把掩码当新值提交上去，
 * 用户的 key 会被一坨 `****xxxx` 覆盖掉。
 */
export interface ModelSlotConfig {
  slot: ModelSlotName;
  enabled: boolean;
  mode: string;
  endpoint: string;
  model: string;
  api_key_env: string;
  api_key?: string | null;
  timeout_ms: number;
  input_template?: string;
  output_schema?: string;
}

/** 「测试连接」的结果。**它来自一次真实网络请求**，不是字段校验。 */
export interface ModelSlotProbeResult {
  ok: boolean;
  elapsed_ms: number;
  /** 成功摘要，如「4096 维 × 1 条」「3 条重排，最高分 0.3772」「回复：二」 */
  summary: string;
  /** 失败原因。**不含密钥**（内核在每条错误路径上做了 redact） */
  error: string;
}

/** 三槽位的展示元数据（文案与顺序，纯前端常量）。 */
export const MODEL_SLOTS: {
  slot: ModelSlotName;
  title: string;
  hint: string;
  presetModel: string;
}[] = [
  {
    slot: "embedding",
    title: "Embedding · 向量模型",
    hint: "把能力文本编码为向量，供检索做相似度排序。",
    presetModel: "Qwen/Qwen3-VL-Embedding-8B",
  },
  {
    slot: "judge",
    title: "Judge · 判定分类模型",
    hint: "输出类别与判断；结构化输出走 json_schema。",
    presetModel: "Qwen/Qwen3.8-27B",
  },
  {
    slot: "rerank",
    title: "Rerank · 重排序模型",
    hint: "对首轮候选做第二段相关性重排。未配置时检索跳过一次重排。",
    presetModel: "Qwen/Qwen3-VL-Reranker-8B",
  },
  { slot: "diagnose", title: "Diagnose · 故障诊断", hint: "依据故障位置和证据提出原因；空槽时回退通用判定模型。", presetModel: "" },
  { slot: "repair", title: "Repair · 修复生成", hint: "生成可审查的纳管修复版本，经过预览和验收后发布。", presetModel: "" },
  { slot: "dedup", title: "Dedup · 语义去重", hint: "比较功能重叠、差异与合并建议，保留原始资产。", presetModel: "" },
  { slot: "generate", title: "Generate · 能力生产", hint: "将需求转换为技能草稿，先验证再注册。", presetModel: "" },
  { slot: "verify", title: "Verify · 质量验收", hint: "独立检查能力草稿的适用范围、风险和验收条件。", presetModel: "" },
  { slot: "fallback", title: "Fallback · 降级备用", hint: "专项及通用模型不可用时尝试此槽位，未指定模型留空。", presetModel: "" },
  { slot: "localembedding", title: "Local Embedding · 本地向量", hint: "独立的本机 OpenAI 兼容嵌入端点；与云端向量缓存隔离。", presetModel: "" },
];

/** 硅基流动预设（用户点按钮时填充，**不含密钥**）。 */
export const SILICONFLOW_ENDPOINT = "https://api.siliconflow.cn/v1";
