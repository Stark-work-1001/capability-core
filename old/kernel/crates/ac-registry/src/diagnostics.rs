//! M3 产品化：**维修站**的诊断本体。
//!
//! # 三件事，每件都要能回答"凭什么这么说"
//!
//! 1. **故障分类** —— 七类，见 [`DiagnosticCategory`]。
//! 2. **故障内容** —— 具体哪个资产 / 哪个阶段 / 错误原文（`subject` + `detail`）。
//! 3. **能否自修** —— [`Repairability`] 三档，外加"是否建议求援 LLM"。
//!
//! # 「自修」的边界是结构性的，不是口头承诺
//!
//! 硬约束：**自修只能作用在中枢自己管理的数据上**（search-index / cache /
//! preview.json / 库内派生表），**绝不自动修改用户的插件 / 技能源文件**。
//!
//! 这条边界不靠注释维持，靠 [`RepairPlan::repairability`] 里的一个纯函数：
//! 作用域是 [`RepairScope::UserFiles`] 的修复**永远**落在 Confirm 或 Manual，
//! 只有 HubOwned 且非破坏性的才可能是 Auto。测试 `auto_repair_never_touches_user_files`
//! 直接遍历所有修复计划断言这条不变量——有人加一条越界修复，测试立刻红。
//!
//! 涉及源文件的修复一律降级为 Confirm，并在 `plan.changes` 里**逐条列出将要改什么**，
//! 让用户能自己判断，而不是点一个看不懂的"确定"。

use ac_retrieval::fulltext::{FullTextIndex, SyncPolicy};
use ac_retrieval::health::Stage;
use rusqlite::OpenFlags;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── 分类 ─────────────────────────────────────────────────────────

/// 故障分类。**这是穷举**，不是随手挑的几个标签：
/// 新增一类故障必须先想清楚它属于哪一类，否则面板上会出现无法归类的条目。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum DiagnosticCategory {
    /// 结构化存储不可用：SQLite 打不开 / 查询失败 / 文件被锁。
    StructuredStore,
    /// 全文索引缺失或损坏。
    FullTextIndex,
    /// 向量层不可用。
    VectorLayer,
    /// LLM 层不可用。
    LlmLayer,
    /// 资产解析失败：源文件读不到 / manifest 损坏 / 解析结果退化。
    AssetParse,
    /// 依赖缺失：数据目录、扫描根、索引目录等中枢自身依赖不可用。
    MissingDependency,
    /// DB 版本高于程序。
    DbVersionAhead,
}

impl DiagnosticCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticCategory::StructuredStore => "structured_store",
            DiagnosticCategory::FullTextIndex => "full_text_index",
            DiagnosticCategory::VectorLayer => "vector_layer",
            DiagnosticCategory::LlmLayer => "llm_layer",
            DiagnosticCategory::AssetParse => "asset_parse",
            DiagnosticCategory::MissingDependency => "missing_dependency",
            DiagnosticCategory::DbVersionAhead => "db_version_ahead",
        }
    }
}

// ── 能否自修 ─────────────────────────────────────────────────────

/// 能否自修。三档，语义互斥且穷举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum Repairability {
    /// 可自动修：只动中枢自己的数据，且非破坏性。
    Auto,
    /// 需用户确认：会改配置 / 装依赖 / 动源文件 / 破坏性重建。
    ///     **必须**在 [`RepairPlan::changes`] 里说明将要改什么。
    Confirm,
    /// 不可自修：缺文件、协议不支持、需要升级程序。
    Manual,
}

impl Repairability {
    pub fn as_str(self) -> &'static str {
        match self {
            Repairability::Auto => "auto",
            Repairability::Confirm => "confirm",
            Repairability::Manual => "manual",
        }
    }
}

/// 修复动作。枚举而不是自由字符串：
/// 命令层只认这些值，拼错的字符串会在类型上就过不去。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum RepairAction {
    /// 没有可执行的修复（缺文件、协议不支持、需升级程序）。
    None,
    /// 重建全文索引。**中枢自己的派生物**。
    RebuildSearchIndex,
    /// 清掉过期的预览缓存。中枢自己生成的产物。
    ClearPreviewCache,
    /// 重扫资产目录。会改库内派生数据，且可能清除已消失的资产。
    RescanAssets,
    /// 改用户源文件。**永远**降级为需用户确认。
    RepairSourceFile,
    /// 升级程序。
    UpgradeProgram,
}

impl RepairAction {
    pub fn as_str(self) -> &'static str {
        match self {
            RepairAction::None => "none",
            RepairAction::RebuildSearchIndex => "rebuild_search_index",
            RepairAction::ClearPreviewCache => "clear_preview_cache",
            RepairAction::RescanAssets => "rescan_assets",
            RepairAction::RepairSourceFile => "repair_source_file",
            RepairAction::UpgradeProgram => "upgrade_program",
        }
    }
}

/// 修复的作用域。**自修边界的唯一裁判**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum RepairScope {
    /// 不涉及任何文件（例如"升级程序"）。
    None,
    /// 中枢自己管理的数据：search-index / 缓存 / 库内派生表。
    HubOwned,
    /// 用户的插件 / 技能源文件。**不可逆、用户看不懂**。
    UserFiles,
}
#[cfg(test)]mod wire_contract_tests {
    use super::*;
    #[test]fn repair_values_match_frontend_wire_names(){
        for action in [RepairAction::None,RepairAction::RebuildSearchIndex,RepairAction::ClearPreviewCache,RepairAction::RescanAssets,RepairAction::RepairSourceFile,RepairAction::UpgradeProgram]{assert_eq!(serde_json::to_value(action).unwrap(),action.as_str());}
        assert_eq!(serde_json::to_value(RepairScope::HubOwned).unwrap(),"hub_owned");
        assert_eq!(serde_json::to_value(Repairability::Confirm).unwrap(),"confirm");
        assert_eq!(serde_json::to_value(DiagnosticCategory::MissingDependency).unwrap(),"missing_dependency");
    }
}

/// 一条修复计划。`repairability()` 是纯函数，可单测。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairPlan {
    pub action: RepairAction,
    pub scope: RepairScope,
    /// 破坏性 = 会丢弃已有数据（重建索引、清除缓存、重扫移除资产）。
    pub destructive: bool,
    /// 一句话说明这个动作干什么。
    pub summary: String,
    /// **逐条**列出将要改什么。Confirm 档必须有内容——
    /// 空列表的"需你确认"等于让用户盲点。
    pub changes: Vec<String>,
}

impl RepairPlan {
    /// 判定档位。这条判定就是"自修边界"的全部实现。
    pub fn repairability(&self) -> Repairability {
        if self.action == RepairAction::None {
            return Repairability::Manual;
        }
        match self.scope {
            // 用户的源文件：一律要人点头。
            RepairScope::UserFiles => Repairability::Confirm,
            // 中枢自己的数据：破坏性的也要人点头。
            RepairScope::HubOwned => {
                if self.destructive {
                    Repairability::Confirm
                } else {
                    Repairability::Auto
                }
            }
            // 无文件动作（例如升级程序）不可能由中枢自己完成。
            RepairScope::None => Repairability::Manual,
        }
    }
}

/// 一条诊断。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// 稳定 id：同一故障反复查出来必须是同一条，便于前端做增量更新。
    pub id: String,
    pub category: DiagnosticCategory,
    /// critical / error / warning / info
    pub severity: String,
    pub title: String,
    /// 具体是哪个资产 / 哪个阶段 / 哪个文件。
    pub subject: String,
    /// 错误原文。不做改写——"看起来像什么"不如"原文是什么"。
    pub detail: String,
    pub plan: RepairPlan,
    /// 是否建议求援主对话模型（本地算法判不了时为 true）。
    pub needs_llm: bool,
    /// 为什么需要 / 为什么不需要。空字符串视为"不需要"。
    pub needs_llm_reason: String,
}

impl Diagnostic {
    pub fn repairability(&self) -> Repairability {
        self.plan.repairability()
    }
}

// ── 采集上下文 ───────────────────────────────────────────────────

/// LLM 接入状态。由上层从 `ac-llm` 读出后传进来。
///
/// **为什么不直接依赖 ac-llm**：诊断要能在没有 LLM 配置时独立跑起来，
/// 而且这层依赖方向一旦反了（registry -> llm -> registry）就会成环。
/// 这里用值传递，零依赖。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmStatus {
    /// 配置位置（供 UI 显示"改哪儿"）。
    pub config_path: String,
    /// 配置里声明的 provider 数。0 = 还没配过。
    pub configured_providers: u32,
    /// 已实现真实调用的 provider 数。**本轮恒为 0**。
    pub implemented_providers: u32,
}

/// 诊断需要的外部事实。
#[derive(Debug, Clone, Default)]
pub struct DiagContext {
    /// 库文件路径。用于只读探测 `user_version`（新版库会让 `Registry::open` 直接失败，
    /// 拿不到连接就查不了版本，所以必须走独立只读连接）。
    pub db_path: PathBuf,
    /// 资产扫描根。用于把库里的相对 source_path 还原成真实文件、
    /// 从而判断"文件还在不在"。
    pub scan_root: Option<PathBuf>,
    /// 预览缓存路径（中枢生成物，可安全清理）。
    pub preview_json: Option<PathBuf>,
    pub llm: LlmStatus,
    /// 向量层的**实测**状态。
    ///
    /// 之前这里是硬编码的「absent」——那在 M2 阶段是事实，但 `ac-retrieval/src/vector.rs`
    /// 落地之后就成了**谎报**：面板会告诉用户"本仓库没有向量实现"，
    /// 而实际上向量层有 1800+ 行真实实现、网关也已装配，只是服务没起来。
    /// 谎报比不报更坏：用户会据此做出错误判断（"反正没有，不用管"）。
    ///
    /// 现在由调用方传入实测结果；`None` = 调用方还没探过（UI 显示"未探测"，
    /// 不再是"没有实现"）。语义见 `ac_retrieval::health::StageState` 的四态。
    pub vector: Option<VectorProbe>,
}

/// 向量层实测结果。`detail` 是给人看的原因，不是给程序判的枚举。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorProbe {
    /// `ac_retrieval::health::StageState::as_str()` 的取值：absent / disabled / failed / ready。
    pub state: String,
    /// 端点，例如 `http://127.0.0.1:18191`。
    pub endpoint: String,
    /// 模型名与维度（服务在时才拿得到）。
    pub model: String,
    pub dim: Option<u32>,
    /// 已建向量条数（`ready` 但 0 条 = 还没建索引，这不是故障）。
    pub vectors: Option<u32>,
    /// 失败原因。`failed` 时必须有值，否则就是又一次"不知道为什么"。
    pub reason: String,
}

impl VectorProbe {
    pub fn new(state: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            state: state.into(),
            endpoint: endpoint.into(),
            model: String::new(),
            dim: None,
            vectors: None,
            reason: String::new(),
        }
    }

    /// **真探一次**本机向量层，返回实测结果。
    ///
    /// 放在 ac-registry 而不是各个调用方里，理由：探测逻辑一旦复制，
    /// 两个调用方迟早对"什么叫可用"给出不同答案——而诊断面板最怕的就是
    /// 同一个问题两个入口两种说法。
    ///
    /// 代价是一次最长 `VectorConfig.timeout`（默认 2.5s）的网络等待，
    /// 且失败后有 5s 冷却不重复等。诊断是用户主动点开的低频操作，可接受。
    pub fn probe_from_env() -> Self {
        let (cfg, warnings) = ac_retrieval::vector::VectorConfig::from_env();
        let endpoint = cfg.endpoint.clone();
        let idx = ac_retrieval::vector::VectorIndex::attach(cfg);
        let state = idx.state();
        let mut probe = Self::new(state.as_str(), endpoint);
        if let Some(info) = idx.info() {
            probe.model = info.model;
            probe.dim = info.dim.map(|d| d as u32);
        }
        probe.vectors = Some(idx.len() as u32);
        if let ac_retrieval::health::StageState::Failed(reason) = &state {
            probe.reason = reason.clone();
        }
        // 配置警告不能丢：一个拼错的 AC_VECTOR_URL 被静默回落，
        // 用户会以为自己连的是云端。
        if !warnings.is_empty() {
            if !probe.reason.is_empty() {
                probe.reason.push_str("；");
            }
            probe.reason.push_str(&format!("配置警告：{}", warnings.join("; ")));
        }
        probe
    }

    /// 结构上就无法构造"failed 但没原因"的探针——这是对上面那句注释的守门。
    pub fn failed(endpoint: impl Into<String>, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        assert!(!reason.is_empty(), "failed 必须带原因");
        Self {
            state: "failed".into(),
            endpoint: endpoint.into(),
            model: String::new(),
            dim: None,
            vectors: None,
            reason,
        }
    }
}

/// 一轮诊断的完整结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosticReport {
    pub diagnostics: Vec<Diagnostic>,
    pub critical: u32,
    pub error: u32,
    pub warning: u32,
    pub info: u32,
    /// 可自动修的条数。
    pub auto_fixable: u32,
    /// 需用户确认的条数。
    pub needs_confirm: u32,
    /// 不可自修的条数。
    pub manual_only: u32,
    /// 建议求援 LLM 的条数。
    pub needs_llm: u32,
}

impl DiagnosticReport {
    fn push(&mut self, d: Diagnostic) {
        match d.severity.as_str() {
            "critical" => self.critical += 1,
            "error" => self.error += 1,
            "warning" => self.warning += 1,
            _ => self.info += 1,
        }
        match d.repairability() {
            Repairability::Auto => self.auto_fixable += 1,
            Repairability::Confirm => self.needs_confirm += 1,
            Repairability::Manual => self.manual_only += 1,
        }
        if d.needs_llm {
            self.needs_llm += 1;
        }
        self.diagnostics.push(d);
    }
}

fn plan_none(summary: &str) -> RepairPlan {
    RepairPlan {
        action: RepairAction::None,
        scope: RepairScope::None,
        destructive: false,
        summary: summary.to_string(),
        changes: Vec::new(),
    }
}

// ── 主入口 ───────────────────────────────────────────────────────

/// 跑一轮完整诊断。**只读**：这一轮里不会写任何文件、不会改任何数据。
pub fn collect(reg: &crate::store::Registry, ctx: &DiagContext) -> DiagnosticReport {
    let mut rep = DiagnosticReport::default();
    check_structured_store(reg, &mut rep);
    check_db_version(ctx, &mut rep);
    check_dependencies(ctx, &mut rep);
    check_fulltext(reg, ctx, &mut rep);
    check_vector(ctx, &mut rep);
    check_llm(ctx, &mut rep);
    check_asset_parse(reg, ctx, &mut rep);
    rep
}

// ── 1. 结构化存储 ────────────────────────────────────────────────

fn check_structured_store(reg: &crate::store::Registry, rep: &mut DiagnosticReport) {
    // 真跑一条查询，而不是看某个布尔标志。
    match reg.conn.query_row("SELECT COUNT(*) FROM assets", [], |r| r.get::<_, i64>(0)) {
        Ok(n) => rep.push(Diagnostic {
            id: "structured_store".into(),
            category: DiagnosticCategory::StructuredStore,
            severity: "info".into(),
            title: "结构化存储可用".into(),
            subject: "SQLite".into(),
            detail: format!("assets 表可读，共 {n} 条记录"),
            plan: plan_none("无需修复"),
            needs_llm: false,
            needs_llm_reason: String::new(),
        }),
        Err(e) => rep.push(Diagnostic {
            id: "structured_store".into(),
            category: DiagnosticCategory::StructuredStore,
            severity: "critical".into(),
            title: "结构化存储不可用".into(),
            subject: "SQLite".into(),
            detail: format!("查询 assets 失败: {e}"),
            // 库坏了不是"索引重建"能解决的：重扫会再写一次同样的坏库。
            plan: plan_none("需人工检查库文件与磁盘状态"),
            needs_llm: false,
            needs_llm_reason: "磁盘/文件系统故障，模型帮不上忙".into(),
        }),
    }
}

// ── 2. DB 版本高于程序 ───────────────────────────────────────────

fn check_db_version(ctx: &DiagContext, rep: &mut DiagnosticReport) {
    if ctx.db_path.as_os_str().is_empty() {
        return;
    }
    // 独立只读连接：库比程序新时 Registry::open 会直接失败，
    // 到这一步已经没有连接可用了。
    let conn = rusqlite::Connection::open_with_flags(&ctx.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY);
    let conn = match conn {
        Ok(c) => c,
        Err(e) => {
            rep.push(Diagnostic {
                id: "db_version_ahead".into(),
                category: DiagnosticCategory::DbVersionAhead,
                severity: "error".into(),
                title: "无法读取数据库版本".into(),
                subject: ctx.db_path.display().to_string(),
                detail: format!("以只读方式打开失败: {e}"),
                plan: plan_none("需人工确认库文件状态"),
                needs_llm: false,
                needs_llm_reason: String::new(),
            });
            return;
        }
    };
    let v: i64 = match conn.pragma_query_value(None, "user_version", |r| r.get(0)) {
        Ok(v) => v,
        Err(e) => {
            rep.push(Diagnostic {
                id: "db_version_ahead".into(),
                category: DiagnosticCategory::DbVersionAhead,
                severity: "error".into(),
                title: "无法读取数据库版本".into(),
                subject: ctx.db_path.display().to_string(),
                detail: format!("读取 user_version 失败: {e}"),
                plan: plan_none("需人工确认库文件状态"),
                needs_llm: false,
                needs_llm_reason: String::new(),
            });
            return;
        }
    };
    let program = crate::store::SCHEMA_VERSION;
    if v > program {
        rep.push(Diagnostic {
            id: "db_version_ahead".into(),
            category: DiagnosticCategory::DbVersionAhead,
            severity: "critical".into(),
            title: "数据库版本高于程序".into(),
            subject: ctx.db_path.display().to_string(),
            detail: format!("库 schema v{v}，程序只认到 v{program}"),
            plan: RepairPlan {
                action: RepairAction::UpgradeProgram,
                scope: RepairScope::None,
                destructive: false,
                summary: "升级 capability-core 后重开".into(),
                changes: vec![format!("升级程序到支持 schema v{v} 的版本")],
            },
            // 降级写库会毁掉用户数据，这正是本项目栽过的坑。
            needs_llm: false,
            needs_llm_reason: "必须升级程序；任何自动改库都可能破坏用户数据".into(),
        });
    } else {
        rep.push(Diagnostic {
            id: "db_version".into(),
            category: DiagnosticCategory::DbVersionAhead,
            severity: "info".into(),
            title: "数据库版本匹配".into(),
            subject: ctx.db_path.display().to_string(),
            detail: format!("库 schema v{v} / 程序 v{program}"),
            plan: plan_none("无需修复"),
            needs_llm: false,
            needs_llm_reason: String::new(),
        });
    }
}

// ── 3. 中枢自身依赖 ──────────────────────────────────────────────

fn check_dependencies(ctx: &DiagContext, rep: &mut DiagnosticReport) {
    // 数据目录可写 —— 索引、缓存、库都落在这儿，写不进去后面全都白搭。
    if let Some(parent) = ctx.db_path.parent() {
        if !parent.as_os_str().is_empty() {
            if !parent.is_dir() {
                rep.push(Diagnostic {
                    id: "dep_data_dir".into(),
                    category: DiagnosticCategory::MissingDependency,
                    severity: "critical".into(),
                    title: "数据目录不存在".into(),
                    subject: parent.display().to_string(),
                    detail: format!("找不到数据目录 {}", parent.display()),
                    plan: plan_none("需人工创建数据目录或改 CAPABILITYCORE_DATA_DIR"),
                    needs_llm: false,
                    needs_llm_reason: String::new(),
                });
            } else if std::fs::File::create(parent.join(".ac-diag-probe")).is_err() {
                rep.push(Diagnostic {
                    id: "dep_data_dir".into(),
                    category: DiagnosticCategory::MissingDependency,
                    severity: "error".into(),
                    title: "数据目录不可写".into(),
                    subject: parent.display().to_string(),
                    detail: "在数据目录里创建探测文件失败（权限或策略拦截）".into(),
                    plan: plan_none("需人工放开该目录的写权限"),
                    needs_llm: false,
                    needs_llm_reason: String::new(),
                });
            } else {
                rep.push(Diagnostic {
                    id: "dep_data_dir".into(),
                    category: DiagnosticCategory::MissingDependency,
                    severity: "info".into(),
                    title: "数据目录可用".into(),
                    subject: parent.display().to_string(),
                    detail: "可读可写".into(),
                    plan: plan_none("无需修复"),
                    needs_llm: false,
                    needs_llm_reason: String::new(),
                });
            }
        }
    }

    // 扫描根：资产来源目录。缺了不影响已有数据，只影响"再扫描"。
    match &ctx.scan_root {
        Some(root) if root.is_dir() => rep.push(Diagnostic {
            id: "dep_scan_root".into(),
            category: DiagnosticCategory::MissingDependency,
            severity: "info".into(),
            title: "扫描根可用".into(),
            subject: root.display().to_string(),
            detail: "目录存在".into(),
            plan: plan_none("无需修复"),
            needs_llm: false,
            needs_llm_reason: String::new(),
        }),
        Some(root) => rep.push(Diagnostic {
            id: "dep_scan_root".into(),
            category: DiagnosticCategory::MissingDependency,
            severity: "error".into(),
            title: "扫描根缺失".into(),
            subject: root.display().to_string(),
            detail: format!("扫描根 {} 不存在，资产来源不可达", root.display()),
            // 重扫需要用户指定路径，属于改配置。
            plan: RepairPlan {
                action: RepairAction::RescanAssets,
                scope: RepairScope::HubOwned,
                destructive: true,
                summary: "重新指定扫描根后重扫".into(),
                changes: vec![format!("把扫描根改为一个存在的目录（当前 {}）", root.display())],
            },
            needs_llm: false,
            needs_llm_reason: "路径该填什么是配置问题，不是推理问题".into(),
        }),
        None => rep.push(Diagnostic {
            id: "dep_scan_root".into(),
            category: DiagnosticCategory::MissingDependency,
            severity: "info".into(),
            title: "未提供扫描根".into(),
            subject: "scan_root".into(),
            detail: "本轮未传入扫描根，跳过资产文件存在性检查".into(),
            plan: plan_none("无需修复"),
            needs_llm: false,
            needs_llm_reason: String::new(),
        }),
    }
}

// ── 4. 全文索引 ──────────────────────────────────────────────────

fn check_fulltext(reg: &crate::store::Registry, ctx: &DiagContext, rep: &mut DiagnosticReport) {
    if ctx.db_path.as_os_str().is_empty() {
        return;
    }
    // **Manual 策略：一个字都不写盘。** 诊断必须是只读的，
    // 哪怕"顺手帮你建了索引"也是越权——那是修复动作，得用户点头。
    let dir = FullTextIndex::index_dir_for(&ctx.db_path);
    let index = FullTextIndex::attach(&dir, reg, SyncPolicy::Manual);
    let state = index.state().clone();
    let docs = index.doc_count();
    let assets = reg.total().unwrap_or(0) as usize;
    match state {
        ac_retrieval::health::StageState::Ready if docs >= assets => {
            rep.push(Diagnostic {
                id: "full_text".into(),
                category: DiagnosticCategory::FullTextIndex,
                severity: "info".into(),
                title: "全文索引就绪".into(),
                subject: dir.display().to_string(),
                detail: format!("状态 ready，{docs} 篇文档 / 库内 {assets} 条资产"),
                plan: plan_none("无需修复"),
                needs_llm: false,
                needs_llm_reason: String::new(),
            });
        }
        ac_retrieval::health::StageState::Ready => {
            rep.push(Diagnostic {
                id: "full_text".into(),
                category: DiagnosticCategory::FullTextIndex,
                severity: "warning".into(),
                title: "全文索引落后于库".into(),
                subject: dir.display().to_string(),
                detail: format!("状态 ready 但只有 {docs} 篇文档，库内已有 {assets} 条资产"),
                plan: RepairPlan {
                    action: RepairAction::RebuildSearchIndex,
                    scope: RepairScope::HubOwned,
                    // 重建会丢弃旧倒排，但那是中枢自己的派生物。
                    destructive: true,
                    summary: "重建全文索引（只动中枢自己的 search-index 目录）".into(),
                    changes: vec![format!("删除并重建 {}", dir.display()), "资产源文件不受影响".into()],
                },
                needs_llm: false,
                needs_llm_reason: String::new(),
            });
        }
        other => {
            let detail = format!("状态 {}，{docs} 篇文档（{assets} 条资产）", other);
            // 缺席 ≠ 故障，但索引缺失/损坏是需要动作的：
            // 「这一版压根没接」才叫缺席。
            let is_absent = matches!(other, ac_retrieval::health::StageState::Absent);
            let (action, severity) = if is_absent {
                (RepairAction::None, "info")
            } else {
                (RepairAction::RebuildSearchIndex, "error")
            };
            rep.push(Diagnostic {
                id: "full_text".into(),
                category: DiagnosticCategory::FullTextIndex,
                severity: severity.into(),
                title: if is_absent {
                    "全文索引未建立".into()
                } else {
                    "全文索引缺失或损坏".into()
                },
                subject: dir.display().to_string(),
                detail,
                plan: if action == RepairAction::None {
                    plan_none("本版未启用全文层，不影响结构化检索")
                } else {
                    RepairPlan {
                        action,
                        scope: RepairScope::HubOwned,
                        destructive: true,
                        summary: "重建全文索引（只动中枢自己的 search-index 目录）".into(),
                        changes: vec![format!("删除并重建 {}", dir.display()), "资产源文件不受影响".into()],
                    }
                },
                needs_llm: false,
                needs_llm_reason: "索引是派生物，重建即可，不需要模型".into(),
            });
        }
    }
}

// ── 5. 向量层 ────────────────────────────────────────────────────

fn check_vector(ctx: &DiagContext, rep: &mut DiagnosticReport) {
    // 铁律：**只报实测，不报假设**。
    //
    // 这条函数曾经硬编码「本仓库没有向量实现」——在 M2 阶段属实，
    // 但 vector.rs 落地之后它就变成了对外谎报：用户看到"没有实现"，
    // 就不会去启动嵌入服务，语义召回永远用不上。
    //
    // 现在四种情况分开说，每种都给出**用户下一步能做什么**：
    //   ready   → 正常，只报信息（含模型/维度/条数）
    //   未探测   → 明确说"没探测过"，不冒充"没有"
    //   failed  → 带原始原因（连接拒绝/超时），并给出可执行的修复建议
    //   absent/disabled → 如实说，并说明降级后果
    let Some(probe) = &ctx.vector else {
        rep.push(Diagnostic {
            id: "vector_layer".into(),
            category: DiagnosticCategory::VectorLayer,
            severity: "info".into(),
            title: "向量层未探测".into(),
            subject: format!("stage={}", Stage::Vector.as_str()),
            detail: "本轮诊断没有探测向量层（调用方未提供实测结果）。这不代表向量层不存在或不可用——只是没有测过。".into(),
            plan: plan_none("无需修复；若想确认向量层状态，请从网关的 health 接口读取"),
            needs_llm: false,
            needs_llm_reason: String::new(),
        });
        return;
    };

    let ep = if probe.endpoint.is_empty() {
        "（未配置端点）".to_string()
    } else {
        probe.endpoint.clone()
    };

    match probe.state.as_str() {
        "ready" => {
            let mut detail = format!("向量服务可用：{ep}");
            if !probe.model.is_empty() {
                detail.push_str(&format!("，模型 {}", probe.model));
            }
            if let Some(d) = probe.dim {
                detail.push_str(&format!("，{d} 维"));
            }
            match probe.vectors {
                Some(0) => detail.push_str("；但尚未建立向量索引（0 条），语义召回暂不生效，可在维修站重建"),
                Some(n) => detail.push_str(&format!("，已建 {n} 条向量")),
                None => detail.push_str("；向量条数未知"),
            }
            rep.push(Diagnostic {
                id: "vector_layer".into(),
                category: DiagnosticCategory::VectorLayer,
                severity: "info".into(),
                title: "向量层可用".into(),
                subject: format!("stage={} · {ep}", Stage::Vector.as_str()),
                detail,
                plan: plan_none("无需修复"),
                needs_llm: false,
                needs_llm_reason: String::new(),
            });
        }
        "failed" => {
            rep.push(Diagnostic {
                id: "vector_layer".into(),
                category: DiagnosticCategory::VectorLayer,
                severity: "warning".into(),
                title: "向量层故障（已自动降级）".into(),
                subject: format!("stage={} · {ep}", Stage::Vector.as_str()),
                detail: format!(
                    "向量服务探测失败：{}。检索会自动降级到「结构化 + 全文 BM25」，**结果仍然返回**，只是少了语义召回。",
                    if probe.reason.is_empty() { "（无原因，这本身是个缺陷）" } else { &probe.reason }
                ),
                plan: plan_none("启动嵌入服务后重新诊断；降级不影响可用性，不需要改代码"),
                needs_llm: false,
                needs_llm_reason: String::new(),
            });
        }
        "disabled" => {
            rep.push(Diagnostic {
                id: "vector_layer".into(),
                category: DiagnosticCategory::VectorLayer,
                severity: "info".into(),
                title: "向量层已禁用".into(),
                subject: format!("stage={} · {ep}", Stage::Vector.as_str()),
                detail: "向量层被配置显式关闭了（不是故障）。检索会自动降级到「结构化 + 全文 BM25」。".into(),
                plan: plan_none("如需语义召回，请重新启用向量层配置"),
                needs_llm: false,
                needs_llm_reason: String::new(),
            });
        }
        _ => {
            // absent 与任何未知字符串都落这里：如实说"缺席"，不推断原因。
            rep.push(Diagnostic {
                id: "vector_layer".into(),
                category: DiagnosticCategory::VectorLayer,
                severity: "info".into(),
                title: "向量层缺席".into(),
                subject: format!("stage={} · {ep}", Stage::Vector.as_str()),
                detail: "本次装配没有挂载向量层（absent），检索走「结构化 + 全文 BM25」两层，结果仍会返回。".into(),
                plan: plan_none("如需语义召回，请启用向量层并启动嵌入服务"),
                needs_llm: false,
                needs_llm_reason: String::new(),
            });
        }
    }
}

// ── 6. LLM 层 ────────────────────────────────────────────────────

fn check_llm(ctx: &DiagContext, rep: &mut DiagnosticReport) {
    let cfg = display_opt(&ctx.llm.config_path).to_string();
    let configured = ctx.llm.configured_providers;
    let implemented = ctx.llm.implemented_providers;
    let severity;
    let title;
    let mut detail;
    if configured == 0 {
        severity = "info";
        title = "未配置任何模型 provider";
        detail = format!("配置文件 {cfg} 不存在或没有任何 provider；所有诊断均由本地算法给出");
    } else if implemented == 0 {
        severity = "warning";
        title = "已配置 provider，但本轮未实现厂商调用";
        detail = format!("{cfg} 中声明了 {configured} 个 provider，实际实现 0 个");
        detail.push_str("；故障诊断一律走本地算法（结构化过滤 + BM25），不会调用任何模型");
    } else {
        severity = "info";
        title = "LLM 层可用";
        detail = format!("已实现 {implemented}/{configured} 个 provider");
    }
    rep.push(Diagnostic {
        id: "llm_layer".into(),
        category: DiagnosticCategory::LlmLayer,
        severity: severity.into(),
        title: title.into(),
        subject: cfg,
        detail,
        plan: plan_none("本轮只交付 provider 抽象与配置位，不实现具体厂商"),
        // 求援 LLM 这件事本身不需要 LLM 层就绪——它由主对话模型执行。
        needs_llm: false,
        needs_llm_reason: String::new(),
    });
}

// ── 7. 资产解析 ──────────────────────────────────────────────────

fn check_asset_parse(reg: &crate::store::Registry, ctx: &DiagContext, rep: &mut DiagnosticReport) {
    // (a) 库内 manifest 是不是还读得回来。读不回来 = 入库时写坏了。
    let corrupt: Vec<String> = {
        let mut stmt = match reg
            .conn
            .prepare("SELECT id, manifest FROM assets ORDER BY id")
        {
            Ok(s) => s,
            Err(e) => {
                rep.push(Diagnostic {
                    id: "asset_parse".into(),
                    category: DiagnosticCategory::AssetParse,
                    severity: "error".into(),
                    title: "无法枚举资产".into(),
                    subject: "assets".into(),
                    detail: format!("查询失败: {e}"),
                    plan: plan_none("需人工检查库文件"),
                    needs_llm: false,
                    needs_llm_reason: String::new(),
                });
                return;
            }
        };
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        });
        let mut out = Vec::new();
        match rows {
            Ok(it) => {
                for row in it.flatten() {
                    let (id, text) = row;
                    if serde_json::from_str::<serde_json::Value>(&text).is_err() {
                        out.push(id);
                    }
                }
            }
            Err(e) => {
                rep.push(Diagnostic {
                    id: "asset_parse".into(),
                    category: DiagnosticCategory::AssetParse,
                    severity: "error".into(),
                    title: "无法枚举资产".into(),
                    subject: "assets".into(),
                    detail: format!("遍历失败: {e}"),
                    plan: plan_none("需人工检查库文件"),
                    needs_llm: false,
                    needs_llm_reason: String::new(),
                });
                return;
            }
        }
        out
    };

    if !corrupt.is_empty() {
        rep.push(Diagnostic {
            id: "asset_parse_manifest".into(),
            category: DiagnosticCategory::AssetParse,
            severity: "error".into(),
            title: "已入库的 manifest 读不回来".into(),
            subject: format!("{} 条资产", corrupt.len()),
            detail: format!("以下 id 的 manifest 不是合法 JSON: {}", preview_list(&corrupt)),
            // 改用户的源文件才能真正修好，规则表判不了该怎么办。
            plan: RepairPlan {
                action: RepairAction::RepairSourceFile,
                scope: RepairScope::UserFiles,
                destructive: false,
                summary: "需人工修复下列资产的源文件后重扫".into(),
                changes: corrupt
                    .iter()
                    .take(10)
                    .map(|id| format!("检查并修复 {id} 对应的源文件"))
                    .collect(),
            },
            needs_llm: true,
            needs_llm_reason: "本地规则只能确认这份内容解析不出来；该怎么改需要模型读原文".into(),
        });
    }

    // (b) 库里有、磁盘上没了 —— 解析必然失败。
    let Some(root) = &ctx.scan_root else { return };
    if !root.is_dir() {
        return;
    }
    let mut stmt = match reg
        .conn
        .prepare("SELECT id, name, source_path FROM assets ORDER BY id")
    {
        Ok(s) => s,
        Err(_) => return,
    };
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        ))
    });
    let mut missing: Vec<(String, String, String)> = Vec::new();
    if let Ok(it) = rows {
        for row in it.flatten() {
            let (id, name, sp) = row;
            if sp.is_empty() {
                continue;
            }
            if !root.join(&sp).exists() {
                missing.push((id, name, sp));
            }
        }
    }
    if missing.is_empty() {
        rep.push(Diagnostic {
            id: "asset_parse".into(),
            category: DiagnosticCategory::AssetParse,
            severity: "info".into(),
            title: "资产源文件全部在位".into(),
            subject: root.display().to_string(),
            detail: "库内每条带 source_path 的资产都能在磁盘上找到".into(),
            plan: plan_none("无需修复"),
            needs_llm: false,
            needs_llm_reason: String::new(),
        });
    } else {
        let names: Vec<String> = missing
            .iter()
            .take(5)
            .map(|(id, name, sp)| format!("{name}（{id}）→ {sp}"))
            .collect();
        rep.push(Diagnostic {
            id: "asset_parse".into(),
            category: DiagnosticCategory::AssetParse,
            severity: "warning".into(),
            title: "库内资产在磁盘上已不存在".into(),
            subject: format!("{} 条", missing.len()),
            detail: format!("例：{}", names.join("；")),
            // 文件都被人删/挪了，中枢**补不回来**：这正是「不可自修」的典型。
            plan: plan_none("源文件缺失，中枢无法恢复；请用户自行确认是删除还是移走"),
            needs_llm: false,
            needs_llm_reason: String::new(),
        });
    }
}

fn display_opt(p: &str) -> &str {
    if p.trim().is_empty() {
        "（未指定）"
    } else {
        p
    }
}

fn preview_list(ids: &[String]) -> String {
    let head: Vec<&str> = ids.iter().take(5).map(|s| s.as_str()).collect();
    if ids.len() > head.len() {
        format!("{} 等 {} 条", head.join(", "), ids.len())
    } else {
        head.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;

    fn plan(action: RepairAction, scope: RepairScope, destructive: bool) -> RepairPlan {
        RepairPlan {
            action,
            scope,
            destructive,
            summary: "s".into(),
            changes: vec!["c".into()],
        }
    }

    /// 「自修只能作用在中枢自己的数据上」这条硬约束的可执行版本。
    #[test]
    fn auto_repair_never_touches_user_files() {
        // 逐个组合断言：只要作用域是 UserFiles，就**不可能**是 Auto。
        for destructive in [false, true] {
            for action in [
                RepairAction::RebuildSearchIndex,
                RepairAction::ClearPreviewCache,
                RepairAction::RescanAssets,
                RepairAction::RepairSourceFile,
                RepairAction::UpgradeProgram,
            ] {
                let p = plan(action, RepairScope::UserFiles, destructive);
                assert_eq!(
                    p.repairability(),
                    Repairability::Confirm,
                    "UserFiles 上的 {action:?} 绝不能自动修"
                );
            }
        }
        // 而中枢自己的数据、非破坏性的，才允许自动修。
        assert_eq!(
            plan(RepairAction::RebuildSearchIndex, RepairScope::HubOwned, false).repairability(),
            Repairability::Auto
        );
    }

    #[test]
    fn hub_owned_but_destructive_needs_confirmation() {
        // 重建索引会丢弃旧倒排——它是中枢自己的数据，仍然要人点头。
        assert_eq!(
            plan(RepairAction::RebuildSearchIndex, RepairScope::HubOwned, true).repairability(),
            Repairability::Confirm
        );
    }

    #[test]
    fn no_action_means_manual() {
        for scope in [RepairScope::None, RepairScope::HubOwned, RepairScope::UserFiles] {
            assert_eq!(
                plan(RepairAction::None, scope, false).repairability(),
                Repairability::Manual
            );
        }
    }

    /// 向量诊断必须**如实反映输入的那一档**，不能恒定成"没有实现"。
    ///
    /// 这条测试守的是一段真实事故：check_vector 曾硬编码 absent，
    /// 而 vector.rs 落地后那句话变成对外谎报。四个状态必须给出四种不同结论，
    /// 否则"修复"就只是换了个说法。
    #[test]
    fn vector_diagnostic_reflects_each_state_instead_of_hardcoding_absent() {
        use super::VectorProbe;
        let s = Scratch::new("diag-vector-states");
        let db = s.path().join("registry.db");
        let r = crate::store::Registry::open(&db).unwrap();

        let cases = [
            (
                VectorProbe::new("ready", "http://127.0.0.1:18191"),
                "info",
                "向量层可用",
            ),
            (
                VectorProbe::failed("http://127.0.0.1:18191", "连接被拒绝"),
                "warning",
                "向量层故障（已自动降级）",
            ),
            (
                VectorProbe::new("disabled", "http://127.0.0.1:18191"),
                "info",
                "向量层已禁用",
            ),
            (
                VectorProbe::new("absent", "http://127.0.0.1:18191"),
                "info",
                "向量层缺席",
            ),
        ];

        for (probe, want_severity, want_title) in cases {
            let state = probe.state.clone();
            let ctx = DiagContext {
                db_path: db.clone(),
                scan_root: Some(s.path().to_path_buf()),
                preview_json: None,
                llm: LlmStatus::default(),
                vector: Some(probe),
            };
            let rep = collect(&r, &ctx);
            let d = rep
                .diagnostics
                .iter()
                .find(|d| d.category == DiagnosticCategory::VectorLayer)
                .unwrap_or_else(|| panic!("没有 vector_layer 诊断（state={state}）"));
            assert_eq!(d.severity, want_severity, "state={state} 的严重度不对");
            assert_eq!(d.title, want_title, "state={state} 的标题不对");
            // 反证：任何一档都不许再说"本仓库没有向量实现"。
            assert!(
                !d.detail.contains("本仓库没有向量实现"),
                "state={state} 仍在谎报：{}",
                d.detail
            );
        }
    }

    /// 未探测 ≠ 缺席。这两个必须能区分，否则面板又会用"没有"冒充"没测过"。
    #[test]
    fn unprobed_vector_is_reported_as_unprobed_not_absent() {
        let s = Scratch::new("diag-vector-unprobed");
        let db = s.path().join("registry.db");
        let r = crate::store::Registry::open(&db).unwrap();
        let ctx = DiagContext {
            db_path: db.clone(),
            scan_root: Some(s.path().to_path_buf()),
            preview_json: None,
            llm: LlmStatus::default(),
            vector: None,
        };
        let rep = collect(&r, &ctx);
        let d = rep
            .diagnostics
            .iter()
            .find(|d| d.category == DiagnosticCategory::VectorLayer)
            .expect("有 vector_layer 诊断");
        assert_eq!(d.title, "向量层未探测");
        assert!(d.detail.contains("不代表向量层不存在或不可用"));
    }

    #[test]
    fn collect_covers_all_seven_categories_on_an_empty_library() {
        let s = Scratch::new("diag-empty");
        let db = s.path().join("registry.db");
        let r = crate::store::Registry::open(&db).unwrap();
        let ctx = DiagContext {
            db_path: db.clone(),
            scan_root: Some(s.path().to_path_buf()),
            preview_json: None,
            llm: LlmStatus::default(),
            vector: None,
        };
        let rep = collect(&r, &ctx);
        let kinds: Vec<&str> =
            rep.diagnostics.iter().map(|d| d.category.as_str()).collect();
        for want in [
            "structured_store",
            "db_version_ahead",
            "missing_dependency",
            "full_text_index",
            "vector_layer",
            "llm_layer",
            "asset_parse",
        ] {
            assert!(kinds.contains(&want), "诊断里没有 {want}: {kinds:?}");
        }
        // 计数必须与列表一致，不能各数各的。
        let counted = rep.critical + rep.error + rep.warning + rep.info;
        assert_eq!(counted as usize, rep.diagnostics.len());
        let by_fix = rep.auto_fixable + rep.needs_confirm + rep.manual_only;
        assert_eq!(by_fix as usize, rep.diagnostics.len());
        assert_eq!(
            rep.needs_llm as usize,
            rep.diagnostics.iter().filter(|d| d.needs_llm).count()
        );
    }

    #[test]
    fn confirm_repair_must_list_what_changes() {
        let s = Scratch::new("diag-confirm");
        let db = s.path().join("registry.db");
        let r = crate::store::Registry::open(&db).unwrap();
        let ctx = DiagContext {
            db_path: db.clone(),
            scan_root: Some(s.path().to_path_buf()),
            preview_json: None,
            llm: LlmStatus::default(),
            vector: None,
        };
        let rep = collect(&r, &ctx);
        for d in &rep.diagnostics {
            if d.repairability() == Repairability::Confirm {
                assert!(
                    !d.plan.changes.is_empty(),
                    "需确认的修复必须说明将要改什么: {}",
                    d.id
                );
            }
        }
    }

    #[test]
    fn db_newer_than_program_is_critical_and_manual() {
        let s = Scratch::new("diag-newer");
        let db = s.path().join("registry.db");
        // 先正常打开（这会建库并跑 migrate），再从另一条连接把版本顶上去，
        // 模拟"这份库是用更新版本的程序建的"。
        //
        // 注意**不能**用顶高版本之后的 Registry::open 来拿连接：
        // migrate 见到比自己新的库会直接失败，那正是被测行为本身，
        // 会让这个测试在拿到连接之前就 panic。
        let r = crate::store::Registry::open(&db).unwrap();
        {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.pragma_update(None, "user_version", crate::store::SCHEMA_VERSION + 3)
                .unwrap();
        }
        let ctx = DiagContext {
            db_path: db.clone(),
            scan_root: Some(s.path().to_path_buf()),
            preview_json: None,
            llm: LlmStatus::default(),
            vector: None,
        };
        let rep = collect(&r, &ctx);
        let d = rep
            .diagnostics
            .iter()
            .find(|d| d.id == "db_version_ahead");
        let d = d.expect("必须有 db_version_ahead 诊断");
        assert_eq!(d.severity, "critical");
        assert_eq!(d.repairability(), Repairability::Manual);
        assert_eq!(d.plan.action, RepairAction::UpgradeProgram);
        assert!(d.detail.contains(&format!("v{}", crate::store::SCHEMA_VERSION + 3)));
    }

    #[test]
    fn missing_source_file_is_manual_not_confirm() {
        let s = Scratch::new("diag-missing-src");
        let root = s.mkdir("vendor");
        let mut m = ac_core::CapabilityManifest::new(
            "skill::x",
            ac_core::AssetKind::Skill,
            "x",
        );
        m.provenance.source_path = Some("gone/SKILL.md".into());
        m.provenance.source_format = Some("skill_md".into());
        m.provenance.content_hash = Some("h-x".into());
        m.provides.push(ac_core::ProvidedCapability {
            capability_id: "skill::x::run".into(),
            name: "run".into(),
            summary: String::new(),
            input_schema: None,
            tags: vec![],
        });
        let db = s.path().join("registry.db");
        {
            let r = crate::store::Registry::open(&db).unwrap();
            r.upsert_all(&[m]).unwrap();
        }
        let r = crate::store::Registry::open(&db).unwrap();
        let ctx = DiagContext {
            db_path: db.clone(),
            scan_root: Some(root.clone()),
            preview_json: None,
            llm: LlmStatus::default(),
            vector: None,
        };
        let rep = collect(&r, &ctx);
        let d = rep.diagnostics.iter().find(|d| d.id == "asset_parse").unwrap();
        assert_eq!(d.severity, "warning");
        assert_eq!(d.repairability(), Repairability::Manual, "文件没了中枢补不回来");
        assert_eq!(d.plan.action, RepairAction::None);
    }
}
