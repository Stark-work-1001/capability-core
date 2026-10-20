//! 健康 / 能力模型 —— M2「永远不会失败的兜底检索层」的机制本体。
//!
//! # 这解决的是设计文档里最难的一条完成标志
//!
//! M2 的完成标志原文：
//!
//!   **在向量库和 LLM 全部不可用时仍能返回结果**
//!
//! 这句话不是一个「降级开关」，它是一条**结构性质**：任何可选阶段
//! （向量召回、LLM 消歧）都不参与「查询能否成功」这个判定。
//! 本模块把这条性质做成**可执行的形状**：
//!
//! - 一个 `Stage` 枚举描述整条链路上每个阶段的身份；
//! - 每个阶段有 `StageState`：缺席 / 禁用 / 故障 / 就绪；
//! - 查询结果永远带一份 `DegradationReport`，逐阶段说清「为什么没用上」。
//!
//! 于是「向量库和 LLM 全部不可用时仍能返回结果」这句话，
//! 在代码里就等于：**结构化基线是唯一必需阶段，其余全是可选项**。
//! 只要结构化基线就绪，查询必定返回；向量/LLM 的状态如何都不影响这一点。
//!
//! # 为什么「缺席」和「故障」必须分开
//!
//! 这两种状态对上层的信息量完全不同：
//!
//! - **缺席（Absent）**：这一版压根没接向量库（M2 阶段就是如此）。
//!   不是故障，是事实。
//! - **禁用（Disabled）**：接了，但用户/配置把它关了。
//! - **故障（Failed）**：接了、开了、但这次调用失败（超时/连接拒绝）。
//!   这是唯一需要真正告警的一种。
//!
//! 把三者混成一个 `available: bool`，上层就分不清「没这能力」和
//! 「有这能力但它挂了」——而这两者的处置完全不同。前者无需处理，
//! 后者需要熔断与日志。

use std::fmt;

/// 检索链路上的一个阶段。
///
/// `Structured` 是**基线**，其余全是可选层。基线之外的一切都可能缺席、
/// 禁用或故障，而查询仍然成功——这就是 M2 的兜底承诺。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// 结构化过滤基线（kind / tag / min_authority / text）。
    ///
    /// **唯一必需阶段。** 没有它，查询没有可返回的东西。
    Structured,
    /// 全文 BM25 召回（M2 后续：tantivy 接入前恒缺席）。
    FullText,
    /// 向量语义召回（M3 引入）。
    Vector,
    /// LLM 判定消歧（M3/M4 引入，且**不是默认路径**）。
    Llm,
}

impl Stage {
    /// 全部阶段，按链路顺序。用于健康报告完整列举——
    /// 一个阶段没被提到，和一个阶段被明说「缺席」，对排查是两种体验。
    pub const ALL: [Stage; 4] = [Stage::Structured, Stage::FullText, Stage::Vector, Stage::Llm];

    /// 该阶段是否为可选层。
    pub fn is_optional(self) -> bool {
        self != Stage::Structured
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Structured => "structured",
            Stage::FullText => "full_text",
            Stage::Vector => "vector",
            Stage::Llm => "llm",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 某一阶段在本次查询中的可用状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageState {
    /// 本版本压根没有这个阶段（例如 M2 尚未接向量库）。
    Absent,
    /// 有这个阶段，但被显式关闭。
    Disabled,
    /// 接了也开了，但本次调用失败。**只有这一种需要告警。**
    Failed(String),
    /// 就绪。
    Ready,
}

impl StageState {
    /// 是否可以参与本次查询。
    pub fn is_usable(&self) -> bool {
        matches!(self, StageState::Ready)
    }

    /// 是否为**故障**（与「缺席」「禁用」区分开，见模块文档）。
    pub fn is_failed(&self) -> bool {
        matches!(self, StageState::Failed(_))
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            StageState::Absent => "absent",
            StageState::Disabled => "disabled",
            StageState::Failed(_) => "failed",
            StageState::Ready => "ready",
        }
    }
}

impl fmt::Display for StageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StageState::Failed(e) => write!(f, "failed({e})"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// 整条链路的能力快照。
///
/// 由装配阶段（谁在运行检索层、向量库/LLM 有没有接）**提前**声明，
/// 而不是靠查询时去猜。这正是「缺席 / 禁用 / 故障」三态的来源：
/// 装配时就知道有没有，查询时才可能失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSnapshot {
    states: Vec<(Stage, StageState)>,
}

impl Default for HealthSnapshot {
    fn default() -> Self {
        Self::structured_only()
    }
}

impl HealthSnapshot {
    /// 构造一份快照。重复传入的阶段，后写的覆盖先写的。
    pub fn new(states: impl IntoIterator<Item = (Stage, StageState)>) -> Self {
        let mut v: Vec<(Stage, StageState)> = Vec::new();
        for (s, st) in states {
            match v.iter_mut().find(|(x, _)| *x == s) {
                Some(slot) => slot.1 = st,
                None => v.push((s, st)),
            }
        }
        // 恒定按 Stage::ALL 顺序输出，便于 UI 与日志稳定对比。
        v.sort_by_key(|(s, _)| *s);
        Self { states: v }
    }

    /// 全部阶段都缺席：只有结构化基线，且它必须就绪。
    pub fn structured_only() -> Self {
        Self::new([(Stage::Structured, StageState::Ready)])
    }

    /// 基线就绪、向量故障、LLM 缺席——M2/M3 的典型过渡形态。
    ///
    /// 不是测试专用构造：装配阶段发现向量服务连不上、LLM 没配置时，
    /// 产出的正是这份快照。
    pub fn degraded_baseline_ready() -> Self {
        Self::new([
            (Stage::Structured, StageState::Ready),
            (Stage::FullText, StageState::Absent),
            (Stage::Vector, StageState::Failed("向量服务不可达".into())),
            (Stage::Llm, StageState::Absent),
        ])
    }

    /// 查询某个阶段的状态。未声明的阶段按**缺席**处理——
    /// 没被显式声明成可用，就不得被当成可用。
    pub fn state(&self, stage: Stage) -> &StageState {
        self.states
            .iter()
            .find(|(s, _)| *s == stage)
            .map(|(_, st)| st)
            .unwrap_or(&StageState::Absent)
    }

    /// 结构化基线是否可用。**查询能否成功，全看这一位。**
    pub fn baseline_ready(&self) -> bool {
        self.state(Stage::Structured).is_usable()
    }

    /// 当前实际可用的阶段列表（按链路顺序）。
    pub fn usable_stages(&self) -> Vec<Stage> {
        Stage::ALL
            .into_iter()
            .filter(|s| self.state(*s).is_usable())
            .collect()
    }

    /// 处于故障态的阶段（需要告警的那些）。
    pub fn failed_stages(&self) -> Vec<Stage> {
        Stage::ALL
            .into_iter()
            .filter(|s| self.state(*s).is_failed())
            .collect()
    }

    /// 是否处于降级状态：基线就绪，但有可选阶段不可用。
    pub fn is_degraded(&self) -> bool {
        self.baseline_ready() && Stage::ALL.iter().any(|s| !self.state(*s).is_usable())
    }

    /// 逐阶段渲染成一行人可读的摘要，供日志与自检输出。
    pub fn summary(&self) -> String {
        Stage::ALL
            .iter()
            .map(|s| format!("{}={}", s, self.state(*s)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// 本次查询的降级说明。
///
/// **每次查询都必带这份报告**——即使一切正常。理由和 M3 的完成标志一致：
/// 「人为断开向量服务，查询仍返回结果**且降级日志明确**」。
/// 「明确」意味着上层永远能回答「这次为什么只有这些结果」，
/// 而不必去猜或翻日志。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DegradationReport {
    /// 本次查询**实际交付了结果**的阶段（按链路顺序）。
    ///
    /// 基线没交付时**不得**包含 [`Stage::Structured`]——那等于对外宣称
    /// 「结构化基线跑过了」，而实际上一行都没产出。
    pub used: Vec<Stage>,
    /// 本次查询**没能**用上的阶段，以及原因。
    ///
    /// 通常是可选阶段；但**基线自己故障时它也在里面**（携带具体原因）。
    /// 这样「查不到东西」就有了明确归因，而不是一个需要猜的空列表。
    pub skipped: Vec<(Stage, StageState)>,
}

impl DegradationReport {
    /// 全部阶段都就绪、全部都用上了。
    pub fn full_stack(used: Vec<Stage>) -> Self {
        Self {
            used,
            skipped: Vec::new(),
        }
    }

    /// 本次查询里**基线**（结构化阶段）实际落到的状态。
    ///
    /// 在 `used` 里 → [`StageState::Ready`]；在 `skipped` 里 → 那个状态。
    /// 两处都没提到 → 按**缺席**处理（fail-closed）：没被声明成交付过，
    /// 就不得被当成交付了。这与 [`HealthSnapshot::state`] 对未声明阶段的
    /// 默认处理是同一条纪律。
    pub fn baseline_state(&self) -> StageState {
        if self.skipped.iter().any(|(s, _)| *s == Stage::Structured) {
            return self
                .skipped
                .iter()
                .find(|(s, _)| *s == Stage::Structured)
                .map(|(_, st)| st.clone())
                .unwrap_or(StageState::Absent);
        }
        if self.used.contains(&Stage::Structured) {
            StageState::Ready
        } else {
            StageState::Absent
        }
    }

    /// 基线是否**故障**——上层唯一必须告警的那一种「什么都没返回」。
    ///
    /// 这是「真的没有匹配」（`false`）与「基线根本没跑成」（`true`）
    /// 之间唯一的分界。两者 `hits` 都是空的，只有本方法能分开它们。
    pub fn baseline_failed(&self) -> bool {
        self.baseline_state().is_failed()
    }

    /// 是否发生过降级（有任何阶段没参与）。
    ///
    /// 注意：基线故障时它同样为真——这正是我们希望上层注意到的场合。
    pub fn is_degraded(&self) -> bool {
        !self.skipped.is_empty()
    }

    /// 是否至少**跳过一个**指定阶段（可选层，或基线自身）。
    ///
    /// 这是「向量库和 LLM 全部不可用时仍能返回结果」的可测形式：
    /// 断言 `skipped_stage(Stage::Vector)` 与 `Stage::Llm` 为真，
    /// 同时结果集仍然正确。基线故障时 `skipped_stage(Stage::Structured)`
    /// 为真、`skipped_stage(Stage::Vector)` 仍互不干扰。
    pub fn skipped_stage(&self, stage: Stage) -> bool {
        self.skipped.iter().any(|(s, _)| *s == stage)
    }

    pub fn summary(&self) -> String {
        // `used` 为空只可能是基线故障（可选层不贡献结果不影响 used）。
        // 显式写成「（无）」而不是留一个空的 `used=`，否则读起来像格式化坏了。
        let used = if self.used.is_empty() {
            "（无）".to_string()
        } else {
            self.used
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join("+")
        };
        let skipped = self
            .skipped
            .iter()
            .map(|(s, st)| format!("{s}({st})"))
            .collect::<Vec<_>>()
            .join(",");
        if skipped.is_empty() {
            format!("used={used} 无降级")
        } else {
            format!("used={used} skipped={skipped}")
        }
    }
}

