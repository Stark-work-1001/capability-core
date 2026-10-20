//! `Searcher` trait —— M2 交付物，**检索侧实现**。
//!
//! # 依赖方向的一处如实说明
//!
//! 设计文档耦合点清单写的是：
//!
//!   mcp -> retrieval | `Searcher` trait | mcp 定义，retrieval 实现
//!
//! 但同一个表里 mcp -> retrieval 这条箭头意味着 **ac-mcp 依赖 ac-retrieval**。
//! 若让 ac-retrieval 去「实现一个定义在 ac-mcp 里的 trait」，就必须
//! 反向依赖 ac-mcp，与该箭头相反，并直接构成循环依赖（硬性约束 #4）。
//!
//! 因此这里采用**唯一无环的读法**：
//!
//! - ac-retrieval 定义 `Searcher` trait（自洽的检索侧接口），
//! - ac-retrieval 提供具体实现 `RetrievalSearcher`；
//! - 将来 ac-mcp 建好时，由它定义面向消费者的 trait，再委派给
//!   `RetrievalSearcher`（它本来就依赖 ac-retrieval，无环）。
//!
//! 这满足「实现检索侧」的要求，且不引入循环。

use crate::health::{HealthSnapshot, StageState};
use crate::pipeline::{RetrievalEngine, SearchQuery, SearchResult};
use crate::store::AssetStore;

/// 检索侧对外的统一检索接口。
///
/// 语义刻意极窄：**只返回结果，永不返回 `Result`**。
///
/// 这一条签名就是 M2 完成标志的代码形态——既然没有可供冒泡的错误通道，
/// 「向量库和 LLM 全部不可用时仍能返回结果」在类型上就是必然的，
/// 而不是一条靠约定维持的纪律。
pub trait Searcher {
    /// 执行一次检索。**不会失败**：可选阶段缺席/故障只体现在
    /// `SearchResult::degradation` 里。
    fn search(&self, q: &SearchQuery) -> SearchResult;

    /// 当前能力快照，供上层展示「这次能不能用向量/LLM」。
    fn health(&self) -> &HealthSnapshot;
}

/// `Searcher` 的检索侧具体实现。
///
/// 就是把 [`RetrievalEngine`] 原样暴露出来——它已经满足「永不失败」语义。
pub struct RetrievalSearcher<S: AssetStore> {
    engine: RetrievalEngine<S>,
}

impl<S: AssetStore> RetrievalSearcher<S> {
    /// 用存储与能力快照装配。
    pub fn new(store: S, health: HealthSnapshot) -> Self {
        Self {
            engine: RetrievalEngine::new(store, health),
        }
    }

    /// 基线就绪、其余全缺席——M2 的实际装配形态。
    pub fn structured_only(store: S) -> Self {
        Self::new(store, HealthSnapshot::structured_only())
    }

    /// 挂上全文索引（M2 第二刀）。**纯增量**：不调它就是 M2 第一刀的形态，
    /// 行为逐条不变。
    ///
    /// 索引句柄用 `Arc`：多个 `Searcher` 共用同一份倒排，而查询是 `&self`，
    /// 不需要可变访问——可变的那一面（`sync`）由装配/后台阶段单独走。
    pub fn with_fulltext(mut self, index: std::sync::Arc<crate::fulltext::FullTextIndex>) -> Self {
        *self.engine.fulltext_mut() = Some(index);
        self
    }

    /// 挂上的全文索引句柄（未挂载则为 `None`）。
    pub fn fulltext(&self) -> Option<&std::sync::Arc<crate::fulltext::FullTextIndex>> {
        self.engine.fulltext()
    }

    /// 挂上向量召回层（M3）。**纯增量**，与 [Self::with_fulltext] 完全对称。
    pub fn with_vector(mut self, index: std::sync::Arc<crate::vector::VectorIndex>) -> Self {
        *self.engine.vector_mut() = Some(index);
        self
    }

    /// 挂上的向量层句柄（未挂载则为 None）。
    pub fn vector(&self) -> Option<&std::sync::Arc<crate::vector::VectorIndex>> {
        self.engine.vector()
    }
}

impl<S: AssetStore> Searcher for RetrievalSearcher<S> {
    fn search(&self, q: &SearchQuery) -> SearchResult {
        self.engine.search(q)
    }
    fn health(&self) -> &HealthSnapshot {
        self.engine.health()
    }
}

/// 便捷构造：基线就绪、向量与 LLM 全部不可用的检索器。
///
/// 这是 M2 完成标志那句话的直接可执行形态：
/// 向量库和 LLM 全部不可用，检索照常工作。
pub fn no_vector_no_llm<S: AssetStore>(store: S) -> RetrievalSearcher<S> {
    RetrievalSearcher::new(store, HealthSnapshot::new([
        (crate::health::Stage::Structured, StageState::Ready),
        (crate::health::Stage::Vector, StageState::Failed("向量库不可用".into())),
        (crate::health::Stage::Llm, StageState::Failed("LLM 不可用".into())),
    ]))
}

