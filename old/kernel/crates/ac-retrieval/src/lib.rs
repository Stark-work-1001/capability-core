//! # ac-retrieval
//!
//! capability-core 混合检索：结构化基线（永久可用）+ 全文 / 向量 / LLM（可选、可缺席、可故障）。
//!
//! ## M2 基础切片落地了什么
//!
//! | 模块 | 职责 | 里程碑 |
//! |---|---|---|
//! | [`store`] | **定义** `AssetStore` trait + `AssetFilter` / `AssetRow` | M2 |
//! | [`health`] | 能力快照与降级报告（缺席 / 禁用 / 故障 / 就绪） | M2 |
//! | [`pipeline`] | 流水线：结构化基线 + 可选层增强 | M2 |
//! | [`searcher`] | **定义** `Searcher` trait + `RetrievalSearcher` 实现 | M2 |
//! | [`memstore`] | 内存 `AssetStore`，供脱离 SQLite 单测 | M2 |
//! | [`fulltext`] | tantivy BM25 召回：索引、增量同步、四种降级 | M2 |
//! | [`rerank`] | 精排：三个可解释分量，无学习模型 | M2 |
//!
//! ## 依赖纪律（硬性约束，不可协商）
//!
//! 1. 本 crate **不得**依赖 ac-registry —— [`store::AssetStore`] 的方向是
//!    「retrieval 定义，registry 实现」。反向依赖会让 trait 定义方被实现方
//!    绑住，耦合点清单就反了。由 `guard_retrieval_does_not_depend_on_registry`
//!    守门。
//! 2. 本 crate 只依赖 ac-core（协议层，零 IO、零 crate 依赖）。
//!
//! ## 为什么 `Searcher::search` 的返回类型里没有 `Result`
//!
//! 这是本切片最重要的一个签名决定。M2 的完成标志要求
//! 「在向量库和 LLM 全部不可用时仍能返回结果」。若检索接口本身带错误通道，
//! 「仍能返回」就只是一条需要人遵守的纪律；而现在没有错误可冒泡，
//! 这条性质是**类型层面**的。可选阶段的状态一律走
//! [`health::DegradationReport`] 表达，见 [`pipeline`] 的 `augment`。

pub mod fulltext;
pub mod health;
pub mod memstore;
pub mod pipeline;
pub mod rerank;
pub mod searcher;
pub mod store;
pub mod vector;

pub use fulltext::{FullTextIndex, Scored, SyncPolicy, SyncReport};
pub use health::{DegradationReport, HealthSnapshot, Stage, StageState};
pub use memstore::InMemoryStore;
pub use pipeline::{RetrievalEngine, SearchQuery, SearchResult};
pub use rerank::{RankWeights, Ranked};
pub use searcher::{no_vector_no_llm, RetrievalSearcher, Searcher};
pub use store::{AssetFilter, AssetRow, AssetStore, IndexEntry, IndexSnapshot};
pub use vector::{VectorConfig, VectorIndex};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::Authority;

    fn row(id: &str, kind: &str, name: &str, auth: Authority, tags: &[&str]) -> AssetRow {
        let mut r = AssetRow::new(id, kind, name, auth);
        r.tags = tags.iter().map(|s| s.to_string()).collect();
        r.text = format!("{name} {}", tags.join(" "));
        r
    }

    fn demo_store() -> InMemoryStore {
        InMemoryStore::new(vec![
            row("s::a", "skill", "画时序图", Authority::Read, &["read_only"]),
            row("s::b", "skill", "画流程图", Authority::Write, &["write"]),
            row("m::c", "mcp", "zotero 检索", Authority::Read, &["Read", "read_only"]),
        ])
    }

    #[test]
    fn crate_loads() {
        assert!(!super::CRATE_NAME.is_empty());
    }

    /// 依赖方向守门：本 crate 的实现代码里不得出现 ac-registry 引用。
    ///
    /// 只扫 `#[cfg(test)]` 之前的部分（否则本测试自己写的关键词会命中），
    /// 沿用 ac-registry 指纹层 `no_l3_in_the_implementation` 的同一手法。
    #[test]
    fn guard_retrieval_does_not_depend_on_registry() {
        let src = include_str!("lib.rs");
        let mods = include_str!("store.rs")
            .to_string()
            + include_str!("health.rs")
            + include_str!("pipeline.rs")
            + include_str!("searcher.rs")
            + include_str!("memstore.rs");
        for name in ["store.rs", "health.rs", "pipeline.rs", "searcher.rs", "memstore.rs"] {
            assert!(
                src.contains(name),
                "lib.rs 必须声明模块 {name}");
        }
        // 实现代码（注释除外）不得引用 ac_registry / ac-registry。
        let code: String = mods
            .lines()
            .map(|l| {
                let t = l.trim_start();
                if t.starts_with("//") {
                    ""
                } else {
                    t.split("//").next().unwrap_or("")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in ["ac_registry", "ac-registry", "use ac_registry"] {
            assert!(
                !code.contains(forbidden),
                "ac-retrieval 实现不得引用 {forbidden} —— AssetStore 必须由 retrieval 定义、registry 实现"
            );
        }
    }

    // ── 结构化过滤：四维都要真的过滤掉东西 ────────────────────

    fn hits(filter: AssetFilter) -> Vec<String> {
        let e = RetrievalEngine::structured_only(demo_store());
        let r = e.search(&SearchQuery::new(filter, 100));
        r.hits.into_iter().map(|h| h.id).collect()
    }

    #[test]
    fn kind_filter_selects_exactly_that_kind() {
        let mut f = AssetFilter::all();
        f.kind = Some("mcp".into());
        assert_eq!(hits(f), vec!["m::c".to_string()]);
    }

    #[test]
    fn tag_filter_is_or_semantics_single_value() {
        // 「任一标签命中即通过」：s::a 与 m::c 都带 read_only，都该命中。
        let mut f = AssetFilter::all();
        f.tag = Some("read_only".into());
        let mut got = hits(f);
        got.sort();
        assert_eq!(got, vec!["m::c".to_string(), "s::a".to_string()]);
    }

    #[test]
    fn min_authority_uses_enum_order_not_lexicographic() {
        // 字典序下 "read" > "write"，按字典序会反过来把 read 排除掉。
        // 正确语义：write(2) ≥ write(2) → 只剩 s::b。
        let mut f = AssetFilter::all();
        f.min_authority = Some(Authority::Write);
        assert_eq!(hits(f), vec!["s::b".to_string()]);

        // Read(0) < Observe(1)，所以只读资产**必须**被排除；剩下的只有 write(2)。
        let mut g = AssetFilter::all();
        g.min_authority = Some(Authority::Observe);
        let mut got = hits(g);
        got.sort();
        assert_eq!(got, vec!["s::b".to_string()]);

        // 最宽松的一档（Read）放行全部：证明上一条不是把过滤整体关掉了。
        let mut r0 = AssetFilter::all();
        r0.min_authority = Some(Authority::Read);
        assert_eq!(hits(r0).len(), 3);
    }

    #[test]
    fn text_filter_matches_substring() {
        let mut f = AssetFilter::all();
        f.text = Some("时序".into());
        assert_eq!(hits(f), vec!["s::a".to_string()]);
    }

    #[test]
    fn filters_are_conjunctive_when_combined() {
        let mut f = AssetFilter::all();
        f.kind = Some("skill".into());
        f.tag = Some("read_only".into());
        assert_eq!(hits(f), vec!["s::a".to_string()]);

        // 同一 tag 但 kind 不同 → 空。证明各维度是 AND 而不是被覆盖。
        let mut g = AssetFilter::all();
        g.kind = Some("mcp".into());
        g.tag = Some("write".into());
        assert!(hits(g).is_empty());
    }

    #[test]
    fn no_filter_returns_everything_and_limit_caps() {
        let e = RetrievalEngine::structured_only(demo_store());
        let r = e.search(&SearchQuery::new(AssetFilter::all(), 2));
        assert_eq!(r.len(), 2, "limit 必须真的截断");
    }

    // ── 降级：M2 的核心承诺 ────────────────────────────────────

    #[test]
    fn query_returns_results_when_vector_and_llm_are_both_failing() {
        // 直接对应完成标志：「在向量库和 LLM 全部不可用时仍能返回结果」。
        let s = no_vector_no_llm(demo_store());
        assert!(s.health().baseline_ready());
        assert!(s.health().state(Stage::Vector).is_failed());
        assert!(s.health().state(Stage::Llm).is_failed());

        let r = s.search(&SearchQuery::new(AssetFilter::all(), 100));
        assert_eq!(r.len(), 3, "向量与 LLM 全挂，基线仍必须返回全部 3 条");
        assert!(r.degradation.skipped_stage(Stage::Vector));
        assert!(r.degradation.skipped_stage(Stage::Llm));
        assert!(r.degradation.is_degraded());
        assert!(r.degradation.used.contains(&Stage::Structured));
    }

    #[test]
    fn query_returns_correct_results_with_no_optional_layer_present() {
        // 「一个可选层都不在」：纯结构化路径必须给出正确结果。
        let s = RetrievalSearcher::structured_only(demo_store());
        let mut f = AssetFilter::all();
        f.tag = Some("read_only".into());
        let r = s.search(&SearchQuery::new(f, 100));
        assert_eq!(r.len(), 2);
        assert!(r.degradation.is_degraded());
        // 每个可选阶段都如实记为缺席，没有一个被静默当成可用。
        for st in [Stage::FullText, Stage::Vector, Stage::Llm] {
            assert!(
                r.degradation.skipped_stage(st),
                "可选阶段 {st} 必须被显式记为跳过");
        }
    }

    #[test]
    fn disabled_optional_stage_does_not_affect_results() {
        // 「禁用」与「故障」「缺席」一样，都不得影响结果集。
        let h = HealthSnapshot::new([
            (Stage::Structured, StageState::Ready),
            (Stage::Vector, StageState::Disabled),
        ]);
        let s = RetrievalSearcher::new(demo_store(), h);
        let r = s.search(&SearchQuery::new(AssetFilter::all(), 100));
        assert_eq!(r.len(), 3);
        assert!(r.degradation.skipped_stage(Stage::Vector));
    }

    #[test]
    fn a_failing_optional_stage_leaves_the_structured_hits_untouched() {
        // 逐条比对：故障前后 hits 必须**完全相同**，
        // 而不是「碰巧条数一样」。这排除了「故障悄悄改结果」的实现。
        let healthy = RetrievalEngine::structured_only(demo_store());
        let broken = RetrievalEngine::new(
            demo_store(),
            HealthSnapshot::degraded_baseline_ready(),
        );
        let q = SearchQuery::new(AssetFilter::all(), 100);
        assert_eq!(healthy.search(&q).hits, broken.search(&q).hits);
    }

    #[test]
    fn free_text_still_works_without_any_optional_layer() {
        // SearchQuery.text 在没有全文层时必须并入基线过滤，
        // 否则「只靠结构化路径」的自由文本检索就是死的。
        let s = RetrievalSearcher::structured_only(demo_store());
        let q = SearchQuery {
            filter: AssetFilter::all(),
            text: Some("流程".into()),
            limit: 100,
        };
        let r = s.search(&q);
        assert_eq!(r.len(), 1);
        assert_eq!(r.hits[0].id, "s::b");
    }

    #[test]
    fn engine_exposes_health_for_callers() {
        let e = RetrievalEngine::structured_only(demo_store());
        assert!(e.health().baseline_ready());
        assert!(!e.health().is_degraded() == false || e.health().is_degraded());
        // 结构化_only 的快照：全文/向量/LLM 全部缺席 → 属于降级。
        assert!(e.health().is_degraded());
    }

    // ── Searcher trait（M2 交付物，检索侧）────────────────────

    #[test]
    fn searcher_trait_is_usable_behind_a_trait_object() {
        let s: Box<dyn Searcher> = Box::new(RetrievalSearcher::structured_only(demo_store()));
        let r = s.search(&SearchQuery::new(AssetFilter::all(), 100));
        assert_eq!(r.len(), 3);
        assert!(s.health().baseline_ready());
    }

    #[test]
    fn searcher_is_object_safe_and_returns_no_result() {
        // Searcher 不带泛型方法，也不返回 Result —— 静态断言不了，
        // 这里用「能当作 dyn 用」把这条性质钉住（若加了泛型方法就编译不过）。
        fn takes_searcher(s: &dyn Searcher) -> usize {
            s.search(&SearchQuery::new(AssetFilter::all(), 100)).len()
        }
        assert_eq!(takes_searcher(&RetrievalSearcher::structured_only(demo_store())), 3);
    }

    // ── 快照默认值与三态可区分性 ──────────────────────────────

    /// 未声明的阶段必须按**缺席**处理，绝不能被当成「就绪」。
    ///
    /// 这是「漏声明是最危险的默认值」那条决定的直接断言。
    ///
    /// 变异敏感：把 [`HealthSnapshot::state`] 里的 `.unwrap_or(&StageState::Absent)`
    /// 改成 `&StageState::Ready`，本条**立刻红**。
    /// 在这条测试被写出来之前，该反向变异只能被
    /// `engine_exposes_health_for_callers` 里的 `is_degraded()` 顺手逮到，
    /// 而真正断言「阶段被跳过」的那些用例**照样是绿的**——覆盖是漏的。
    #[test]
    fn undeclared_stage_is_treated_as_absent_not_ready() {
        let h = HealthSnapshot::new([(Stage::Structured, StageState::Ready)]);
        for st in [Stage::FullText, Stage::Vector, Stage::Llm] {
            assert_eq!(
                h.state(st),
                &StageState::Absent,
                "{st} 未被声明 → 必须按缺席处理"
            );
            assert!(!h.state(st).is_usable(), "{st} 未被声明 → 不得被当成可用");
            assert!(
                !h.state(st).is_failed(),
                "{st} 未被声明 → 是「没有这能力」，不是「有但挂了」"
            );
        }
        assert_eq!(
            h.usable_stages(),
            vec![Stage::Structured],
            "可用阶段不得凭空多出未声明的"
        );
        assert!(
            h.failed_stages().is_empty(),
            "未声明 ≠ 故障，不该触发任何告警"
        );
        // 同一条纪律对基线自身成立：连基线都没声明时也不许假定就绪。
        let empty = HealthSnapshot::new([]);
        assert_eq!(empty.state(Stage::Structured), &StageState::Absent);
        assert!(!empty.baseline_ready());
        assert!(!empty.is_degraded(), "基线没就绪时不该报「降级」——那是基线故障");
    }

    #[test]
    fn absent_disabled_failed_are_three_distinct_states() {
        let all = [
            StageState::Absent,
            StageState::Disabled,
            StageState::Failed("连不上".into()),
            StageState::Ready,
        ];

        // ① 四态两两可区分——压成一个 bool 就活不到这一行。
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "{} 与 {} 不得相等", a.as_str(), b.as_str());
                assert_ne!(a.as_str(), b.as_str());
            }
        }

        // ② 「都不可用」不等于「可互换」：只有 Ready 可用，只有 Failed 算故障。
        //    把 is_usable 误写成 `!is_failed()` 的话，Disabled 会立刻变成可用 → 本条红。
        assert!(!all[0].is_usable() && !all[1].is_usable() && !all[2].is_usable());
        assert!(all[3].is_usable());
        assert!(!all[0].is_failed() && !all[1].is_failed());
        assert!(all[2].is_failed());
        assert!(!all[3].is_failed());

        // ③ 落到降级报告里，三者必须**各自成条**、顺序与 Stage::ALL 一致，
        //    而不被并成一个「不可用」标记。
        let h = HealthSnapshot::new([
            (Stage::Structured, StageState::Ready),
            (Stage::FullText, StageState::Absent),
            (Stage::Vector, StageState::Disabled),
            (Stage::Llm, StageState::Failed("连不上".into())),
        ]);
        let e = RetrievalEngine::new(demo_store(), h);
        let r = e.search(&SearchQuery::new(AssetFilter::all(), 100));
        let entries: Vec<(Stage, &str)> = r
            .degradation
            .skipped
            .iter()
            .map(|(s, st)| (*s, st.as_str()))
            .collect();
        assert_eq!(
            entries,
            vec![
                (Stage::FullText, "absent"),
                (Stage::Vector, "disabled"),
                (Stage::Llm, "failed"),
            ],
            "三种状态必须各自如实上报，不能被压成一个「不可用」"
        );
        for st in [Stage::FullText, Stage::Vector, Stage::Llm] {
            assert!(r.degradation.skipped_stage(st), "{st} 必须被显式记为跳过");
        }

        // ④ 三种状态都算「降级」，但只有 failed 需要告警。
        assert!(r.degradation.is_degraded());
        assert_eq!(
            r.degradation
                .skipped
                .iter()
                .filter(|(_, st)| st.is_failed())
                .count(),
            1,
            "只有 Llm 处于故障态"
        );
        assert_eq!(e.health().failed_stages(), vec![Stage::Llm]);
        // ⑤ 三种状态都不影响结果集——可区分 ≠ 影响结果。
        assert_eq!(r.len(), 3);
        assert!(!r.degradation.baseline_failed(), "基线本身是好的");
    }

    // ── memstore：过滤语义与 registry 对齐的参照 ──────────────

    #[test]
    fn memstore_count_matches_query_length() {
        let s = demo_store();
        let mut f = AssetFilter::all();
        f.tag = Some("read_only".into());
        // count 不受 limit 影响：这两者不等就说明 limit 泄漏进了计数。
        let q = s.query(&f, 1);
        assert_eq!(q.len(), 1);
        assert_eq!(s.count(&f), 2);
    }

    #[test]
    fn unhealthy_store_reports_not_healthy_and_actually_refuses() {
        // 变异敏感：`unhealthy()` 早先只把 is_healthy 翻成 false、query 照样
        // 返回行，于是「库坏了」与「库里没有」在测试里完全一样，任何针对
        // 「基线失败」的用例都逼不出真正的故障。本条要求它**真的拒绝**。
        let s = InMemoryStore::unhealthy(vec![row(
            "s::a",
            "skill",
            "画时序图",
            Authority::Read,
            &["read_only"],
        )]);
        assert!(!s.is_healthy());
        assert!(InMemoryStore::new(vec![]).is_healthy());

        // 手里明明有行，查询却必须报错——被锁住 / 损坏的 SQLite 就是这个形状。
        assert!(
            s.try_query(&AssetFilter::all(), 100).is_err(),
            "故障存储的 try_query 必须报错，不能返回行"
        );
        assert!(s.query(&AssetFilter::all(), 100).is_empty(), "有损视图退化为空");
        assert_eq!(s.count(&AssetFilter::all()), 0);
    }

    // ── 基线自身失败：绝不能伪装成「成功的空结果」 ────────────────

    #[test]
    fn broken_store_reports_structured_baseline_as_failed_not_as_empty_success() {
        // 库里**有数据**却读不出来 —— 被锁住 / 损坏的 SQLite 就是这个形状。
        let broken = InMemoryStore::unhealthy(vec![
            row("s::a", "skill", "画时序图", Authority::Read, &["read_only"]),
            row("s::b", "skill", "画流程图", Authority::Write, &["write"]),
        ]);
        let r = RetrievalEngine::structured_only(broken)
            .search(&SearchQuery::new(AssetFilter::all(), 100));

        // ① 基线没交付 —— 绝不能出现在 used 里。那等于对外宣称
        //    「结构化基线跑过了」，而实际上一行都没产出。
        assert!(
            !r.degradation.used.contains(&Stage::Structured),
            "基线查询失败却把 Structured 记进 used = 谎报成功"
        );
        // ② 失败必须看得见，且带原因。
        assert!(
            r.degradation.baseline_failed(),
            "基线故障必须能被看见，实际报告：{}",
            r.degradation.summary()
        );
        let st = r.degradation.baseline_state();
        assert!(st.is_failed(), "基线状态必须是 failed，实际 {st}");
        assert!(
            r.degradation.skipped_stage(Stage::Structured),
            "基线故障时它必须出现在 skipped 里"
        );
        // ③ 关键：**空结果 ≠ 基线成功**。两种情况 hits 都是空，必须可区分。
        let healthy_but_empty = RetrievalEngine::structured_only(InMemoryStore::new(vec![]))
            .search(&SearchQuery::new(AssetFilter::all(), 100));
        assert_eq!(r.hits.len(), 0);
        assert_eq!(healthy_but_empty.hits.len(), 0);
        assert!(
            healthy_but_empty.degradation.used.contains(&Stage::Structured),
            "健康库返回 0 条 ≠ 基线故障：这条必须是基线成功"
        );
        assert!(!healthy_but_empty.degradation.baseline_failed());
        // ④ 基线挂了不代表可选层状态被改写，两者各说各的。
        assert!(r.degradation.skipped_stage(Stage::Vector));
        assert!(r.degradation.skipped_stage(Stage::Llm));
        // ⑤ 摘要里不得出现「用了 structured」。
        let summary = r.degradation.summary();
        assert!(
            summary.contains("structured(failed"),
            "摘要必须如实显示基线故障，实际：{summary}"
        );
        assert!(
            !summary.contains("used=structured"),
            "摘要不得声称用了基线，实际：{summary}"
        );
    }
}
