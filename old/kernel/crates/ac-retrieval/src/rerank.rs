//! 精排（rerank）—— M2 功能链路的最后一步。
//!
//! # 它做什么、不做什么
//!
//! BM25 召回是「**召回优先**」的：只要沾边就捞回来，不管排序对不对。
//! 精排在结构化过滤之后，把候选按一个**可写下来的公式**重新排一次，
//! 让「名字里就有这个词」的资产排在「正文里顺带提了一句」的资产前面。
//!
//! **明确不做的事**：不引入任何学习模型（cross-encoder / LLM / 向量）。
//! 理由有三，每条都写在这里而不是藏在心里：
//!
//! 1. M3 的完成标志是「断开向量服务，查询仍返回结果」。精排里放一个
//!    不可用的组件，等于把这条承诺又变成一次口头约定。
//! 2. 调研文档引用的 NeMo 数据：+8.7 分 nDCG 换来 160 倍延迟。
//!    在 P95 < 50ms 的预算里，一个外部推理调用会直接吃掉全部余量。
//! 3. 本地优先是硬性约束，一个需要下载权重的模型会破坏「拷贝即用」的打包形态。
//!
//! # 三个分量，逐个可解释
//!
//! ```text
//! score = w_bm25   × bm25_norm     // 倒排本身的分数，归一化到 [0,1]
//!       + w_name   × name_hit      // 查询词命中资产名的比例
//!       + w_cover  × coverage      // 查询词被正文覆盖的比例
//! ```
//!
//! - `bm25_norm`：本次候选里的最大值归一。BM25 原始分没有绝对意义
//!   （随语料规模漂移），只有相对顺序有意义，所以必须归一。
//! - `name_hit`：**独立字段加权**。倒排把 name 和 description 混在一个
//!   `search_blob` 里，一份文档里名字出现 3 次和正文出现 3 次得分相同，
//!   而这两件事的相关性差别很大。这个分量把「名字命中」单独拎出来。
//! - `coverage`：BM25 不奖励「命中更多不同查询词」的文档——
//!   一篇到处蹭一个高频词的文档可能排在精准命中 3 个词的文档前面。
//!   这个分量补的就是这一点。
//!
//! 三个分量互不重叠、可单独打印出来给人看（见 [Ranked::explain]），
//! 权重是常数不是训练出来的，所以「为什么这条排第一」永远答得出来。

use crate::fulltext::analyze_terms;
use crate::store::AssetRow;

/// 三个分量的权重。
///
/// 数值来源：按「名字命中 > 正文命中」的产品直觉手工设定，
/// 并经 `rerank_prefers_exact_name_match` 与
/// `rerank_rewards_query_term_coverage` 两条测试钉住相对关系。
/// **刻意不给出「最优值」的说法**——没有标注集就没有最优，只有可解释。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankWeights {
    pub bm25: f32,
    pub name_hit: f32,
    pub coverage: f32,
}

impl Default for RankWeights {
    fn default() -> Self {
        // bm25 仍是主导（0.55），但两个结构化分量合计 0.45 足以翻盘：
        // 一个名字精确命中的资产不该被正文里蹭词的资产压下去。
        Self {
            bm25: 0.55,
            name_hit: 0.30,
            coverage: 0.15,
        }
    }
}

/// 进入精排的一条候选。
#[derive(Debug, Clone, PartialEq)]
pub struct RankInput {
    pub row: AssetRow,
    /// BM25 原始分（未归一）。
    pub bm25: f32,
}

/// 精排后的结果，带**每一项分量**，供 UI/日志解释排序理由。
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub row: AssetRow,
    pub bm25: f32,
    pub bm25_norm: f32,
    pub name_hit: f32,
    pub coverage: f32,
    pub score: f32,
}

impl Ranked {
    /// 人可读的排序理由，一行一条。给自检、日志和将来的 UI 用。
    pub fn explain(&self) -> String {
        format!(
            "{} score={:.4} = bm25 {:.4}×{:.3} + name {:.4}×{:.3} + cover {:.4}×{:.3}",
            self.row.name,
            self.score,
            self.bm25_norm,
            RankWeights::default().bm25,
            self.name_hit,
            RankWeights::default().name_hit,
            self.coverage,
            RankWeights::default().coverage
        )
    }
}

/// 对 BM25 候选重排。
///
/// # 稳定性
///
/// 排序键是 `(score 降序, id 升序)`。id 是确定性的兜底键：
/// 没有它，同分的候选会按输入顺序（= BM25 的近似并列顺序）排列，
/// 而那在不同 tantivy 版本/segment 切分下**不保证一致**——
/// 「同样的查询两次给出不同顺序」是极难排查的一类问题。
pub fn rerank(query: &str, items: Vec<RankInput>, weights: RankWeights) -> Vec<Ranked> {
    let terms = analyze_terms(query);
    if items.is_empty() {
        return Vec::new();
    }
    let max_bm25 = items
        .iter()
        .map(|i| i.bm25)
        .fold(0.0f32, f32::max)
        .max(f32::MIN_POSITIVE);

    let mut out: Vec<Ranked> = items
        .into_iter()
        .map(|it| {
            let bm25_norm = (it.bm25 / max_bm25).clamp(0.0, 1.0);
            let (name_hit, coverage) = if terms.is_empty() {
                // 查询切不出词（例如全是符号）时，BM25 本就没有信息，
                // 两个结构化分量也都无从谈起。给 0 而不是 1。
                (0.0, 0.0)
            } else {
                let name_lower = it.row.name.to_lowercase();
                let text_lower = it.row.text.to_lowercase();
                let in_name = terms
                    .iter()
                    .filter(|t| name_lower.contains(t.as_str()))
                    .count();
                let in_text = terms
                    .iter()
                    .filter(|t| text_lower.contains(t.as_str()))
                    .count();
                (
                    in_name as f32 / terms.len() as f32,
                    in_text as f32 / terms.len() as f32,
                )
            };
            let score =
                weights.bm25 * bm25_norm + weights.name_hit * name_hit + weights.coverage * coverage;
            Ranked {
                bm25: it.bm25,
                row: it.row,
                bm25_norm,
                name_hit,
                coverage,
                score,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.row.id.cmp(&b.row.id))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::Authority;

    fn row(id: &str, name: &str, text: &str) -> AssetRow {
        let mut r = AssetRow::new(id, "skill", name, Authority::Read);
        r.text = text.to_string();
        r
    }

    fn input(id: &str, name: &str, text: &str, bm25: f32) -> RankInput {
        RankInput {
            row: row(id, name, text),
            bm25,
        }
    }

    #[test]
    fn rerank_prefers_exact_name_match_over_body_mention() {
        // 两条 BM25 分几乎一样（10.0 vs 9.9）。
        // 但查询词出现在 s::a 的**名字**里，只出现在 s::b 的正文里。
        // 精排必须把 a 排前面——这正是 name_hit 分量存在的理由。
        let out = rerank(
            "时序图",
            vec![
                input("s::b", "无关的名字", "正文里顺带提了一句时序图", 9.9),
                input("s::a", "时序图", "正文随便写点什么", 10.0),
            ],
            RankWeights::default(),
        );
        assert_eq!(out[0].row.id, "s::a");
        assert_eq!(out[0].name_hit, 1.0);
        assert!(
            out[1].name_hit < out[0].name_hit,
            "name_hit 必须真的区分两条，a={} b={}",
            out[0].name_hit,
            out[1].name_hit
        );
    }

    #[test]
    fn rerank_rewards_query_term_coverage() {
        // 两条 BM25 分相同、name_hit 也相同，唯一差别是 coverage：
        // s::wide 覆盖查询里 3 个词里的 3 个，s::narrow 只覆盖 1 个。
        // BM25 本身**不奖励覆盖面**（一篇到处蹭高频词的文档完全可能排在
        // 精准命中 3 个词的文档前面），所以这个分量必须自己补上。
        let out = rerank(
            "画时序图",
            vec![
                input("s::narrow", "同名", "时序图的补充说明，其余无关", 5.0),
                input("s::wide", "同名", "画时序图 画 时序 图 三个词全覆盖", 5.0),
            ],
            RankWeights::default(),
        );
        assert_eq!(out[0].name_hit, out[1].name_hit, "前置条件：name_hit 必须相同");
        assert_eq!(out[0].bm25_norm, out[1].bm25_norm, "前置条件：bm25 必须相同");
        assert!(
            out[0].coverage > out[1].coverage,
            "coverage 必须真的区分两条，wide={} narrow={}",
            out[0].coverage,
            out[1].coverage
        );
        assert_eq!(out[0].row.id, "s::wide", "覆盖面大的应排前");
    }

    #[test]
    fn rerank_normalises_bm25_so_raw_magnitude_does_not_leak() {
        // 两次调用的 BM25 分整体放大 100 倍，**相对顺序不该变**。
        // BM25 原始分随语料规模漂移，没有绝对意义——不归一的话，
        // 语料一变，name_hit/coverage 这两个结构化分量就等于失效了。
        let mk = |scale: f32| {
            rerank(
                "时序图",
                vec![
                    input("s::a", "时序图", "时序图 时序", 12.0 * scale),
                    input("s::b", "别的", "提到时序图而已", 6.0 * scale),
                ],
                RankWeights::default(),
            )
        };
        let a = mk(1.0);
        let b = mk(100.0);
        assert_eq!(a[0].row.id, b[0].row.id);
        for (x, y) in a.iter().zip(b.iter()) {
            assert!(
                (x.score - y.score).abs() < 1e-4,
                "整体放大不该改变精排分数：{} vs {}",
                x.score,
                y.score
            );
        }
        assert!((a[0].bm25_norm - 1.0).abs() < 1e-6, "最高分归一后必须是 1.0");
    }

    #[test]
    fn rerank_is_deterministic_for_equal_scores() {
        // 同分时按 id 升序。少了这个兜底键，并列项的顺序就取决于
        // tantivy 的 segment 切分与并列处理——「同样的查询两次顺序不同」
        // 是极难排查的一类问题。
        let out = rerank(
            "图",
            vec![
                input("s::z", "图", "图", 1.0),
                input("s::a", "图", "图", 1.0),
                input("s::m", "图", "图", 1.0),
            ],
            RankWeights::default(),
        );
        assert_eq!(
            out.iter().map(|r| r.row.id.as_str()).collect::<Vec<_>>(),
            vec!["s::a", "s::m", "s::z"]
        );
    }

    #[test]
    fn rerank_explain_exposes_every_component() {
        // 「可解释」不能只是一句口号：必须能把三个分量逐项打出来给人看。
        let out = rerank(
            "时序图",
            vec![input("s::a", "时序图", "时序图 时序 图", 4.0)],
            RankWeights::default(),
        );
        let s = out[0].explain();
        for token in ["score=", "bm25", "name", "cover"] {
            assert!(s.contains(token), "排序理由里必须出现 {token}，实际：{s}");
        }
    }

    #[test]
    fn rerank_handles_empty_input_and_untokenizable_query() {
        assert!(rerank("时序图", vec![], RankWeights::default()).is_empty());
        // 查询切不出词（全是标点）时不能崩，也不能凭空给满分。
        let out = rerank(
            "---",
            vec![input("s::a", "名字", "正文", 3.0)],
            RankWeights::default(),
        );
        assert_eq!(out[0].name_hit, 0.0);
        assert_eq!(out[0].coverage, 0.0);
        assert!((out[0].score - 0.55).abs() < 1e-6, "只剩 bm25 一项：{}", out[0].score);
    }

    #[test]
    fn rerank_weights_sum_to_one_so_scores_stay_comparable() {
        let w = RankWeights::default();
        assert!((w.bm25 + w.name_hit + w.coverage - 1.0).abs() < 1e-6);
    }
}
