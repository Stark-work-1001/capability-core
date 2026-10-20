//! 任务路由表：把「能力中枢要对用户做的事」拆成可执行任务节点，逐个判定**谁来执行**。
//!
//! # 这张表要回答的问题
//!
//! 用户的原话是：「这些能力不调用相关模型的话，能否通过包括但不限于相关的脚本、
//! 算法、程序进行实现，同时，要对能够通过脚本、算法、程序实现的，尽量去实现；
//! 不能实现的话，你将这些分工中加入接入模型来实现相应的功能」。
//!
//! 所以本模块的产物不是一张「模型分派配置」，而是一张**判定表**：
//! 每个节点都必须回答三件事 ——
//!
//! 1. **谁执行**（[TaskExecutor]：脚本 / 模型 / 混合）；
//! 2. **凭什么这么判**（[CapabilityTask] 的 evidence 字段）；
//! 3. **到底做了没有**（[TaskStatus]：已实现 / 部分 / 计划）。
//!
//! 第 3 条是这张表最容易被做假的地方。内核的模型层（ac-llm）**只有 trait
//! 没有实现**（见 provider.rs 里 ProviderStatus::Ready 的注释：本轮不可达），
//! 因此任何标成 Implemented 的 model 型节点都是谎报。这一点由
//! no_model_task_claims_to_be_implemented 这条测试钉住。
//!
//! # script 型为什么必须给出真实函数
//!
//! CapabilityTask 的 script_entry 里每一项都是 crate::模块::函数 形式的
//! **真实符号**，由 every_script_entry_resolves_to_a_real_function 在测试期
//! 扫源码核对。写一个不存在的函数名会当场红 —— 这张表不能靠形容词证明自己。
//!
//! # 交叉（crosses_with）
//!
//! 用户明确要「不止有一条线，还应该会有一些交叉的项」。交叉在本模块里是
//! **双向且对称**的：A 的 crosses_with 里有 B，B 的 crosses_with 里就必须有 A，
//! 由 crossings_are_symmetric 守门。不对称的交叉图在界面上会画出单向箭头，
//! 而单向箭头会被读成「A 依赖 B」——那是 depends_on 的语义，两者不能混。
//!
//! # 与「模型分派」的区别
//!
//! 本表**不**记录「哪个任务走哪个 provider」。那是第二步：等 ac-llm 有了
//! 真正的 LlmProvider 实现、并且有 provider 配置之后，才谈得上把 model 型
//! 节点绑到某个 provider 上。现在能诚实地给出的，是「哪些节点**需要**模型」
//! 以及「需要什么能力」——即 [ModelContract]。

use crate::diagnostics::RepairAction;
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────
// 三态枚举
// ─────────────────────────────────────────────────────────────────────

/// 谁来执行这个任务节点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskExecutor {
    /// 纯本地程序：读文件、查库、比较时间、算哈希、走规则表。
    Script,
    /// 必须模型：需要理解自然语言 / 语义等价 / 生成正文。
    Model,
    /// 脚本做能做的部分，模型只做脚本做不了的那一段判断。
    Hybrid,
}

impl TaskExecutor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Script => "script",
            Self::Model => "model",
            Self::Hybrid => "hybrid",
        }
    }

    /// 界面上的分组标题。颜色由前端按 executor 决定，不在这里写死。
    pub fn label(self) -> &'static str {
        match self {
            Self::Script => "脚本 / 算法",
            Self::Model => "模型",
            Self::Hybrid => "混合（脚本 + 模型）",
        }
    }
}

/// 这个节点到底做到哪一步了。**三档而不是布尔值**：
/// 「没做」与「做了一半」对用户的含义完全不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// 内核里已经有能直接跑的实现（script 型由 script_entry 给出）。
    Implemented,
    /// 关键函数都在，但缺一个把它们串起来的入口（命令 / 编排）。
    Partial,
    /// 只登记了计划，内核里没有任何对应实现。
    Planned,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implemented => "implemented",
            Self::Partial => "partial",
            Self::Planned => "planned",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Implemented => "已实现",
            Self::Partial => "部分实现",
            Self::Planned => "计划中",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// 模型契约
// ─────────────────────────────────────────────────────────────────────

/// 一个 model 型节点需要模型提供什么。
///
/// # 为什么必须写这么细
///
/// 「接个模型来做」是最容易糊过去的一句话。真正要落地时必须先回答：
/// 要什么能力、输入长什么样、输出怎么被下游消费、以及**为什么脚本做不了**。
/// 最后一条尤其重要：写不出「为什么脚本做不了」的节点，通常说明它其实是脚本
/// 能做的，只是没人愿意写那段规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelContract {
    /// 需要的模型能力（长上下文 / 代码理解 / 结构化输出 / 跨语言 …）。
    pub capabilities: Vec<String>,
    /// 输入形状（JSON 或一句话说清字段）。
    pub input_schema: String,
    /// 输出形状。**必须是结构化输出**，否则下游无法消费。
    pub output_schema: String,
    /// 为什么脚本 / 算法做不到。这一段是判定理由，不是宣传语。
    pub why_not_script: String,
}

// ─────────────────────────────────────────────────────────────────────
// 任务节点
// ─────────────────────────────────────────────────────────────────────

/// 一个任务节点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityTask {
    /// 稳定 id，点分命名，例如 capability.index。跨版本不变。
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub executor: TaskExecutor,
    /// script 型：说清调哪个函数、做什么；model 型：说清需要什么能力；
    /// hybrid：分别说清两半。
    pub implementation: String,
    pub status: TaskStatus,
    /// 依赖哪些任务的输出（有向，允许无环拓扑排序）。
    pub depends_on: Vec<String>,
    /// 与哪些任务有交叉（**双向对称**，见模块文档）。
    pub crosses_with: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// 为什么判成 script / model / hybrid。**每条都要能被核验**。
    pub evidence: Vec<String>,
    /// script / hybrid 型引用的真实函数符号（crate::模块::函数）。
    /// 空表 = 这个节点没有可执行内核入口。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_entry: Vec<String>,
    /// model / hybrid 型必须给出契约；纯 script 型为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_contract: Option<ModelContract>,
}

// ─────────────────────────────────────────────────────────────────────
// 构造：用 Draft 让 25 个节点的字面量保持可读
// ─────────────────────────────────────────────────────────────────────

struct Draft {
    id: &'static str,
    display_name: &'static str,
    description: &'static str,
    executor: TaskExecutor,
    implementation: &'static str,
    status: TaskStatus,
    script_entry: Vec<&'static str>,
    model_contract: Option<ModelContract>,
    depends_on: Vec<&'static str>,
    crosses_with: Vec<&'static str>,
    inputs: Vec<&'static str>,
    outputs: Vec<&'static str>,
    evidence: Vec<&'static str>,
}

impl Draft {
    fn deps(mut self, v: &[&'static str]) -> Self {
        self.depends_on = v.to_vec();
        self
    }
    fn crosses(mut self, v: &[&'static str]) -> Self {
        self.crosses_with = v.to_vec();
        self
    }
    fn io(mut self, inputs: &[&'static str], outputs: &[&'static str]) -> Self {
        self.inputs = inputs.to_vec();
        self.outputs = outputs.to_vec();
        self
    }
    fn why(mut self, v: &[&'static str]) -> Self {
        self.evidence = v.to_vec();
        self
    }
    fn entry(mut self, v: &[&'static str]) -> Self {
        self.script_entry = v.to_vec();
        self
    }

    fn build(self) -> CapabilityTask {
        CapabilityTask {
            id: self.id.to_string(),
            display_name: self.display_name.to_string(),
            description: self.description.to_string(),
            executor: self.executor,
            implementation: self.implementation.to_string(),
            status: self.status,
            depends_on: self.depends_on.into_iter().map(str::to_string).collect(),
            crosses_with: self.crosses_with.into_iter().map(str::to_string).collect(),
            inputs: self.inputs.into_iter().map(str::to_string).collect(),
            outputs: self.outputs.into_iter().map(str::to_string).collect(),
            evidence: self.evidence.into_iter().map(str::to_string).collect(),
            script_entry: self.script_entry.into_iter().map(str::to_string).collect(),
            model_contract: self.model_contract,
        }
    }
}

fn blank(
    id: &'static str,
    display_name: &'static str,
    description: &'static str,
    executor: TaskExecutor,
    status: TaskStatus,
    implementation: &'static str,
    model_contract: Option<ModelContract>,
) -> Draft {
    Draft {
        id,
        display_name,
        description,
        executor,
        implementation,
        status,
        script_entry: Vec::new(),
        model_contract,
        depends_on: Vec::new(),
        crosses_with: Vec::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        evidence: Vec::new(),
    }
}

fn script(
    id: &'static str,
    display_name: &'static str,
    description: &'static str,
    status: TaskStatus,
    implementation: &'static str,
) -> Draft {
    blank(id, display_name, description, TaskExecutor::Script, status, implementation, None)
}

fn hybrid(
    id: &'static str,
    display_name: &'static str,
    description: &'static str,
    status: TaskStatus,
    implementation: &'static str,
    contract: ModelContract,
) -> Draft {
    blank(id, display_name, description, TaskExecutor::Hybrid, status, implementation, Some(contract))
}

fn model(
    id: &'static str,
    display_name: &'static str,
    description: &'static str,
    status: TaskStatus,
    implementation: &'static str,
    contract: ModelContract,
) -> Draft {
    blank(id, display_name, description, TaskExecutor::Model, status, implementation, Some(contract))
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ─────────────────────────────────────────────────────────────────────
// 25 个任务节点
// ─────────────────────────────────────────────────────────────────────
//
// 分组对应需求 1 → 6。依赖只写直接前置；交叉必须对称（见模块文档）。

fn drafts() -> Vec<Draft> {
    vec![
        // ══ 需求 1：对话智能体接入 → 识别 / 索引 / 调用 ══
        script(
            "connection.agent-register",
            "对话智能体接入中枢",
            "宿主插件通过 A/B/C 三通道把自己登记进中枢：A 端点注册、B 心跳文件、C 静态归属推断。",
            TaskStatus::Implemented,
            "三通道全是本地程序：A 通道 Registry::register_agent 写 agents 表；B 通道 Registry::scan_heartbeat_dir 扫 <data_dir>/heartbeat/*.json；C 通道 Registry::register_static_agents 按包名与路径标记静态归属。宿主侧插件只负责写心跳文件，中枢侧零模型参与。",
        )
        .entry(&[
            "ac_registry::agents::register_agent",
            "ac_registry::agents::scan_heartbeat_dir",
            "ac_registry::agents::register_static_agents",
        ])
        .crosses(&[
            "connection.channel-health",
            "capability.invoke",
            "fault.host-detect",
        ])
        .io(
            &[
                "AgentRegistration{agent_id, display_name, ecosystem, kind, transport, endpoint}",
                "heartbeat/<host>.json{host, version, pid, last_beat, capabilities}",
            ],
            &[
                "agents 表一行（同一身份的多通道合并成一条，channels 列出全部通道）",
                "AgentRecord",
            ],
        )
        .why(&[
            "三通道都是文件读写与 SQL 写入，没有一处需要模型推理",
            "在线/离线由时间差与 STALE_WINDOW_MS 阈值比较得出，是算术不是判断",
            "宿主身份归一（heartbeat:<id> 与 <id> 视为同一智能体）是字符串处理",
        ]),

        script(
            "connection.channel-health",
            "通道健康判定",
            "把 agents 表的接入事实汇总成在线 / 陈旧 / 离线 / 未知四档，供界面与诊断共用。",
            TaskStatus::Implemented,
            "Registry::list_agents 汇总各通道最近一次心跳；agent_overview 给出四档计数；observe_state(age_ms) 按 900s 阈值判 stale。",
        )
        .entry(&[
            "ac_registry::agents::list_agents",
            "ac_registry::agents::agent_overview",
            "ac_registry::agents::observe_state",
        ])
        .deps(&["connection.agent-register"])
        .crosses(&["connection.agent-register", "fault.model-health"])
        .io(
            &["agents 表 + heartbeat 文件 mtime + 自报 last_beat"],
            &["AgentOverview{online, stale, offline, unknown, static_agents, heartbeat_agents, mcp_agents}"],
        )
        .why(&[
            "四档由时间差阈值直接判定，脚本可做且可复现",
            "unknown（从没上报过心跳）与 offline 分开统计：把「没上报」算成「挂了」会让在线数长期骗人",
        ]),

        script(
            "capability.index",
            "能力索引",
            "扫描资产目录，解析后写入库，并与磁盘做全量对账：磁盘上没了的，库里跟着消失。",
            TaskStatus::Implemented,
            "ingest::ingest(registry, root) 一条调用串起 scan_dir_with → upsert_all → 对账删除 → sync_logical → dedup_candidates，返回 IngestReport。",
        )
        .entry(&[
            "ac_registry::ingest::ingest",
            "ac_registry::scan::scan_dir_with",
            "ac_registry::store::upsert_all",
            "ac_registry::store::sync_logical",
        ])
        .crosses(&[
            "capability.identify",
            "capability.delete",
            "capability.retrieve",
            "capability.create",
            "capability.dedup-decide",
        ])
        .io(
            &["扫描根目录（vendor/）的目录树"],
            &[
                "IngestReport{parsed, inserted, updated, unchanged, removed, counts, skipped, dedup_candidates}",
                "assets / capabilities / asset_locations 三张表",
            ],
        )
        .why(&[
            "扫描-解析-入库-对账是一条确定性流水线，M1 起就有单测覆盖（含「扫描失败不得清库」）",
            "inserted/updated/unchanged 由内容指纹 content_hash 决定，不看 mtime，不需要任何语义判断",
        ]),

        script(
            "capability.identify",
            "能力识别与身份推断",
            "把三种异构清单（SKILL.md / plugin.json / .mcp.json）解析成统一的 CapabilityManifest，并推断权限、副作用、来源、作者。",
            TaskStatus::Implemented,
            "parse::parse_asset 按文件名选解析器；infer::infer 用规则表推断身份维度；origin::classify_manifest 判来源与注册方式；author::infer_author 判作者与出处。",
        )
        .entry(&[
            "ac_registry::parse::parse_asset",
            "ac_registry::infer::infer",
            "ac_registry::origin::classify_manifest",
            "ac_registry::author::infer_author",
        ])
        .crosses(&[
            "capability.index",
            "capability.retrieve",
            "capability.create",
            "capability.dedup-decide",
            "fault.compat-check",
            "fault.behavior-vs-description",
        ])
        .io(
            &["SKILL.md / plugin.json / .mcp.json 原文 + 相对路径"],
            &["CapabilityManifest{kind, identity, provides[], provenance, author, author_source}"],
        )
        .why(&[
            "三种格式的解析是「作者声明字段的确定性抽取」，同一份文件永远得到同一结果",
            "身份维度由 infer 的关键词 / 工具名 / 标签规则表给出，可单测、可复现",
            "作者出处分 manifest / frontmatter / package 三档，是三段路径判断，不是「猜作者是谁」",
        ]),

        script(
            "capability.retrieve",
            "能力检索",
            "按类型 / 标签 / 权限 / 自由文本找能力：结构化基线永远可用，全文 BM25 与向量召回是可选增强。",
            TaskStatus::Implemented,
            "RetrievalEngine::search 走「结构化基线 + 可选层增强」，全文层由 FullTextIndex::attach 挂载；对外由 CapabilityService::search 统一裁剪与鉴权。",
        )
        .entry(&[
            "ac_retrieval::pipeline::RetrievalEngine::search",
            "ac_retrieval::fulltext::FullTextIndex::attach",
            "ac_mcp::service::search",
        ])
        .deps(&["capability.index"])
        .crosses(&[
            "capability.index",
            "capability.identify",
            "capability.semantic-recall",
            "capability.invoke",
            "capability.intent-disambiguate",
        ])
        .io(
            &["AssetFilter{kind, tag, min_authority, text} + 自由文本 + limit"],
            &["SearchResult{hits: Vec<AssetRow>, degradation: DegradationReport}"],
        )
        .why(&[
            "结构化过滤下推 SQLite，BM25 走 tantivy 倒排，全程无模型调用",
            "三层降级（全文 / 向量 / LLM 缺席或故障）都仍返回基线结果，M2 有六组降级用例钉住",
            "结果为空与基线故障是两件事：degradation.baseline_failed() 单独报告",
        ]),

        script(
            "capability.semantic-recall",
            "语义召回",
            "把「画个时序图」和「mermaid sequence diagram」对上：向量召回层。",
            TaskStatus::Partial,
            "VectorIndex 已实现 HTTP 嵌入客户端、余弦打分与索引重建（rebuild / recall / probe）。它调用的是**嵌入服务**，不是对话模型；服务不可用时如实降级为结构化基线。",
        )
        .entry(&[
            "ac_retrieval::vector::VectorIndex::recall",
            "ac_retrieval::vector::VectorIndex::rebuild",
            "ac_retrieval::vector::VectorIndex::probe",
        ])
        .deps(&["capability.index"])
        .crosses(&[
            "capability.retrieve",
            "capability.intent-disambiguate",
            "fault.model-health",
        ])
        .io(
            &[
                "查询文本",
                "库内资产的 search_blob（name + description + 能力摘要 + 标签）",
            ],
            &["Scored{id, score} 向量命中列表（经结构化过滤后回库取行）"],
        )
        .why(&[
            "嵌入 + 余弦相似度是算法，不是模型推理：代码在 ac_retrieval::vector（含超时、chunked 解析、维度校验）",
            "Partial 的原因：内核侧代码齐了，但需要外部嵌入服务（默认 127.0.0.1:18191）真的在跑，且没有单独的 Tauri 命令暴露 rebuild",
            "服务不在时必须报 Failed 并给原因，不能伪装成「没有语义命中」——那是谎报",
        ]),

        script(
            "capability.invoke",
            "能力调用",
            "把能力内容交给调用方，并记账：谁、在什么时候、拉了哪一项能力。",
            TaskStatus::Implemented,
            "CapabilityService::content 按调用方身份裁剪后返回能力全文；Router::acquire 给可变能力加共享 / 互斥租约；调用结束后 report_capability_usage 记账。MCP 侧由 McpServer::tools 暴露同一套逻辑。",
        )
        .entry(&[
            "ac_mcp::service::content",
            "ac_mcp::router::acquire",
            "ac_registry::agents::report_capability_usage",
            "ac_mcp::mcp::tools",
        ])
        .deps(&["capability.retrieve"])
        .crosses(&[
            "capability.retrieve",
            "connection.agent-register",
            "repair.track",
        ])
        .io(
            &[
                "caller agent_id",
                "capability_id / asset_id",
                "租约模式（shared / exclusive）",
            ],
            &[
                "CapabilityContent{manifest, content, ...}（交给宿主执行）",
                "agent_capability_usage 记账行 + 租约 Allocation",
            ],
        )
        .why(&[
            "中枢的边界是「把能力内容交出去」而不是「代它执行」：这一步是读文件 + 鉴权 + 记账，全是脚本",
            "MCP tools/call 与 HTTP /api/v1/capabilities/:id/content 共用 CapabilityService::content，一条逻辑两个入口，不会漂移",
            "租约互斥由单条带条件的 UPDATE 与内存队列保证，不涉及语义判断",
        ]),

        model(
            "capability.intent-disambiguate",
            "意图消歧（选哪个能力）",
            "用户一句话，库里有十几个名字相近的能力：挑一个，并说清为什么是它。",
            TaskStatus::Planned,
            "需要模型的语义理解能力：输入用户原话 + 检索命中的候选（≤20 条），输出选中的 capability_id 与理由，或明确回答「都不是」。",
            ModelContract {
                capabilities: strings(&[
                    "中文语义理解（同义改写、跨语言归一）",
                    "在给定候选集内做选择（不能凭空造 id）",
                    "结构化输出（JSON）",
                ]),
                input_schema: "{\"query\": string, \"candidates\": [{\"id\", \"name\", \"summary\", \"tags\"}]}；候选 ≤ 20 条，全部来自 capability.retrieve 的真实命中，候选为空时不调用模型".to_string(),
                output_schema: "{\"picked\": [{\"id\": string, \"reason\": string}], \"none_of_them\": bool}；id 必须出现在输入候选里，否则整条结果作废（由调用方校验）".to_string(),
                why_not_script: "BM25 只能对齐字面。用户说「把会议录音变成待办」，库里的能力叫 transcribe-meeting 且摘要里只有「转写」，两者没有共同词项，规则表补不到这个差距；同义词表能覆盖一小部分，覆盖不到的部分只能猜——而猜错一个能力的代价比不召回更大（用户会照着错误的能力去干活）。".to_string(),
            },
        )
        .deps(&["capability.retrieve"])
        .crosses(&[
            "capability.retrieve",
            "capability.semantic-recall",
            "solution.explain",
            "routing.script-first",
        ])
        .io(
            &[
                "用户自然语言需求",
                "capability.retrieve 的候选集（含 name / summary / tags）",
            ],
            &["选中的 capability_id + 理由；或 none_of_them=true"],
        )
        .why(&[
            "只在「候选非空且字面分数接近」时触发，其余情况脚本直接给结果，模型不是默认路径",
            "必须结构化输出，否则下游无法把选择落到具体能力上",
            "Planned：ac-llm 只有 LlmProvider trait，没有任何实现（provider.rs 明写「本轮不可达」）",
        ]),

        // ══ 需求 2：新增能力 / 删除能力 ══
        hybrid(
            "capability.create",
            "新增能力",
            "把一份新能力纳管进来：脚本负责落盘与入库，模型负责把「我要一个能 X 的工具」写成清单。",
            TaskStatus::Partial,
            "脚本半：asset_store::init_layout 建目录骨架，install_skill / install_mcp 校验 id 与相对路径后落盘，verify_entity 复核入口文件，store::upsert 登记入库。模型半：生成 SKILL.md 正文 / plugin.json 清单 / MCP 启动命令。",
            ModelContract {
                capabilities: strings(&[
                    "按规范生成 Markdown / JSON 清单（SKILL.md frontmatter、plugin.json、.mcp.json）",
                    "把用户的功能描述拆成「能力名 + 摘要 + 标签」",
                    "结构化输出（必须能被 parse_asset 解析）",
                ]),
                input_schema: "{requirement: string（用户原话）, kind: skill|mcp, constraints?: {allowed_tools, author, version}}".to_string(),
                output_schema: "{files: [{rel_path: string, content: string}], entry: string（入口相对路径，必须在 files 里）}".to_string(),
                why_not_script: "脚本能校验、能落盘、能入库，但写不出一份合规的 SKILL.md：正文是从「用户想做什么」到「一份给人、也给模型看的能力说明」的生成，不是模板填空。模板填出来的东西解析能过、用起来是空壳。".to_string(),
            },
        )
        .entry(&[
            "ac_registry::asset_store::init_layout",
            "ac_registry::asset_store::install_skill",
            "ac_registry::asset_store::install_mcp",
            "ac_registry::asset_store::verify_entity",
            "ac_registry::store::upsert",
        ])
        .deps(&["capability.identify"])
        .crosses(&[
            "capability.index",
            "capability.identify",
            "capability.delete",
            "fault.compat-check",
        ])
        .io(
            &[
                "目标 kind（skill / mcp）",
                "文件清单（相对路径 → 字节）",
                "入口文件相对路径",
            ],
            &[
                "assets/<kind>/<id>/ 下的纳管实体",
                "AssetWriteReport{rel_path, files, asset_id}",
            ],
        )
        .why(&[
            "脚本能做的那半已经全部实现：建目录、id 消毒、相对路径不许越界、目标已存在时拒绝覆盖、入口必须真的在文件清单里",
            "脚本做不了的那半是「生成正文」：需要自然语言生成，不是规则",
            "Partial：脚本侧就绪，模型侧未接（没有 provider 实现，也就没有生成入口）",
        ]),

        script(
            "capability.delete",
            "删除能力",
            "用户说的「删除」在中枢里是软删除：库内行置 archived，列表默认看不见；只有 assets/ 内的纳管副本才允许物理删。",
            TaskStatus::Implemented,
            "store::set_asset_lifecycle(id, \"archived\") 改一行 SQL；asset_store::remove_entity(kind, id) 删 assets/ 内的纳管副本（带越界防护）；faults::remove_asset_entry 同步清掉纳管登记。",
        )
        .entry(&[
            "ac_registry::store::set_asset_lifecycle",
            "ac_registry::asset_store::remove_entity",
            "ac_registry::faults::remove_asset_entry",
        ])
        .deps(&["capability.identify"])
        .crosses(&[
            "capability.index",
            "capability.create",
            "capability.dedup-decide",
        ])
        .io(
            &["asset_id", "删除语义（archived 软删 / assets 纳管副本物理删）"],
            &[
                "lifecycle=archived 的库内行（默认不出现在列表，include_archived 才可见）",
                "remove_entity 返回是否真的删掉了那一份纳管副本",
            ],
        )
        .why(&[
            "项目铁律：绝不物理删除库内资产行；「删除」= UPDATE 一个状态列，纯 SQL",
            "唯一允许物理删的是 assets/ 内部纳管副本，remove_entity 锁死两条：规范化后必须在 root 之内、深度必须正好是 root/<kind>/<id>",
            "没有一处需要模型判断「这个能力该不该删」——那是用户的选择，不是模型的建议",
        ]),

        script(
            "capability.dedup-decide",
            "重复能力裁决",
            "同一份内容在磁盘上有多个副本：算出候选，交给用户裁决，绝不自动合并。",
            TaskStatus::Implemented,
            "store::dedup_candidates 按 content_hash（L1，字节级）与 normalized_hash（L2，去空白 / 版本号 / 大小写）分组给出候选；decide_dedup 落裁决；dedup_history 出审计；指纹算法在 fingerprint 模块。",
        )
        .entry(&[
            "ac_registry::store::dedup_candidates",
            "ac_registry::store::decide_dedup",
            "ac_registry::store::dedup_history",
            "ac_registry::fingerprint::l1_hash",
            "ac_registry::fingerprint::l2_hash",
        ])
        .deps(&["capability.index"])
        .crosses(&[
            "capability.index",
            "capability.identify",
            "capability.delete",
        ])
        .io(
            &["assets + asset_locations 表", "用户裁决（merge / keep_both / ignore）"],
            &[
                "DedupCandidate{group_key, match_level, suggested_canonical, location_ids}",
                "DedupRecord 审计行",
            ],
        )
        .why(&[
            "候选完全由哈希分组算出：L1 是字节级相同，L2 是规范化后相同，两者都是确定性算法",
            "「绝不自动合并」是纪律：程序只提供候选与历史，合并动作必须由用户显式点出来",
            "裁决落库是写一行记录，纯 SQL",
        ]),

        // ══ 需求 3：识别故障与不兼容 ══
        script(
            "fault.detect",
            "故障识别",
            "七类检查跑一遍：结构化存储、库版本、依赖、全文索引、向量层、LLM 层、资产解析。",
            TaskStatus::Implemented,
            "diagnostics::collect(registry, ctx) 逐类跑实测探针，产出 DiagnosticReport；向量层由 VectorProbe::probe_from_env 真探一次（带超时与冷却）。",
        )
        .entry(&[
            "ac_registry::diagnostics::collect",
            "ac_registry::diagnostics::VectorProbe::probe_from_env",
            "ac_retrieval::health::StageState::as_str",
        ])
        .deps(&["capability.index"])
        .crosses(&[
            "fault.locate",
            "fault.compat-check",
            "fault.model-health",
            "fault.behavior-vs-description",
            "solution.plan",
        ])
        .io(
            &["Registry", "DiagContext{db_path, scan_root, preview_json, llm, vector}"],
            &["DiagnosticReport{diagnostics[], critical, error, warning, info, auto_fixable, needs_confirm, manual_only, needs_llm}"],
        )
        .why(&[
            "七类检查全部是实测探针（读 user_version / 打开索引 / 探端点 / 试解析），不是模型判断",
            "每条诊断自带 needs_llm 与 needs_llm_reason：什么时候该求援模型，由脚本先判定并写清理由",
            "「缺席 / 禁用 / 故障 / 就绪」四态可区分是 M2 的硬要求：把「没这能力」和「有但挂了」混成一个 bool，处置就无从下手",
        ]),

        script(
            "fault.locate",
            "故障定位",
            "不只说「坏了」，要说清「哪个文件的哪个字段、凭什么这么判」。",
            TaskStatus::Implemented,
            "locator::Locator::locate 对 MCP 配置逐 server 检测，产出 FaultReport（7 类 FailureClass + 三档证据）；sweep::sweep 把检测与落库串成一次巡检，sweep::mcp_targets 提供目标。",
        )
        .entry(&[
            "ac_registry::locator::Locator::locate",
            "ac_registry::sweep::sweep",
            "ac_registry::sweep::mcp_targets",
            "ac_registry::faults::record_faults",
        ])
        .deps(&["fault.detect"])
        .crosses(&[
            "fault.detect",
            "solution.plan",
            "repair.user-files",
            "repair.verify",
            "fault.behavior-vs-description",
        ])
        .io(
            &["资产 source_path + 原文", "文件系统 / PATH / 环境变量探针"],
            &[
                "FaultReport{asset_id, failure_class, also_detected[], location{field,value,source_file}, evidence[Probed|Recorded|Inferred], prerequisites[], suggested_repair_kind}",
                "asset_faults 表",
            ],
        )
        .why(&[
            "7 类 FailureClass 每一类都对应一次实测：文件是否存在、命令是否在 PATH、变量是否已设、JSON 能否解析",
            "证据分 Probed / Recorded / Inferred 三档，推断出来的永远不会被当成实测（has_probed_evidence 是硬要求）",
            "定位到「哪个文件哪个字段」是路径与字符串计算，不需要模型",
        ]),

        script(
            "fault.compat-check",
            "不兼容识别",
            "这个资产声明的协议中枢认不认、它的能力条目解析得对不对、它能被哪些宿主用。",
            TaskStatus::Implemented,
            "protocol::check_all 逐资产逐能力检查协议与清单字段，matrix 汇总成矩阵；hosts::asset_host_support_detail 给出资产 ↔ 宿主的支持判定与依据。",
        )
        .entry(&[
            "ac_registry::protocol::check_all",
            "ac_registry::protocol::matrix",
            "ac_registry::hosts::asset_host_support_detail",
            "ac_registry::protocol::is_supported",
        ])
        .deps(&["capability.identify"])
        .crosses(&[
            "fault.detect",
            "capability.identify",
            "capability.create",
            "fault.host-detect",
        ])
        .io(
            &["库内资产行（source_format / source_path / manifest）"],
            &[
                "ProtocolMatrix{rows[protocol, supported, assets, capabilities, passed, failed, success_rate, top_reasons]}",
                "HostSupport{hosts[], basis[]}",
            ],
        )
        .why(&[
            "兼容性判据是协议白名单 + 清单字段校验，纯规则",
            "success_rate 的分母是能力条目数；分母为 0 时报 0 而不是 1——没有样本不等于全部通过",
        ]),

        script(
            "fault.host-detect",
            "宿主探测",
            "本机装了哪些智能体宿主：claude-code / codex / qoder / kimi-code / deepseek-harness …",
            TaskStatus::Implemented,
            "hosts::detect 逐条跑探测规则（路径存在 / 命令在 PATH / 环境变量非空），sync_hosts 落库，seed_defaults 首次写内置清单，load_specs 读回用户自定义清单。",
        )
        .entry(&[
            "ac_registry::hosts::detect",
            "ac_registry::hosts::sync_hosts",
            "ac_registry::hosts::seed_defaults",
            "ac_registry::hosts::load_specs",
        ])
        .crosses(&[
            "fault.compat-check",
            "connection.agent-register",
        ])
        .io(
            &["<data_dir>/hosts/*.json 宿主清单", "本机文件系统 / PATH / 环境变量"],
            &["AgentHost{id, detected, connected, protocols[], evidence[]}", "agent_hosts 表"],
        )
        .why(&[
            "探测规则只有三类：路径存在、命令在 PATH、环境变量非空，逐条都能产出证据",
            "detected=false 时也输出未命中证据，界面才能区分「真没装」和「这个版本没加规则」",
        ]),

        script(
            "fault.model-health",
            "模型层健康",
            "配了几个 provider、哪些被关了、有没有 key、有没有一个是真能调的。",
            TaskStatus::Implemented,
            "ac_llm::provider::views 把配置转成带掩码的视图，implemented_count 统计「真有调用实现」的 provider 数（本轮恒为 0）；诊断的 LLM 层分支据此报 not_implemented 而不是 ready。",
        )
        .entry(&[
            "ac_llm::provider::views",
            "ac_llm::provider::implemented_count",
            "ac_llm::provider::load_registry",
        ])
        .crosses(&[
            "fault.detect",
            "connection.channel-health",
            "capability.semantic-recall",
        ])
        .io(
            &["<data_dir>/llm-providers.json"],
            &[
                "ProviderView{id, status(not_configured|disabled|not_implemented|ready), masked_key, has_key}",
                "implemented 计数",
            ],
        )
        .why(&[
            "「配置能不能解析、是否 enabled、有没有 key」都是读 JSON 判断，脚本可做",
            "implemented_count 现在恒为 0：ac-llm 里没有任何 LlmProvider 实现，诊断必须如实报 not_implemented——把它报成 ready 就是骗人",
            "api_key 只回掩码（mask_key），明文永远不进前端",
        ]),

        model(
            "fault.behavior-vs-description",
            "行为与描述一致性判定",
            "清单说它能做 A，正文描述的流程却是 B：静态检查全过，但用户报告「这能力不像它说的那样」。",
            TaskStatus::Planned,
            "需要模型的长上下文与语义一致性判断：读资产全文，判断「它声明的能力」与「它正文里实际描述的流程」是否一致，并引用原文作为证据。",
            ModelContract {
                capabilities: strings(&[
                    "长上下文（单资产 5k–50k 字）",
                    "代码 / 流程理解",
                    "结构化输出 + 原文引用（必须能人工复核）",
                ]),
                input_schema: "{asset:{id, name, description, provides[]}, source_text: string（≤50k 字）, static_facts:[{kind, ok, detail}]}".to_string(),
                output_schema: "{verdict: consistent|partially|inconsistent|undecidable, evidence:[{quote, why}], confidence: 0..1}".to_string(),
                why_not_script: "脚本能判「字段齐不齐、协议认不认」，判不了「这段 SKILL.md 声称的能力与它正文描述的步骤是不是一回事」。这是语义一致性判断：同一句话在不同语境下可以是承诺，也可以是示例。规则表只能抓极少数关键词矛盾（例如声明只读却出现 rm -rf），抓不到的占绝大多数。".to_string(),
            },
        )
        .deps(&["capability.identify", "fault.detect"])
        .crosses(&[
            "fault.detect",
            "fault.locate",
            "capability.identify",
            "solution.explain",
            "routing.script-first",
        ])
        .io(
            &["资产 manifest + 全文", "静态检查结果（已通过的那些）"],
            &["判定：一致 / 部分一致 / 不一致 / 判不了，附原文引用与置信度"],
        )
        .why(&[
            "只在静态检查全绿、但用户报告「行为不对」时触发，不是默认路径——否则每份资产都要过一次模型，成本与误报都不可接受",
            "必须带原文引用：没有引用的「不一致」结论无法人工复核，等于不可反驳的指控",
            "Planned：模型层未接",
        ]),

        // ══ 需求 4：获取故障解决方案 ══
        script(
            "solution.plan",
            "修复计划生成",
            "每条故障对应一个动作：改什么、动谁的数据、是否破坏性、需不需要用户点头。",
            TaskStatus::Implemented,
            "diagnostics::collect 为每条诊断填 RepairPlan{action, scope, destructive, summary, changes[]}；RepairPlan::repairability 是纯函数，决定 auto / confirm / manual；faults_api::repair_tasks 把故障清单转成给智能体看的作业单。",
        )
        .entry(&[
            "ac_registry::diagnostics::RepairPlan::repairability",
            "ac_registry::faults::list_faults",
            "ac_mcp::faults_api::repair_tasks",
        ])
        .deps(&["fault.detect", "fault.locate"])
        .crosses(&[
            "fault.detect",
            "fault.locate",
            "solution.explain",
            "solution.known-issue-match",
            "repair.hub-owned",
            "repair.user-files",
        ])
        .io(
            &["Diagnostic[]", "FaultQuery{status, asset_id, failure_class, claimed_by}"],
            &[
                "RepairPlan{action, scope, destructive, summary, changes[]}",
                "RepairTaskList{open, in_progress, tasks[], central_agent_scope[]}",
            ],
        )
        .why(&[
            "repairability 是纯函数：动用户源文件一律 confirm；动中枢自己的数据时破坏性动作也要 confirm；其余 auto",
            "任务单里带 prerequisites 与 boundaries，接手的智能体不用再猜边界",
            "脚本给的是「按分类映射到动作」；「这个具体故障到底该怎么修」见 solution.explain",
        ]),

        hybrid(
            "solution.known-issue-match",
            "已知问题匹配",
            "这条故障以前出现过吗？上次是怎么修的？",
            TaskStatus::Planned,
            "脚本半：按 fault_id / failure_class / 资产指纹查 asset_faults 与 fault_repairs 的历史记录。模型半：历史无命中或只有部分相似时，从故障原文与证据推理一个方案。",
            ModelContract {
                capabilities: strings(&[
                    "故障原文理解（含错误码、路径、堆栈）",
                    "历史修复记录归纳",
                    "结构化输出（分步骤 + 风险）",
                ]),
                input_schema: "{fault:{failure_class, classes[], location, evidence, prerequisites}, history:[{action, actor, note, at}]}".to_string(),
                output_schema: "{match: reuse|adapt|novel, plan:[{step, why, risk}], confidence: 0..1}".to_string(),
                why_not_script: "「这条新故障和三个月前那条是不是同一类问题」不是字符串相等：错误信息里带着不同的路径、不同的版本号、不同的 server 名，但根因可能完全相同。规则能按 failure_class 粗筛，粗筛之后「像不像」的排序需要语义判断。".to_string(),
            },
        )
        .entry(&[
            "ac_registry::faults::list_faults",
            "ac_registry::faults::repair_history",
            "ac_registry::fingerprint::l1_hash",
        ])
        .deps(&["fault.locate", "repair.track"])
        .crosses(&[
            "solution.plan",
            "solution.explain",
            "repair.track",
        ])
        .io(
            &["StoredFault（含 evidence 与 prerequisites）", "该故障 / 同类故障的修复历史"],
            &["匹配结论（复用 / 改造 / 全新）+ 分步方案 + 风险"],
        )
        .why(&[
            "脚本能做的那半：查询函数全在（list_faults / repair_history），数据也在库里",
            "Planned：**没有任何匹配与排序算法**——历史记录只是躺在表里，还没有东西去比对它们",
            "不把它做成纯脚本：按 failure_class 分组是粗筛，粗筛完剩下的「像不像」才是关键那一步",
        ]),

        model(
            "solution.explain",
            "故障解释（说人话）",
            "把「full_text 状态 failed，0 篇文档」翻译成「搜索的全文索引坏了，重建一次就好，你的资产文件不受影响」。",
            TaskStatus::Planned,
            "需要模型的中文技术解释能力：输入一条诊断（含原始错误原文与 RepairPlan），输出面向非专家的说明、可执行步骤与「什么时候该停手」。",
            ModelContract {
                capabilities: strings(&[
                    "中文技术解释（面向非专家改写）",
                    "保留原始事实（不许把错误原文改写成猜测）",
                    "结构化输出（步骤 + 风险 + 停止条件）",
                ]),
                input_schema: "{diagnostic:{category, severity, title, subject, detail, plan:{action, scope, destructive, summary, changes[]}}, context?:{asset?, recent_changes?}}".to_string(),
                output_schema: "{summary: string, steps:[{action, expected, risk}], when_to_stop: string}".to_string(),
                why_not_script: "RepairPlan.summary 已经是人话，但它是**分类级模板**：同一句「重建全文索引」在「索引从未建过」「索引过期」「索引目录被占」三种情况下该做的事不一样，用户看到的后果也不一样。把模板拼成段落不等于解释；解释需要理解这条具体故障的因果。".to_string(),
            },
        )
        .deps(&["solution.plan"])
        .crosses(&[
            "solution.plan",
            "solution.known-issue-match",
            "fault.behavior-vs-description",
            "capability.intent-disambiguate",
            "repair.user-files",
            "routing.script-first",
        ])
        .io(
            &["一条 Diagnostic（含 detail 原文与 RepairPlan）", "可选上下文：相关资产、最近改动"],
            &["summary + 分步操作 + 每步的预期与风险 + 停止条件"],
        )
        .why(&[
            "界面本轮直接展示 plan.summary + changes（真实字段），不伪造解释——所以这一栏现在是空的，不是写了个占位",
            "Planned：模型层未接",
        ]),

        // ══ 需求 5：故障修复 ══
        script(
            "repair.hub-owned",
            "中枢自有数据修复",
            "只动中枢自己的派生物：重建全文索引、重新扫描入库、清掉过期预览缓存。",
            TaskStatus::Partial,
            "三个动作各自的执行函数都已存在：FullTextIndex::attach(SyncPolicy::Auto) 在索引缺失 / 过期 / 损坏时当场重建；ingest 重新扫描并全量对账；清 preview_json 是一次 fs::remove_file。缺的是**按 RepairPlan 串起来的执行器**。",
        )
        .entry(&[
            "ac_retrieval::fulltext::FullTextIndex::attach",
            "ac_registry::ingest::ingest",
            "ac_registry::faults::reconcile_asset_faults",
        ])
        .deps(&["solution.plan"])
        .crosses(&[
            "solution.plan",
            "repair.verify",
            "repair.user-files",
        ])
        .io(
            &["RepairPlan{action 属于 rebuild_search_index | rescan_assets | clear_preview_cache}"],
            &["重建后的 search-index/", "重新对账后的 assets 表", "被清掉的已消失故障 id"],
        )
        .why(&[
            "三个 HubOwned 动作都是「删掉派生物重建」或「重新跑一遍确定性流水线」，脚本完全可做",
            "Partial 的原因：能力在，入口不在——内核本轮没有任何 apply / repair 命令，RepairAction 只有枚举与计划，没有执行者",
            "边界由 RepairPlan::repairability 判定：destructive 的必须用户确认，脚本不自己决定",
        ]),

        hybrid(
            "repair.user-files",
            "用户源文件修复",
            "改 vendor/ 里的启动命令、路径、环境变量——这一步中枢绝不自己动手。",
            TaskStatus::Planned,
            "脚本半：locator 已经能定位到字段并算出候选新值（remap_path 做路径重映射、find_unexpanded_vars 找出未展开变量、parse_mcp_servers 解析配置结构）。模型半：判断「应该改成什么」，尤其是路径表覆盖不到的情况。",
            ModelContract {
                capabilities: strings(&[
                    "配置语义理解（这条命令原本想启动什么）",
                    "跨文件一致性推理（同一个路径在几处引用）",
                    "结构化输出（候选补丁 + 风险）",
                ]),
                input_schema: "{fault:{location, evidence, prerequisites}, config_excerpt: string, known_roots:[{old, new}]}".to_string(),
                output_schema: "{patches:[{file, field, old, new, why, risk}], needs_human: bool}".to_string(),
                why_not_script: "脚本能算出「这个路径不存在」「这个变量没展开」，也能按重映射表把旧根换新根。但「这条命令原本应该指向哪里」取决于用户意图与上游约定：重映射表覆盖不到时，猜错会把一个能跑的配置改成不能跑的。中枢的铁律是不写 vendor/，所以最终落盘还必须由外部智能体在自己的职权内执行。".to_string(),
            },
        )
        .entry(&[
            "ac_registry::locator::remap_path",
            "ac_registry::locator::find_unexpanded_vars",
            "ac_registry::locator::parse_mcp_servers",
        ])
        .deps(&["fault.locate", "solution.plan"])
        .crosses(&[
            "fault.locate",
            "solution.plan",
            "repair.hub-owned",
            "repair.verify",
            "solution.explain",
        ])
        .io(
            &["FaultReport.location{field, value, source_file}", "已知的根目录搬家表"],
            &["候选补丁（文件 / 字段 / 旧值 / 新值 / 风险）", "是否需要人判断"],
        )
        .why(&[
            "脚本能做的那半已有真实函数：路径重映射与未展开变量识别都是确定性计算",
            "脚本做不了的那半是「该改成什么」：重映射表是外部给的，表里没有的时候只能判断",
            "Planned：没有 patcher，且中枢铁律是不写 vendor/——落盘必须由外部智能体执行",
        ]),

        script(
            "repair.verify",
            "修复后复检",
            "修完再跑一遍同一条检测：故障真消失了吗？",
            TaskStatus::Implemented,
            "sweep::sweep 再跑一次巡检，用本次检出集合清理该资产的旧故障（reconcile_asset_faults）；确认消失后由 resolve_fault 结案，全程留审计。",
        )
        .entry(&[
            "ac_registry::sweep::sweep",
            "ac_registry::faults::reconcile_asset_faults",
            "ac_registry::faults::resolve_fault",
        ])
        .deps(&["repair.hub-owned"])
        .crosses(&[
            "repair.hub-owned",
            "repair.user-files",
            "fault.locate",
            "repair.track",
        ])
        .io(
            &["资产 id + 该资产本次的检出集合"],
            &["被清掉的故障 id 列表", "status=fixed 的故障行 + 一条修复历史"],
        )
        .why(&[
            "复检 = 再跑一次同一条检测流水线，没有任何新判据",
            "sweep 里「先落库再清理」的顺序有注释与测试钉住：反过来会让刚复检确认的故障短暂消失",
            "结案只允许认领人做，这是权限判断不是语义判断",
        ]),

        script(
            "repair.track",
            "修复过程跟踪",
            "谁认领了这条故障、正在修、修完了、还是判定不可修——状态机与审计。",
            TaskStatus::Implemented,
            "faults::claim_fault 用一条带条件的 UPDATE 做排他认领；release_fault 释放；log_repair 追加审计；repair_history 出历史。",
        )
        .entry(&[
            "ac_registry::faults::claim_fault",
            "ac_registry::faults::release_fault",
            "ac_registry::faults::log_repair",
            "ac_registry::faults::repair_history",
        ])
        .crosses(&[
            "repair.verify",
            "capability.invoke",
            "solution.known-issue-match",
        ])
        .io(
            &["fault_id", "agent_id", "目标状态（fixed / unrepairable）+ 备注"],
            &["ClaimOutcome{Claimed|TakenByOther|AlreadyResolved|...}", "fault_repairs 审计行"],
        )
        .why(&[
            "认领是「先读后写」的反面教材，所以条件写在 UPDATE 的 WHERE 里，由 SQLite 保证互斥",
            "修复历史 append-only：它是审计不是状态，改历史等于销毁证据",
        ]),

        // ══ 需求 6：脚本优先 ══
        script(
            "routing.script-first",
            "脚本优先判定",
            "每个节点先问「能不能写成程序」，能写的一律写成程序；只有写不出来的才接模型。",
            TaskStatus::Implemented,
            "本模块就是判定的产物：all_tasks 给出全部节点，catalog 给出按执行者与状态的计数，route_repair_action 把内核的 RepairAction 映射到「谁执行」。",
        )
        .entry(&[
            "ac_registry::tasks::catalog",
            "ac_registry::tasks::all_tasks",
            "ac_registry::tasks::route_repair_action",
        ])
        .crosses(&[
            "capability.intent-disambiguate",
            "fault.behavior-vs-description",
            "solution.explain",
        ])
        .io(
            &["用户六项需求", "内核已有能力清单（模块 / 函数）"],
            &["任务路由表：每个节点的执行者、状态、依赖、交叉、判定证据"],
        )
        .why(&[
            "判定规则：能写成「读文件 / 查库 / 比较时间 / 算哈希 / 走规则表」的就是 script",
            "只有「理解自然语言 / 判断语义等价 / 生成正文」才归 model——这三类在 25 个节点里只占 3 个",
            "model 型节点全部 Planned：ac-llm 只有 trait 没有实现，任何标 Implemented 的 model 节点都是谎报（由测试钉住）",
        ]),
    ]
}

// ─────────────────────────────────────────────────────────────────────
// 对外接口
// ─────────────────────────────────────────────────────────────────────

/// 全部任务节点，按上表顺序（需求 1 → 6）。
pub fn all_tasks() -> Vec<CapabilityTask> {
    drafts().into_iter().map(Draft::build).collect()
}

/// 按 id 取一个节点。
pub fn task(id: &str) -> Option<CapabilityTask> {
    all_tasks().into_iter().find(|t| t.id == id)
}

/// 六项需求 → 任务节点的映射。
///
/// statement 是用户原话的逐字摘录：映射表最怕的是「按自己的理解重新表述需求」，
/// 那样一旦理解偏了，后面所有节点都会跟着偏，而且没人看得出来。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementMapping {
    /// r1 … r6
    pub id: String,
    pub title: String,
    /// 用户原话（逐字）。
    pub statement: String,
    pub task_ids: Vec<String>,
    /// 这项需求最终落在哪里、还缺什么。
    pub note: String,
}

fn requirement(
    id: &str,
    title: &str,
    statement: &str,
    task_ids: &[&str],
    note: &str,
) -> RequirementMapping {
    RequirementMapping {
        id: id.to_string(),
        title: title.to_string(),
        statement: statement.to_string(),
        task_ids: task_ids.iter().map(|s| s.to_string()).collect(),
        note: note.to_string(),
    }
}

pub fn requirements() -> Vec<RequirementMapping> {
    vec![
        requirement(
            "r1",
            "接入 · 识别 · 索引 · 调用",
            "对话智能体能够通过相应插件与能力中枢连接，来识别、索引、调用相关能力",
            &[
                "connection.agent-register",
                "connection.channel-health",
                "capability.identify",
                "capability.index",
                "capability.retrieve",
                "capability.semantic-recall",
                "capability.invoke",
                "capability.intent-disambiguate",
            ],
            "8 个节点里 7 个是脚本、其中 6 个已实现（语义召回缺外部嵌入服务）。唯一要模型的是「选哪个能力」的意图消歧，且只在候选分数接近时才触发。",
        ),
        requirement(
            "r2",
            "新增能力 · 删除能力",
            "能够完成新增能力和能力的删除",
            &[
                "capability.create",
                "capability.delete",
                "capability.dedup-decide",
            ],
            "删除是纯脚本且已实现（软删除 + assets 纳管副本物理删）。新增是混合：落盘 / 入库脚本已实现，写清单正文需要模型。",
        ),
        requirement(
            "r3",
            "识别故障与不兼容",
            "能够识别到模型的故障和不兼容的地方",
            &[
                "fault.detect",
                "fault.locate",
                "fault.compat-check",
                "fault.host-detect",
                "fault.model-health",
                "fault.behavior-vs-description",
            ],
            "5 个脚本节点已实现（诊断 / 定位 / 协议矩阵 / 宿主探测 / 模型层健康）。只有「行为与描述一致性」必须模型，且是可选路径。",
        ),
        requirement(
            "r4",
            "获取故障解决方案",
            "能够获取故障的解决方案",
            &[
                "solution.plan",
                "solution.known-issue-match",
                "solution.explain",
            ],
            "结构化修复计划已实现；r4 已接通模型诊断、中文解释及维修记录。跨故障的历史语义匹配仍为计划项。",
        ),
        requirement(
            "r5",
            "进行一定的故障修复",
            "能够进行一定的故障修复",
            &[
                "repair.hub-owned",
                "repair.user-files",
                "repair.verify",
                "repair.track",
            ],
            "重扫与索引重建已有执行命令；r5 生成并验收入口修订包，审核后写入纳管版本。原 vendor 保留，多文件修复需受控包导入。复检与跟踪已有入口。",
        ),
        requirement(
            "r6",
            "脚本优先 · 模型兜底",
            "这些能力不调用相关模型的话，能否通过包括但不限于相关的脚本、算法、程序进行实现，同时，要对能够通过脚本、算法、程序实现的，尽量去实现；不能实现的话，你将这些分工中加入接入模型来实现相应的功能",
            &["routing.script-first"],
            "脚本负责确定性检查、目录与数据库操作；模型处理诊断解释、生成和语义比较，执行状态以工作流历史为准。尚未实现的细分节点保留计划标识。",
        ),
    ]
}

/// 按执行者与状态的计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCounts {
    pub total: u32,
    pub script: u32,
    pub model: u32,
    pub hybrid: u32,
    pub implemented: u32,
    pub partial: u32,
    pub planned: u32,
}

/// 给界面 / IPC 的一整份目录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCatalog {
    pub tasks: Vec<CapabilityTask>,
    pub requirements: Vec<RequirementMapping>,
    pub counts: TaskCounts,
    /// 脚本型占比 = script / total。**不是**「脚本能覆盖的工作量占比」——
    /// 那需要按工作量加权，而本表没有工作量字段，不编。
    pub script_share: f32,
    /// 一句话说明这份目录的性质（界面直接展示，避免被读成「模型已接好」）。
    pub note: String,
}

fn counts_of(tasks: &[CapabilityTask]) -> TaskCounts {
    let mut c = TaskCounts {
        total: tasks.len() as u32,
        ..Default::default()
    };
    for t in tasks {
        match t.executor {
            TaskExecutor::Script => c.script += 1,
            TaskExecutor::Model => c.model += 1,
            TaskExecutor::Hybrid => c.hybrid += 1,
        }
        match t.status {
            TaskStatus::Implemented => c.implemented += 1,
            TaskStatus::Partial => c.partial += 1,
            TaskStatus::Planned => c.planned += 1,
        }
    }
    c
}

/// 整份目录。
pub fn catalog() -> TaskCatalog {
    let mut tasks = all_tasks();
    for task in &mut tasks {
        match task.id.as_str() {
            "solution.explain" => {task.status=TaskStatus::Implemented;task.implementation="ac_mcp::workflows::execute(r4) 调用 Diagnose → Judge → Fallback，保存方案和维修历史；需要已配置模型，运行结果单独登记".into();task.evidence=vec!["方案以实际证据为输入；缺少模型时记录未完成；输出待审核，不标记故障已修复".into()];},
            "capability.create" => {task.status=TaskStatus::Partial;task.implementation="prepare_import / commit_import 完成所有类型的受控收录；r16 生成单文件技能并调用验收模型，审核后发布。多文件生成尚需人工导入".into();task.evidence=vec!["不可变版本、SHA-256 预览摘要、乐观并发、事务登记与回滚已接通".into()];},
            "repair.user-files" => {task.status=TaskStatus::Partial;task.implementation="r5 根据故障生成同类型入口修订草稿，验收后提交到纳管目录；原始 vendor 文件保留。多文件修复通过受控包导入".into();task.evidence=vec!["模型生成、验收、发布和维修记录已有入口；发布后的实际可用性仍需复检".into()];},
            "repair.hub-owned" => {task.status=TaskStatus::Partial;task.implementation="repair_hub 命令已连接重新扫描、重建全文索引与审计。预览缓存清理仍未开放执行".into();task.evidence=vec!["两个白名单动作可从故障修复面板执行；模型不能传入任意命令".into()];},
            _=>{}
        }
    }
    let counts = counts_of(&tasks);
    let script_share = if counts.total == 0 {
        0.0
    } else {
        counts.script as f32 / counts.total as f32
    };
    TaskCatalog {
        note: format!(
            "任务路由表（静态目录，不读库）：总计 {} 个节点 = 脚本 {} / 混合 {} / 模型 {}；\
             已实现 {} / 部分 {} / 计划 {}。上方工作流显示最新执行结果；此图标注更细的实现范围，云端是否实际完成以运行历史为准。",
            counts.total,
            counts.script,
            counts.hybrid,
            counts.model,
            counts.implemented,
            counts.partial,
            counts.planned
        ),
        requirements: requirements(),
        tasks,
        counts,
        script_share,
    }
}

// ─────────────────────────────────────────────────────────────────────
// 修复动作 → 谁执行
// ─────────────────────────────────────────────────────────────────────

/// 一个 RepairAction 的执行归属。
///
/// executor 是 Option：None 表示**中枢与模型都不该做**（升级程序、无动作），
/// 需要人来处理。硬塞进 Script 或 Model 会让界面显示一个不存在的执行者。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairRoute {
    /// RepairAction::as_str() 的稳定取值。
    pub action: String,
    /// 谁执行；None = 只能人工。
    pub executor: Option<TaskExecutor>,
    pub status: TaskStatus,
    /// 对应的任务节点 id。
    pub task_id: String,
    /// 已经存在、可直接调用的函数（没有就是空串）。
    pub existing_entry: String,
    pub note: String,
}

/// 把内核的修复动作映射到「谁执行、现在做到哪一步」。
///
/// 这个函数是路由表与真实修复机制之间的**唯一连接点**：枚举加了新动作而这里
/// 没跟上时，match 会编译失败；every_repair_action_is_routed 再钉住每一条都有归属。
pub fn route_repair_action(action: RepairAction) -> RepairRoute {
    fn route(
        action: RepairAction,
        executor: Option<TaskExecutor>,
        status: TaskStatus,
        task_id: &str,
        existing_entry: &str,
        note: &str,
    ) -> RepairRoute {
        RepairRoute {
            action: action.as_str().to_string(),
            executor,
            status,
            task_id: task_id.to_string(),
            existing_entry: existing_entry.to_string(),
            note: note.to_string(),
        }
    }
    match action {
        RepairAction::None => route(
            action,
            None,
            TaskStatus::Implemented,
            "solution.plan",
            "",
            "没有可执行的修复动作：只出计划，不动任何东西。这是唯一「什么都不做也是对的」分支。",
        ),
        RepairAction::RebuildSearchIndex => route(
            action,
            Some(TaskExecutor::Script),
            TaskStatus::Partial,
            "repair.hub-owned",
            "ac_retrieval::fulltext::FullTextIndex::attach",
            "索引缺失 / 过期 / 损坏时，attach(SyncPolicy::Auto) 会当场重建。缺的是「按计划触发重建」的命令入口。",
        ),
        RepairAction::ClearPreviewCache => route(
            action,
            Some(TaskExecutor::Script),
            TaskStatus::Partial,
            "repair.hub-owned",
            "",
            "删一个中枢自己生成的缓存文件即可，但内核没有专门的清理函数：诊断只把路径（preview_json）给出来，删由调用方做。",
        ),
        RepairAction::RescanAssets => route(
            action,
            Some(TaskExecutor::Script),
            TaskStatus::Implemented,
            "capability.index",
            "ac_registry::ingest::ingest",
            "重扫就是再跑一次 ingest：扫描、入库、全量对账，函数已实现且被 Tauri 的 rescan 命令直接调用。",
        ),
        RepairAction::RepairSourceFile => route(
            action,
            Some(TaskExecutor::Hybrid),
            TaskStatus::Planned,
            "repair.user-files",
            "ac_registry::locator::remap_path",
            "定位与候选值计算是脚本（remap_path / find_unexpanded_vars），「该改成什么」要模型或人判断；且中枢不写 vendor/，落盘由外部智能体执行。",
        ),
        RepairAction::UpgradeProgram => route(
            action,
            None,
            TaskStatus::Planned,
            "solution.explain",
            "",
            "升级程序不是中枢能做的事，也不是模型能做的事：需要人来下载 / 安装。界面必须显示「需人工」而不是给一个假按钮。",
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────
// 测试
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn ids() -> Vec<String> {
        all_tasks().into_iter().map(|t| t.id).collect()
    }

    #[test]
    fn ids_are_unique_and_dotted() {
        let mut seen = BTreeSet::new();
        for id in ids() {
            assert!(seen.insert(id.clone()), "任务 id 重复: {id}");
            assert!(
                id.contains('.') && !id.contains(' '),
                "任务 id 必须是点分命名（便于按需求分组与前端排序）: {id}"
            );
            assert_eq!(id, id.to_lowercase(), "任务 id 必须全小写: {id}");
        }
    }

    #[test]
    fn dependencies_and_crossings_resolve() {
        let all: BTreeSet<String> = ids().into_iter().collect();
        for t in all_tasks() {
            for d in &t.depends_on {
                assert!(all.contains(d), "{} 依赖了不存在的任务 {d}", t.id);
                assert_ne!(&t.id, d, "{} 依赖了自己", t.id);
            }
            for c in &t.crosses_with {
                assert!(all.contains(c), "{} 交叉了不存在的任务 {c}", t.id);
                assert_ne!(&t.id, c, "{} 与自身交叉没有意义", t.id);
            }
            assert!(
                !t.description.trim().is_empty()
                    && !t.implementation.trim().is_empty()
                    && !t.evidence.is_empty(),
                "{} 的描述 / 实现 / 证据不得为空：判定表靠这三样说话",
                t.id
            );
            assert!(
                !t.inputs.is_empty() && !t.outputs.is_empty(),
                "{} 必须写清输入输出，否则节点不可执行",
                t.id
            );
        }
    }

    #[test]
    fn crossings_are_symmetric() {
        let tasks: BTreeMap<String, CapabilityTask> =
            all_tasks().into_iter().map(|t| (t.id.clone(), t)).collect();
        for (id, t) in &tasks {
            for c in &t.crosses_with {
                let other = tasks
                    .get(c)
                    .unwrap_or_else(|| panic!("{id} 交叉了不存在的 {c}"));
                assert!(
                    other.crosses_with.contains(id),
                    "交叉必须对称：{id} 声明了与 {c} 交叉，但 {c} 的 crosses_with 里没有 {id}。\n\
                     不对称的交叉在界面上会画成单向箭头，而单向箭头会被读成依赖关系。"
                );
            }
        }
    }

    #[test]
    fn depends_on_has_no_cycles() {
        let tasks: BTreeMap<String, CapabilityTask> =
            all_tasks().into_iter().map(|t| (t.id.clone(), t)).collect();
        // 0=未访问 1=在栈上 2=已完成
        let mut state: BTreeMap<String, u8> = BTreeMap::new();
        fn visit(
            id: &str,
            tasks: &BTreeMap<String, CapabilityTask>,
            state: &mut BTreeMap<String, u8>,
            path: &mut Vec<String>,
        ) {
            match state.get(id).copied().unwrap_or(0) {
                2 => return,
                1 => {
                    path.push(id.to_string());
                    panic!("depends_on 成环: {}", path.join(" -> "));
                }
                _ => {}
            }
            state.insert(id.to_string(), 1);
            path.push(id.to_string());
            if let Some(t) = tasks.get(id) {
                for d in &t.depends_on {
                    visit(d, tasks, state, path);
                }
            }
            path.pop();
            state.insert(id.to_string(), 2);
        }
        for id in tasks.keys() {
            let mut path = Vec::new();
            visit(id, &tasks, &mut state, &mut path);
        }
    }

    #[test]
    fn every_task_is_mapped_to_exactly_one_requirement() {
        let tasks: BTreeSet<String> = ids().into_iter().collect();
        let mut covered: BTreeMap<String, usize> = BTreeMap::new();
        let reqs = requirements();
        assert_eq!(reqs.len(), 6, "用户列了六项需求，映射表必须是六条");
        for (i, r) in reqs.iter().enumerate() {
            assert_eq!(r.id, format!("r{}", i + 1), "需求 id 必须按 r1..r6 顺序");
            assert!(!r.statement.trim().is_empty(), "{} 必须保留用户原话", r.id);
            assert!(!r.task_ids.is_empty(), "{} 没有落点任务", r.id);
            for t in &r.task_ids {
                assert!(tasks.contains(t), "{} 指向不存在的任务 {t}", r.id);
                *covered.entry(t.clone()).or_insert(0) += 1;
            }
        }
        for t in &tasks {
            let n = covered.get(t).copied().unwrap_or(0);
            assert_eq!(
                n, 1,
                "任务 {t} 被 {n} 条需求覆盖；每个节点必须且只能挂在一项需求下，否则界面上的分组会重复计数"
            );
        }
    }

    /// **script 型必须真的能跑**：script_entry 里的每个符号都要能在源码里找到。
    ///
    /// 扫描是运行期读文件（与 db_safety_guard 同一手法）：include_str! 只对 src/
    /// 变化重编，跨 crate 的文件改了它不会重新编译，测试会继续用旧的字符串。
    #[test]
    fn every_script_entry_resolves_to_a_real_function() {
        let kernel_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("ac-registry 必须在 crates/ 下")
            .to_path_buf();
        for t in all_tasks() {
            if t.executor == TaskExecutor::Model {
                assert!(
                    t.script_entry.is_empty(),
                    "{} 是纯模型节点，不该有 script_entry",
                    t.id
                );
                continue;
            }
            assert!(
                !t.script_entry.is_empty(),
                "{} 是 {} 型节点却没有 script_entry：说明它没有可执行的内核入口，\
                 那它就不该标成这个执行者",
                t.id,
                t.executor.as_str()
            );
            for entry in &t.script_entry {
                let parts: Vec<&str> = entry.split("::").collect();
                assert!(
                    parts.len() >= 3,
                    "{} 的 script_entry 必须写成 crate::模块::函数，实际: {entry}",
                    t.id
                );
                let crate_dir = parts[0].replace('_', "-");
                let module = parts[1];
                let func = parts[parts.len() - 1];
                let file = kernel_dir
                    .join(&crate_dir)
                    .join("src")
                    .join(format!("{module}.rs"));
                let src = std::fs::read_to_string(&file).unwrap_or_else(|e| {
                    panic!(
                        "{} 引用了 {entry}，但读不到 {}: {e}",
                        t.id,
                        file.display()
                    )
                });
                let needles = [
                    format!("pub fn {func}("),
                    format!("pub fn {func}<"),
                    format!("pub fn {func}\n"),
                ];
                assert!(
                    needles.iter().any(|n| src.contains(n.as_str())),
                    "{} 引用了 {entry}，但 {} 里找不到 pub fn {func}。\n\
                     script 型节点必须指向真实存在的函数——这张表不能靠形容词证明自己。",
                    t.id,
                    file.display()
                );
            }
        }
    }

    /// 模型型节点必须有契约，且契约不能是空话。
    #[test]
    fn every_model_task_has_a_usable_contract() {
        for t in all_tasks() {
            match t.executor {
                TaskExecutor::Script => assert!(
                    t.model_contract.is_none(),
                    "{} 是纯脚本节点，不该带模型契约",
                    t.id
                ),
                TaskExecutor::Model | TaskExecutor::Hybrid => {
                    let c = t.model_contract.as_ref().unwrap_or_else(|| {
                        panic!(
                            "{} 是 {} 型节点，必须给出 ModelContract",
                            t.id,
                            t.executor.as_str()
                        )
                    });
                    assert!(!c.capabilities.is_empty(), "{} 没说需要什么模型能力", t.id);
                    assert!(
                        c.input_schema.contains('{'),
                        "{} 的输入 schema 必须是具体形状，实际: {}",
                        t.id,
                        c.input_schema
                    );
                    assert!(
                        c.output_schema.contains('{'),
                        "{} 的输出 schema 必须是结构化输出，实际: {}",
                        t.id,
                        c.output_schema
                    );
                    assert!(
                        c.why_not_script.chars().count() >= 40,
                        "{} 的 why_not_script 太短，不足以说明为什么脚本做不了",
                        t.id
                    );
                }
            }
        }
    }

    /// **诚实守门**：模型层没有任何实现，所以 model 型节点一律不许标 Implemented。
    ///
    /// 这一条是本模块最重要的测试。它把「本轮到底做没做」从人的自觉
    /// 变成一条会红的断言：以后真接了 provider，这里必须同步改，而不是悄悄放过。
    #[test]
    fn no_model_task_claims_to_be_implemented() {
        for t in all_tasks() {
            if t.executor == TaskExecutor::Model {
                assert_ne!(
                    t.status,
                    TaskStatus::Implemented,
                    "{} 是 model 型节点却标成已实现。ac-llm 本轮只有 LlmProvider trait、\
                     没有任何实现（见 provider.rs 的 ProviderStatus::Ready 注释），\
                     标 Implemented 就是谎报。",
                    t.id
                );
            }
        }
    }

    #[test]
    fn catalog_counts_match_the_task_list() {
        let c = catalog();
        let n = c.tasks.len() as u32;
        assert_eq!(c.counts.total, n);
        assert_eq!(
            c.counts.script + c.counts.model + c.counts.hybrid,
            n,
            "执行者三类必须覆盖全部节点"
        );
        assert_eq!(
            c.counts.implemented + c.counts.partial + c.counts.planned,
            n,
            "状态三档必须覆盖全部节点"
        );
        assert!((0.0..=1.0).contains(&c.script_share));
        assert!(
            c.script_share > 0.6,
            "脚本优先是本轮的核心结论；脚本型占比 {} 与结论不符",
            c.script_share
        );
        assert!(
            c.note.contains("计划") || c.note.contains("Planned"),
            "目录说明必须写清模型节点是计划状态"
        );
        assert_eq!(c.tasks.len(), 25, "节点数变了就同步更新本断言与报告");
    }

    #[test]
    fn every_repair_action_is_routed() {
        // 穷举 RepairAction：加了新动作而没在这里跟上的话，match 会编译失败；
        // 这条测试则钉住「每个动作都有归属说明」。
        let all = [
            RepairAction::None,
            RepairAction::RebuildSearchIndex,
            RepairAction::ClearPreviewCache,
            RepairAction::RescanAssets,
            RepairAction::RepairSourceFile,
            RepairAction::UpgradeProgram,
        ];
        for a in all {
            let r = route_repair_action(a);
            assert_eq!(r.action, a.as_str());
            assert!(!r.task_id.is_empty());
            assert!(!r.note.trim().is_empty(), "{a:?} 没有说明");
            if let Some(exec) = r.executor {
                let t = task(&r.task_id)
                    .unwrap_or_else(|| panic!("{a:?} 指向不存在的任务 {}", r.task_id));
                assert!(
                    t.executor == exec || t.executor == TaskExecutor::Hybrid,
                    "{a:?} 说由 {exec:?} 执行，但任务 {} 是 {:?}",
                    t.id,
                    t.executor
                );
            }
        }
        // 具体归属：中枢自有数据 → 脚本；改用户源文件 → 混合；升级程序 → 无人可自动做。
        assert_eq!(
            route_repair_action(RepairAction::RescanAssets).executor,
            Some(TaskExecutor::Script)
        );
        assert_eq!(
            route_repair_action(RepairAction::RebuildSearchIndex).executor,
            Some(TaskExecutor::Script)
        );
        assert_eq!(
            route_repair_action(RepairAction::RepairSourceFile).executor,
            Some(TaskExecutor::Hybrid)
        );
        assert_eq!(
            route_repair_action(RepairAction::UpgradeProgram).executor,
            None
        );
    }

    #[test]
    fn task_lookup_by_id_works() {
        let t = task("capability.index").expect("capability.index 必须存在");
        assert_eq!(t.executor, TaskExecutor::Script);
        assert_eq!(t.status, TaskStatus::Implemented);
        assert!(task("nope.nope").is_none());
    }

    /// 序列化契约：前端按 snake_case 读，改这里等于改 IPC。
    #[test]
    fn json_shape_is_stable() {
        let c = catalog();
        let v = serde_json::to_value(&c).expect("TaskCatalog 必须可序列化");
        let first = &v["tasks"][0];
        for key in ["id", "display_name", "description", "executor", "status"] {
            assert!(
                first.get(key).is_some(),
                "IPC 字段 {key} 缺失；前端 RoutingPanel 依赖它"
            );
        }
        assert_eq!(first["executor"], serde_json::json!("script"));
        assert_eq!(first["status"], serde_json::json!("implemented"));
        assert!(v.get("counts").is_some());
        assert!(v.get("requirements").is_some());
        assert!(v.get("script_share").is_some());
        // model 型节点必须带 model_contract，script 型必须不带。
        for t in &c.tasks {
            let j = serde_json::to_value(t).unwrap();
            match t.executor {
                TaskExecutor::Script => assert!(j.get("model_contract").is_none()),
                _ => assert!(j.get("model_contract").is_some()),
            }
        }
    }
}

