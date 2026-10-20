//! 检索流水线 —— 把「结构化基线 + 可选层」组装成一次查询。
//!
//! # 这就是 M2 的「永远不会失败」落地处
//!
//! 查询能否成功，**只看结构化基线**。可选层（全文 / 向量 / LLM）
//! 的状态只影响 `DegradationReport`，不影响结果集是否存在。
//! 这不是一句口号，而是由 `RetrievalEngine::search` 的控制流保证的：
//! 可选层跑在基线**之后**的增强段里，任何一路出错都只是被记录下来。
//!
//! # 但「不失败」绝不等于「不撒谎」
//!
//! 「永远返回结果」的前提是**结果是真的**。若存储后端失败（SQLite 被锁住、
//! 文件损坏、路径不存在）却被吞成空列表，流水线会报出
//! `used=[structured]` ——对外宣称结构化基线跑通了，实际上一行都没产出。
//! 这比直接报错更糟：用户会把「库坏了」读成「你的过滤条件没匹配到东西」。
//!
//! 因此本模块把基线的**真实结局**（由 `AssetStore::try_query` 判定）
//! 如实写进 `DegradationReport`：`Ready` 才进 `used`，失败则进 `skipped`
//! 且带原因。查询照样返回（不冒泡），但报告不再替失败背书。

use crate::fulltext::FullTextIndex;
use crate::health::{DegradationReport, HealthSnapshot, Stage, StageState};
use crate::rerank::{rerank, RankInput, RankWeights};
use crate::store::{AssetFilter, AssetRow, AssetStore};
use crate::vector::VectorIndex;
use std::collections::HashSet;
use std::sync::Arc;

/// 召回深度相对最终 limit 的倍数。
///
/// BM25 只负责「捞回来」，精排要在这批候选里挑。捞得太少，精排没得挑；
/// 捞得太多，`try_query_in` 要回库取的行数就上去了，直接打在 P95 上。
/// 4 倍是折中，且有 [MAX_RECALL] 兜底。
const RECALL_FACTOR: usize = 4;
/// 召回条数硬上限。与 M2「P95 < 50ms」直接相关：候选行要逐条回库取，
/// 上限就是「最多回库取多少行」的天花板。
const MAX_RECALL: usize = 2000;

/// 一次查询的请求。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    /// 结构化过滤条件（走基线）。
    pub filter: AssetFilter,
    /// 自由文本。基线用它做包含匹配；未来全文层用它做 BM25。
    pub text: Option<String>,
    /// 最多返回多少行。
    pub limit: u32,
}

impl SearchQuery {
    pub fn new(filter: AssetFilter, limit: u32) -> Self {
        Self {
            filter,
            text: None,
            limit,
        }
    }
}

/// 一次查询的结果。
///
/// **`hits` 永远非空语义**：只要基线就绪，它就是一份真实结果集，
///哪怕只有一行、哪怕所有可选层都没参与。基线**未就绪**时它是空的，
///而这一点必须由 `degradation` 如实说明（见 [`DegradationReport::baseline_failed`]）——
/// 空 `hits` 本身没有信息量，「为什么空」才有。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub hits: Vec<AssetRow>,
    /// 本次查询**没能**用上的阶段及原因（降级日志）。基线故障时基线自己也在其中。
    pub degradation: DegradationReport,
}

impl SearchResult {
    pub fn len(&self) -> usize {
        self.hits.len()
    }
    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }
}

/// 检索引擎：持有存储与一份能力快照。
///
/// **泛型参数 `S` 只能是 `ac-retrieval` 里能看见的类型**——
/// 构造它需要 `&S`，所以调用方（ac-registry / 桌面端）注入自己那一份。
pub struct RetrievalEngine<S: AssetStore> {
    store: S,
    health: HealthSnapshot,
    /// 全文索引句柄。**可选**：没有它时 `Stage::FullText` 的状态
    /// 一律取自 `health` 快照（默认 `Absent`），行为与 M2 第一刀完全一致。
    ///
    /// 挂了之后，`Stage::FullText` 的**真实**状态取自它自己
    /// （`FullTextIndex::state()`）而不是快照：快照是装配时的声明，
    /// 「声明就绪、实际已坏」正是本项目要防的那类不一致。
    fulltext: Option<Arc<FullTextIndex>>,
    /// 向量召回句柄。**可选**，且与全文层**完全同级**：
    /// 它挂没挂、活没活，只影响 `Stage::Vector` 这一格，
    /// 结构化基线与 BM25 一律不受牵连。
    ///
    /// 同样以句柄自己的真实状态为准，不采信快照。
    vector: Option<Arc<VectorIndex>>,
}

impl<S: AssetStore> RetrievalEngine<S> {
    /// 用一份存储和能力快照装配引擎。
    ///
    /// 快照描述**可选层**的装配状态。基线这一次到底交付没有，以查询时的
    /// **实测**为准（见 `query_baseline`）：快照说就绪、实际查询失败时，
    /// 以实际为准，并如实记成 `Failed`。
    pub fn new(store: S, health: HealthSnapshot) -> Self {
        Self {
            store,
            health,
            fulltext: None,
            vector: None,
        }
    }

    /// 挂上全文索引。**纯增量**：不传这个方法的引擎行为与 M2 第一刀完全相同。
    pub fn with_fulltext(mut self, index: Arc<FullTextIndex>) -> Self {
        self.fulltext = Some(index);
        self
    }

    /// 挂上的全文索引句柄（未挂载则为 `None`）。
    pub fn fulltext(&self) -> Option<&Arc<FullTextIndex>> {
        self.fulltext.as_ref()
    }

    /// 原地挂上全文索引。
    ///
    /// 与 [Self::with_fulltext] 并存是因为外层的 `RetrievalSearcher` 只能拿到
    /// `&mut engine`（它持有 engine 而不是拥有装配过程），而消费式 builder
    /// 在那种位置用不了。两者语义完全一样。
    pub fn fulltext_mut(&mut self) -> &mut Option<Arc<FullTextIndex>> {
        &mut self.fulltext
    }

    /// 挂上向量召回层。**纯增量**：不传这个方法的引擎行为与之前逐条相同。
    ///
    /// 与 [Self::with_fulltext] 完全对称——这是刻意的：两层必须能被
    /// 「分别挂、都不挂、只挂一个」四种方式组合，而每一种组合下
    /// 结构化基线都必须照常交付。
    pub fn with_vector(mut self, index: Arc<VectorIndex>) -> Self {
        self.vector = Some(index);
        self
    }

    /// 挂上的向量层句柄（未挂载则为 `None`）。
    pub fn vector(&self) -> Option<&Arc<VectorIndex>> {
        self.vector.as_ref()
    }

    /// 原地挂上向量层（供外层的 `RetrievalSearcher` 用，理由同
    /// [Self::fulltext_mut]）。
    pub fn vector_mut(&mut self) -> &mut Option<Arc<VectorIndex>> {
        &mut self.vector
    }

    /// 装配：基线就绪、其余全缺席（M2 的实际形态）。
    pub fn structured_only(store: S) -> Self {
        Self::new(store, HealthSnapshot::structured_only())
    }

    pub fn health(&self) -> &HealthSnapshot {
        &self.health
    }

    /// 执行一次查询。
    ///
    /// # 为什么这个函数永远不会 Err
    ///
    /// 返回类型里**没有 Result**。可选层失败时只是不贡献结果，绝不让整个
    /// 查询失败；基线后端失败时也**不冒泡**，但会如实记进 `degradation`
    ///（`Stage::Structured` = `Failed`）。「不冒泡」与「不说谎」是两件事，
    /// 本函数两条都守。
    ///
    /// 将来接 tantivy / 向量 / LLM 时，可选层跑在 `hits` 求出**之后**，
    /// 它们的失败或缺席写进 `degradation`，但 `hits` 不受影响。
    ///
    /// # 「结果非空」这句话的前提
    ///
    /// `SearchResult::hits` 非空**只保证这次查询不抛错**，不保证基线交付了。
    /// 基线故障时 `hits` 会是空的，且 `degradation` 会说清原因——
    /// 想区分「真的没有匹配」与「基线没跑成」，请读 `baseline_failed()`。
    pub fn search(&self, q: &SearchQuery) -> SearchResult {
        // ① 基线：结构化过滤。查询能否成功只看这一步。
        //    text 与 filter.text 合并：自由文本同样走基线的包含匹配，
        //    保证「没有任何可选层」时文本检索依然可用。
        let mut filter = q.filter.clone();
        if filter.text.is_none() {
            filter.text = q.text.clone().filter(|t| !t.trim().is_empty());
        }
        let limit = q.limit.clamp(1, 1000);
        let (hits, baseline) = self.query_baseline(&filter, limit);

        // ② 可选层增强。当前一个都没接，全部记为缺席/故障。
        //    控制流上它们与基线**解耦**：即使某个阶段 Ready 且将来返回 Err，
        //    也要被就地降级（见 `augment` 的占位实现），不得冒泡成查询失败。
        let (hits, degradation) = self.augment(hits, q, &baseline);

        SearchResult { hits, degradation }
    }

    /// 跑结构化基线，并如实报告它到底交付了没有。
    ///
    /// # 为什么这里不能用 `self.store.query(..)` 一把梭
    ///
    /// `AssetStore::query` 的签名里没有错误通道，后端失败只能退化成空列表。
    /// 流水线若直接用它，库被锁住 / 损坏 / 缺失时就会得到「0 条命中 +
    /// 基线成功」——和「库里确实没有匹配项」**完全一样**。这是本项目
    /// 最不能犯的错：核心承诺是「永远返回结果」，而静默空结果会被读成
    /// 「你的过滤条件没匹配到东西」，用户无从分辨真假。
    ///
    /// 所以这里走 [`AssetStore::try_query`]：失败被接住（**不冒泡**），
    /// 但绝不消失（**记成基线 Failed**，见 `augment`）。
    ///
    /// # `is_healthy()` 在哪一步被问
    ///
    /// **只在失败分支问**，这是有意为之，理由有两条：
    ///
    /// 1. 它是**分类信息**：区分「存储自认已不可用」与「查询单独失败」，
    ///    两者的处置不同（前者要熔断，后者要查 SQL）。
    /// 2. happy path 上问它**没有任何信息增益**，却要多付一次全表查询
    ///    （`Registry::is_healthy` 走的是 `COUNT(*)`），与 M2「P95 < 50ms」
    ///    的目标直接冲突；而 `try_query` 返回 `Ok` 时基线就是交付了，
    ///    这比任何预检查都更强。
    fn query_baseline(&self, filter: &AssetFilter, limit: u32) -> (Vec<AssetRow>, StageState) {
        match self.store.try_query(filter, limit) {
            Ok(rows) => (rows, StageState::Ready),
            Err(e) => {
                let reason = if self.store.is_healthy() {
                    "结构化基线查询失败"
                } else {
                    "结构化存储不可用"
                };
                // 结果照样返回（此刻还没有可选层能补，先给空），
                // 但报告里基线是 Failed —— 一次都没交付就说一次没交付。
                (Vec::new(), StageState::Failed(format!("{reason}：{e}")))
            }
        }
    }

    /// 可选层增强段。
    ///
    /// `hits` 是结构化基线已经交付的结果，**本函数的第一条纪律是：它永远是
    /// 返回值的下界**。任何可选层出任何问题，结果集只可能变长（多召回几条），
    /// 绝不会变短、更不会变空。这不是靠自觉，是下面每一支都写成了
    /// 「失败 ⇒ 原样返回 hits」。
    ///
    /// `baseline` 是基线这一次的**真实结局**（由 `query_baseline` 判定，
    /// 不是从快照里抄的——快照说就绪、实际查询失败时，以实际为准）。
    fn augment(
        &self,
        hits: Vec<AssetRow>,
        q: &SearchQuery,
        baseline: &StageState,
    ) -> (Vec<AssetRow>, DegradationReport) {
        let mut used: Vec<Stage> = Vec::new();
        let mut skipped: Vec<(Stage, StageState)> = Vec::new();
        if baseline.is_usable() {
            used.push(Stage::Structured);
        } else {
            // 基线没交付 → 如实记为跳过（携带原因），**绝不写进 used**。
            // 写进去就等于对外宣称「结构化基线跑过了」，而实际上一行都没产出。
            skipped.push((Stage::Structured, baseline.clone()));
        }

        // ── 全文层：BM25 召回 ──────────────────────────────────
        let (hits, ft_state) = self.run_fulltext(hits, q);
        if ft_state.is_usable() {
            used.push(Stage::FullText);
        } else {
            skipped.push((Stage::FullText, ft_state));
        }

        // ── 向量层：与全文层**完全同级** ───────────────────────────
        // 这一段的形状与上面逐字对称，不是巧合：两层都必须满足
        // 「出任何问题 ⇒ 原样返回 hits + 如实记 skipped」。
        // 任何不对称都会变成一类只在某一层出现的降级漏洞。
        let (hits, vec_state) = self.run_vector(hits, q);
        if vec_state.is_usable() {
            used.push(Stage::Vector);
        } else {
            skipped.push((Stage::Vector, vec_state));
        }

        // ── LLM：M3 未接。规则与 M2 第一刀一字未改 ──────────────
        {
            let state = self.health.state(Stage::Llm).clone();
            if state.is_usable() {
                // 快照说就绪但增强尚未实现 → 如实记成缺席，
                // 免得报告说谎。
                skipped.push((Stage::Llm, StageState::Absent));
            } else {
                skipped.push((Stage::Llm, state));
            }
        }
        (hits, DegradationReport { used, skipped })
    }

    /// 跑全文召回 → 结构化过滤 → 精排，返回 (合并后结果, 全文层的真实状态)。
    ///
    /// # 这段代码为什么写得像一串 catch
    ///
    /// M2 的承诺是「**索引没了检索也还在**」。而这一段能被弄坏的方式恰恰有四种
    /// （缺索引 / 索引过期 / 索引构建失败 / 索引损坏），每一种都会在下面
    /// 某个分支里变成 `Err`。因此这里只有一条规则：
    /// **任何一支 `Err`，返回的都是原封不动的 `hits`**，
    /// 外加一个带原因的 `StageState::Failed`。
    /// 漏写一个 catch 的后果不是「这一层坏了」，而是「检索整体坏了」——
    /// 正是本切片最不能犯的错。
    fn run_fulltext(
        &self,
        hits: Vec<AssetRow>,
        q: &SearchQuery,
    ) -> (Vec<AssetRow>, StageState) {
        // 没挂索引 → 阶段压根没接。如实 Absent（不带原因，也不需要原因：
        // 「这一版没有全文层」是个自解释的事实）。
        let Some(index) = self.fulltext.as_ref() else {
            return (hits, self.health.state(Stage::FullText).clone());
        };
        // 挂了就以索引自己的真实状态为准，覆盖快照。快照是装配时的声明，
        // 「声明就绪、实际已坏」是要防的那类不一致。
        let state = index.state().clone();
        if !state.is_usable() {
            return (hits, state);
        }
        // 本次查询没有自由文本 → 全文层确实没有参与。
        // 报 `Absent` 而不是 `Ready`：没参与的阶段不能进 `used`，
        // 而 `Absent` 正是「本次没参与」的既有语义。
        let text = q
            .text
            .as_deref()
            .or(q.filter.text.as_deref())
            .unwrap_or("")
            .trim();
        if text.is_empty() {
            return (hits, StageState::Absent);
        }
        match self.recall_and_rerank(index, q, text, &hits) {
            Ok(merged) => (merged, StageState::Ready),
            // 失败一律降级：结果集回到基线原样，报告里带原因。
            Err(e) => (hits, StageState::Failed(format!("全文召回失败：{e}"))),
        }
    }

    /// BM25 召回 → 结构化过滤 → 精排 → 与基线结果合并。
    fn recall_and_rerank(
        &self,
        index: &FullTextIndex,
        q: &SearchQuery,
        text: &str,
        baseline: &[AssetRow],
    ) -> Result<Vec<AssetRow>, String> {
        let limit = q.limit.clamp(1, 1000) as usize;
        let recall_n = (limit * RECALL_FACTOR).clamp(1, MAX_RECALL);

        // ① BM25 召回。只出 (id, 分数)，不出展示用的行。
        let scored = index.search(text, recall_n)?;
        if scored.is_empty() {
            // 召回零命中不是错误：全文层确实跑了，只是这个查询没东西。
            // 返回基线原样，状态照旧是 Ready —— 全文层参与了，只是没贡献行。
            return Ok(baseline.to_vec());
        }
        let score_by: std::collections::HashMap<&str, f32> =
            scored.iter().map(|s| (s.id.as_str(), s.score)).collect();
        let ids: Vec<String> = scored.iter().map(|s| s.id.clone()).collect();

        // ② 结构化过滤。行回到 store 取（索引只是派生物），
        //    并且 filter.text 置空 —— 文本匹配是召回层的事。
        let mut struct_filter = q.filter.clone();
        struct_filter.text = None;
        let rows = self
            .store
            .try_query_in(&struct_filter, &ids, recall_n as u32)?;

        // ③ 精排。BM25 分数来自召回，字段加权来自结构化维度。
        let items: Vec<RankInput> = rows
            .into_iter()
            .map(|row| {
                let bm25 = score_by.get(row.id.as_str()).copied().unwrap_or(0.0);
                RankInput { row, bm25 }
            })
            .collect();
        let ranked = rerank(text, items, RankWeights::default());

        // ④ 合并：精排结果在前，基线里未被全文覆盖的行补在后面（按 id 去重）。
        //    补而不是替换，是因为两层对「文本匹配」的理解本就不一样
        //    （BM25 是词级统计，基线是整串包含）—— 两边各有召回，
        //    取并集才是 M2「永远不会失败」的应有之义。
        // 去重表存**拥有所有权**的 id：借用的 &str 会和后面的 move 打架。
        let mut seen: HashSet<String> = HashSet::with_capacity(ranked.len() + baseline.len());
        let mut merged: Vec<AssetRow> = Vec::with_capacity(limit);
        for r in ranked {
            if merged.len() >= limit {
                break;
            }
            if seen.insert(r.row.id.clone()) {
                merged.push(r.row);
            }
        }
        for row in baseline {
            if merged.len() >= limit {
                break;
            }
            if seen.insert(row.id.clone()) {
                merged.push(row.clone());
            }
        }
        Ok(merged)
    }

    /// 跑向量召回 → 结构化过滤 → 与既有结果合并，返回 (合并后结果, 向量层真实状态)。
    ///
    /// # 这个函数的结构与 run_fulltext 逐条对应
    ///
    /// 顺序、分支数、失败回落方式全部一致。共五条出路：
    ///
    /// | 情形 | 结果 | 状态 |
    /// |---|---|---|
    /// | 没挂向量层 | 原样 hits | 快照（默认 Absent） |
    /// | 挂了但不可用 | 原样 hits | 句柄的真实状态（带原因） |
    /// | 查询没有文本 | 原样 hits | Absent（本次没参与） |
    /// | 召回成功 | 合并后结果 | Ready |
    /// | 召回失败 | **原样 hits** | Failed(原因) |
    ///
    /// 最后一行是**本函数的全部意义**。漏写它，向量服务一挂，
    /// 检索就整体没了——而那正是 M3 完成标志要防的事情。
    fn run_vector(&self, hits: Vec<AssetRow>, q: &SearchQuery) -> (Vec<AssetRow>, StageState) {
        // 没挂向量层 → 阶段压根没接。如实 Absent。
        let Some(index) = self.vector.as_ref() else {
            return (hits, self.health.state(Stage::Vector).clone());
        };
        // 挂了就以句柄自己的真实状态为准，覆盖快照。
        // 用 available_state() 而不是 probe_state()：查询路径允许复用
        // 刚确认过的成功（否则一次查询要探两遍）；而 probe_state() 是
        // 健康检查用的强制真探，保证「服务停了健康检查立刻变红」。
        let state = index.available_state();
        if !state.is_usable() {
            return (hits, state);
        }
        let text = q
            .text
            .as_deref()
            .or(q.filter.text.as_deref())
            .unwrap_or("")
            .trim();
        if text.is_empty() {
            // 没有自由文本 → 向量层确实没有参与。报 Absent 而不是 Ready：
            // 没参与的阶段不能进 used。
            return (hits, StageState::Absent);
        }
        match self.recall_by_vector(index, q, text, &hits) {
            Ok(merged) => (merged, StageState::Ready),
            // 失败一律降级：结果集回到基线（加 BM25）原样，报告里带原因。
            Err(e) => (hits, StageState::Failed(format!("向量召回失败：{e}"))),
        }
    }

    /// 向量召回 → 结构化过滤 → 与既有结果合并。
    ///
    /// 与 BM25 那一条链路的三点**刻意差异**：
    ///
    /// 1. **先确保表可用。** 懒建就落在这里：第一次带文本的查询才建表。
    ///    建不起来就是 Err，于是发出这一条查询的人会看到
    ///    vector: failed(原因)，而不是一个静默变慢的查询。
    /// 2. **不打分重排。** 余弦分与 BM25 分不同量纲，硬混会让排序失去意义。
    ///    所以向量层只负责**多召回一批候选**，最终顺序仍由既有的精排
    ///    与基线顺序决定。M3 的定位是「找得更全」，不是「改变排序」。
    /// 3. **不截断到 limit。** 这里返回的是候选，由上层合并后统一截断。
    fn recall_by_vector(
        &self,
        index: &std::sync::Arc<crate::vector::VectorIndex>,
        q: &SearchQuery,
        text: &str,
        baseline: &[AssetRow],
    ) -> Result<Vec<AssetRow>, String> {
        // ① 懒建 / 过期重建 —— **不阻塞本次查询**。
        //
        //    没建好时它立刻返回 false 并留在后台继续建。这是「宁可降级
        //    不要卡住查询」在向量层上的具体落点：一次用户查询绝不为
        //    一次全量嵌入等上十几秒，更不该在 /embed 卡住时等满建表预算。
        if !index.ensure_nonblocking(&self.store) {
            if index.is_building() {
                return Err("向量表建表中（后台进行，本次查询已降级）".to_string());
            }
            let st = index.state();
            return Err(match st {
                StageState::Failed(e) => e,
                other => format!("向量表不可用（{other}）"),
            });
        }

        let limit = q.limit.clamp(1, 1000) as usize;
        let recall_n = (limit * RECALL_FACTOR).clamp(1, MAX_RECALL);

        // ② 余弦召回，只出 (id, 分数)。
        let scored = index.recall(text, recall_n)?;
        if scored.is_empty() {
            // 召回零命中不是错误：向量层确实跑了，只是这个查询没东西。
            return Ok(baseline.to_vec());
        }
        let ids: Vec<String> = scored.iter().map(|s| s.id.clone()).collect();

        // ③ 结构化过滤。行回到 store 取（向量表只是派生物），
        //    filter.text 置空 —— 文本匹配是召回层的事。
        let mut struct_filter = q.filter.clone();
        struct_filter.text = None;
        let rows = self
            .store
            .try_query_in(&struct_filter, &ids, recall_n as u32)?;

        // ④ 合并：**既有结果在前**（它们是结构化基线 / BM25 的成果，
        //    保持其相对顺序），向量召回的新候选补在后面，按 id 去重。
        //
        //    顺序为什么是这个方向：向量召回是「可能相关」，而基线是
        //    「确定命中过滤条件」、BM25 是「词面命中」。把不确定的排在
        //    确定的**前面**会改变既有排序质量；补在后面则只增不减。
        //    这也是「可选层只可能让结果变长」在排序上的直接体现。
        let mut seen: HashSet<String> = HashSet::with_capacity(rows.len() + baseline.len());
        let mut merged: Vec<AssetRow> = Vec::with_capacity(baseline.len() + rows.len());
        for row in baseline {
            if seen.insert(row.id.clone()) {
                merged.push(row.clone());
            }
        }
        for row in rows {
            if seen.insert(row.id.clone()) {
                merged.push(row);
            }
        }
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{DegradationReport, Stage, StageState};
    use crate::memstore::InMemoryStore;
    use crate::store::AssetRow;
    use ac_core::Authority;

    // ── 向量层降级护栏：与 assert_degraded_to_structured 同级 ──────
    //
    // 全文层那四条护栏（不存在/过期/构建失败/损坏）守的是
    // 「索引没了检索也还在」。向量层有**自己的一组**破法，
    // 而且更凶险：它依赖一个外部进程，那个进程随时可能不在。
    // 这里把它的破法做成同一组断言，跑多遍。
    //
    // 与全文层那组的关系：不是复制，是**对称**。两组共用同一段
    // 判定文字，任何一层被写坏都会红。

    use crate::vector::testing::{dead_endpoint, FakeEmbedder, Mode};

    /// 一条查询，同时断言「结果没丢」与「状态没说谎」——**针对向量层**。
    ///
    /// 这是任务里点名要的那条护栏：full_text 有
    /// `assert_degraded_to_structured`，向量层必须有同等级的保护。
    fn assert_vector_degraded_to_structured(
        reason_tag: &str,
        report: &DegradationReport,
        hits: &[AssetRow],
    ) {
        // ① 绝不能进 used —— 那是谎报成功。
        assert!(
            !report.used.contains(&Stage::Vector),
            "{reason_tag}: 向量层不可用时绝不能被写进 used —— 那是谎报成功。报告：{}",
            report.summary()
        );
        // ② 必须出现在 skipped 里，且**带可读原因**。
        let st = report
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::Vector)
            .map(|(_, st)| st.clone())
            .unwrap_or_else(|| {
                panic!("{reason_tag}: 降级报告里必须显式提到 Vector。报告：{}", report.summary())
            });
        match st {
            StageState::Failed(msg) => assert!(
                !msg.trim().is_empty(),
                "{reason_tag}: Failed 必须带原因，空消息等于没有归因"
            ),
            StageState::Absent | StageState::Disabled => {}
            StageState::Ready => panic!("{reason_tag}: 不可用时不得报 ready"),
        }
        // ③ 结构化基线必须照常交付，且真的产出了行。
        assert!(
            report.used.contains(&Stage::Structured),
            "{reason_tag}: 结构化基线必须照常交付，实际报告：{}",
            report.summary()
        );
        assert!(!report.baseline_failed(), "{reason_tag}: 基线不该被牵连");
        assert!(
            !hits.is_empty(),
            "{reason_tag}: 向量层坏了也必须返回结构化结果，实际 0 条"
        );
        // ④ 降级必须自我暴露。
        assert!(report.is_degraded(), "{reason_tag}: 必须被标记为降级");
    }

    fn query_engine(engine: RetrievalEngine<InMemoryStore>, q: &str) -> crate::SearchResult {
        engine.search(&SearchQuery {
            filter: AssetFilter::all(),
            text: Some(q.to_string()),
            limit: 10,
        })
    }

    /// 预热：反复查询直到向量层真的参与（后台建表完成），或超时。
    ///
    /// 把「等后台线程」这件事写成显式的、有上限的等待，
    /// 而不是靠 sleep 一个魔法数字——后者是这类测试变成 flaky 的典型原因。
    fn query_engine_warmup(
        engine: &RetrievalEngine<InMemoryStore>,
        q: &str,
    ) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let r = query_engine_ref(engine, q);
            if r.degradation.used.contains(&Stage::Vector) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    fn query_engine_ref(
        engine: &RetrievalEngine<InMemoryStore>,
        q: &str,
    ) -> crate::SearchResult {
        engine.search(&SearchQuery {
            filter: AssetFilter::all(),
            text: Some(q.to_string()),
            limit: 10,
        })
    }

    fn vector_demo_store() -> InMemoryStore {
        InMemoryStore::new(vec![
            asset("s::seq", "画时序图", "把消息流转画成时序图，参与者与消息顺序"),
            asset("s::flow", "画流程图", "流程图与分支判断，节点连线"),
            asset("s::db", "数据库备份", "备份与恢复脚本，定时任务与校验"),
            asset("s::net", "网络诊断", "排查端口连通性与路由表"),
        ])
    }

    fn asset(id: &str, name: &str, text: &str) -> AssetRow {
        let mut r = AssetRow::new(id, "skill", name, Authority::Read);
        r.text = format!("{name} {text}");
        r
    }

    /// 破法一：**服务根本不在**（连不上）。
    #[test]
    fn vector_mode_1_service_unreachable_degrades_to_structured() {
        let mut cfg = crate::vector::VectorConfig::default();
        cfg.endpoint = dead_endpoint();
        cfg.timeout = std::time::Duration::from_millis(300);
        cfg.build_timeout = std::time::Duration::from_millis(10_000);
        cfg.cooldown = std::time::Duration::from_millis(0);
        let idx = std::sync::Arc::new(crate::vector::VectorIndex::attach(cfg));
        let engine = RetrievalEngine::structured_only(vector_demo_store()).with_vector(idx);

        let r = query_engine(engine, "时序图");
        assert_vector_degraded_to_structured("服务不在", &r.degradation, &r.hits);
        // 原因必须指向地址，否则排查不知道它在连哪儿。
        let st = r
            .degradation
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::Vector)
            .unwrap();
        assert!(
            st.1.to_string().contains("127.0.0.1"),
            "原因必须带地址，实际：{}",
            st.1
        );
    }

    /// 破法二：**服务在，但模型没加载好**（端口通、HTTP 200）。
    ///
    /// 这是最容易糊弄过去的一格：只看「连得上」的实现会报 ready。
    #[test]
    fn vector_mode_2_service_loading_degrades_to_structured() {
        let svc = FakeEmbedder::start(Mode::Loading);
        let idx = std::sync::Arc::new(svc.index(2000));
        let engine = RetrievalEngine::structured_only(vector_demo_store()).with_vector(idx);

        let r = query_engine(engine, "时序图");
        assert_vector_degraded_to_structured("模型加载中", &r.degradation, &r.hits);
        let st = r
            .degradation
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::Vector)
            .unwrap();
        assert!(
            st.1.to_string().contains("loading"),
            "原因必须带上服务自报状态，实际：{}",
            st.1
        );
    }

    /// 破法三：**服务在、就绪，但建表失败**（嵌入接口 500）。
    #[test]
    fn vector_mode_3_index_build_failed_degrades_to_structured() {
        let svc = FakeEmbedder::start(Mode::ServerError);
        let idx = std::sync::Arc::new(svc.index(2000));
        let engine = RetrievalEngine::structured_only(vector_demo_store()).with_vector(idx);

        let r = query_engine(engine, "时序图");
        assert_vector_degraded_to_structured("建表失败", &r.degradation, &r.hits);
    }

    /// 破法四：**响应畸形**（条数不符）。建表必须整批拒绝。
    #[test]
    fn vector_mode_4_malformed_response_degrades_to_structured() {
        let svc = FakeEmbedder::start(Mode::WrongCount);
        let idx = std::sync::Arc::new(svc.index(2000));
        let engine = RetrievalEngine::structured_only(vector_demo_store()).with_vector(idx);

        let r = query_engine(engine, "时序图");
        assert_vector_degraded_to_structured("响应畸形", &r.degradation, &r.hits);
    }

    /// 破法五：**服务卡住不回包**。这条是「不许把查询卡住」的可测形式。
    #[test]
    fn vector_mode_5_timeout_degrades_to_structured_and_returns_fast() {
        let svc = FakeEmbedder::start(Mode::Hang);
        let idx = std::sync::Arc::new(svc.index(300));
        let engine = RetrievalEngine::structured_only(vector_demo_store()).with_vector(idx);

        let started = std::time::Instant::now();
        let r = query_engine(engine, "时序图");
        let elapsed = started.elapsed();
        assert_vector_degraded_to_structured("超时", &r.degradation, &r.hits);
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "超时预算 300ms，整条查询花了 {elapsed:?} —— 向量层把查询卡住了"
        );
        // 超时必须被**如实报成超时**，不能退化成一条含糊的「读取失败」：
        // 「服务卡住」与「服务没开」的处置完全不同。
        let st = r
            .degradation
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::Vector)
            .unwrap();
        assert!(
            st.1.to_string().contains("超时"),
            "原因必须指向超时，实际：{}",
            st.1
        );
    }

    /// 破法六：**句柄在但状态坏了**（锁中毒/内部故障的等价形态）。
    #[test]
    fn vector_mode_6_broken_handle_degrades_to_structured() {
        let idx = std::sync::Arc::new(crate::vector::VectorIndex::unavailable(
            "内部故障（模拟）",
        ));
        let engine = RetrievalEngine::structured_only(vector_demo_store()).with_vector(idx);
        let r = query_engine(engine, "时序图");
        assert_vector_degraded_to_structured("内部故障", &r.degradation, &r.hits);
    }

    /// **核心不变量**：向量层缺席/失败时结果集必须与「压根没有向量层」逐条相同。
    ///
    /// 逐条比对而不是只比条数——这排除了「故障悄悄改结果」的实现。
    #[test]
    fn vector_failure_leaves_the_baseline_hits_byte_identical() {
        let base = query_engine(
            RetrievalEngine::structured_only(vector_demo_store()),
            "时序图",
        );

        let mut cfg = crate::vector::VectorConfig::default();
        cfg.endpoint = dead_endpoint();
        cfg.timeout = std::time::Duration::from_millis(300);
        cfg.build_timeout = std::time::Duration::from_millis(10_000);
        cfg.cooldown = std::time::Duration::from_millis(0);
        let broken = query_engine(
            RetrievalEngine::structured_only(vector_demo_store())
                .with_vector(std::sync::Arc::new(crate::vector::VectorIndex::attach(cfg))),
            "时序图",
        );

        assert_eq!(
            base.hits, broken.hits,
            "向量层故障前后 hits 必须完全相同，实际 base={:?} broken={:?}",
            base.hits.iter().map(|h| &h.id).collect::<Vec<_>>(),
            broken.hits.iter().map(|h| &h.id).collect::<Vec<_>>()
        );
    }

    /// 服务就绪时，向量层必须**真的参与**：进 used，且能召回到基线抓不到的资产。
    ///
    /// 这是「服务在时向量召回真的生效」的可测形式。只断言
    /// used 含 Vector 是不够的——一个「挂在 used 里但什么都没干」的实现
    /// 照样能过。必须同时断言有**基线拿不到、而向量拿到**的行。
    #[test]
    fn ready_vector_layer_actually_participates_and_extends_recall() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let idx = std::sync::Arc::new(svc.index(2000));
        // 关键构造：查询词「时序」在 s::deep 的正文里**不相邻**，
        // 所以基线的整串包含匹配抓不到它；而分桶向量按字符计，
        // 「时序」与含「时」「序」的正文有真实重叠。
        let store = InMemoryStore::new(vec![
            asset("s::exact", "时序图", "把消息流转画成时序图"),
            asset("s::deep", "架构记录", "记录 时 与 序 的排布，以及其它过程"),
            asset("s::noise", "完全无关", "数据库备份与恢复脚本"),
        ]);
        // **先预热**：建表走后台线程，第一条查询按设计是降级的
        // （见 first_query_degrades_while_build_runs_in_background）。
        // 这条测试要断言的是「表建好之后向量真的参与」，所以先等它建完。
        let engine = RetrievalEngine::structured_only(store).with_vector(idx);
        let _warm = query_engine_warmup(&engine, "时序");
        let r = query_engine(engine, "时序");

        assert!(
            r.degradation.used.contains(&Stage::Vector),
            "服务就绪时向量层必须进 used，实际报告：{}",
            r.degradation.summary()
        );
        // 判别器：基线单独跑时拿不到 s::deep。
        let base = query_engine(
            RetrievalEngine::structured_only(InMemoryStore::new(vec![
                asset("s::exact", "时序图", "把消息流转画成时序图"),
                asset("s::deep", "架构记录", "记录 时 与 序 的排布，以及其它过程"),
                asset("s::noise", "完全无关", "数据库备份与恢复脚本"),
            ])),
            "时序",
        );
        let base_ids: Vec<&str> = base.hits.iter().map(|h| h.id.as_str()).collect();
        assert!(
            !base_ids.contains(&"s::deep"),
            "前置条件失败：基线本来就能拿到 s::deep，这条测试失去鉴别力（{base_ids:?}）"
        );
        let ids: Vec<&str> = r.hits.iter().map(|h| h.id.as_str()).collect();
        assert!(
            ids.contains(&"s::deep"),
            "向量层必须召回到基线抓不到的那条，实际 {ids:?}，报告：{}",
            r.degradation.summary()
        );
        // 基线结果必须还在，且排在新召回之前（可选层只增不减、不改既有排序）。
        assert_eq!(ids[0], "s::exact", "既有命中必须仍排第一，实际 {ids:?}");
    }

    /// 没有自由文本时，向量层不得进 used（它确实没参与）。
    #[test]
    fn vector_stage_is_not_credited_when_the_query_has_no_text() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let idx = std::sync::Arc::new(svc.index(2000));
        let engine = RetrievalEngine::structured_only(vector_demo_store()).with_vector(idx);
        let r = engine.search(&SearchQuery::new(AssetFilter::all(), 10));
        assert!(
            !r.degradation.used.contains(&Stage::Vector),
            "没有文本时向量层没参与，不得进 used：{}",
            r.degradation.summary()
        );
        assert!(r.degradation.skipped_stage(Stage::Vector));
        assert_eq!(r.len(), 4, "结果集照常");
    }

    /// **两层同时坏**：向量与全文一起挂，查询必须仍然返回结构化结果。
    ///
    /// 这是 M2 那句完成标志的原样复现，只不过现在两层都真的接上了。
    #[test]
    fn both_optional_layers_down_still_returns_structured_results() {
        let mut cfg = crate::vector::VectorConfig::default();
        cfg.endpoint = dead_endpoint();
        cfg.timeout = std::time::Duration::from_millis(300);
        cfg.build_timeout = std::time::Duration::from_millis(10_000);
        cfg.cooldown = std::time::Duration::from_millis(0);
        let vec = std::sync::Arc::new(crate::vector::VectorIndex::attach(cfg));

        // 全文层同样构造为不可用：只读挂载 + 空目录 = 索引不存在。
        let dir = scratch_dir("both-down");
        let store = vector_demo_store();
        let ft = std::sync::Arc::new(crate::fulltext::FullTextIndex::attach(
            &dir,
            &store,
            crate::fulltext::SyncPolicy::Manual,
        ));

        let engine = RetrievalEngine::structured_only(store)
            .with_fulltext(ft)
            .with_vector(vec);
        let r = query_engine(engine, "时序图");
        // 注意语义：带自由文本时基线的整串包含匹配只放行 1 条
        // （只有 s::seq 的正文里真的有「时序图」）。这**是正确行为**，
        // 不是丢结果——两层全挂时不该有任何一层额外补齐。
        assert_eq!(
            r.len(),
            1,
            "两层全挂时结果集必须恰好等于基线自己交付的那批，实际 {:?}",
            r.hits.iter().map(|h| &h.id).collect::<Vec<_>>()
        );
        assert!(r.degradation.used.contains(&Stage::Structured));
        assert!(!r.degradation.used.contains(&Stage::Vector));
        assert!(!r.degradation.used.contains(&Stage::FullText));
        assert!(r.degradation.skipped_stage(Stage::Vector));
        assert!(r.degradation.skipped_stage(Stage::FullText));
        assert!(!r.degradation.baseline_failed());
    }


    /// **首次查询必须立刻降级**，且如实说明「建表中」。
    ///
    /// 这是「宁可降级不要卡住查询」在端到端上的断言：即使是第一次查询
    /// （表还不存在），也绝不能让调用方等一次全量嵌入。
    #[test]
    fn first_query_degrades_immediately_while_the_table_is_being_built() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let idx = std::sync::Arc::new(svc.index(2000));
        let engine = RetrievalEngine::structured_only(vector_demo_store()).with_vector(idx);

        let started = std::time::Instant::now();
        let r = query_engine(engine, "时序图");
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "首查花了 {elapsed:?} —— 它等在建表上了。报告：{}",
            r.degradation.summary()
        );
        assert!(
            !r.degradation.used.contains(&Stage::Vector),
            "表还没建好就不该声称用了向量：{}",
            r.degradation.summary()
        );
        // 必须如实说清是「正在建」，而不是含糊的「不可用」。
        let st = r
            .degradation
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::Vector)
            .map(|(_, st)| st.clone())
            .expect("必须显式提到 Vector");
        assert!(st.is_failed(), "本次确实没跑成，应记 failed，实际 {st}");
        assert!(
            st.to_string().contains("建表中"),
            "原因必须指向「建表中」而不是别的，实际 {st}"
        );
        // 降级不等于丢结果。
        assert!(!r.hits.is_empty(), "必须照常返回结构化结果");
        assert!(r.degradation.used.contains(&Stage::Structured));
    }

    /// 测试用临时目录（锚在 target/ 下，与 fulltext.rs 同一处纪律）。
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::current_exe().expect("current_exe");
        p.pop();
        if p.file_name().map(|n| n == "deps").unwrap_or(false) {
            p.pop();
        }
        p.push("ac-vector-tests");
        p.push(tag);
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("创建测试目录");
        p
    }
}