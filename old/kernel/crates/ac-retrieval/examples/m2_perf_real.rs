//! M2 基准之二：**真实词表**语料。
//!
//! # 它存在的唯一理由：m2_perf 的合成语料对「相关性」不公平
//!
//! m2_perf.rs 的 1 万条语料由 26 个主题 × 15 个动词 × 15 个名词拼成。
//! 评审用真实分词器复现后发现：
//!
//! | 指标 | 合成语料实测 |
//! |---|---|
//! | 去重 token 数 | 仅 11175（10000 篇文档）|
//! | 每篇都出现的 token | 18 个（几乎每篇完全一样）|
//! | BM25 top1..top20 分差 | 仅 2.3–5.2%，20 个结果只有 13–19 个不同分 |
//!
//! 也就是说：合成语料里每篇文档的词袋几乎相同，BM25 分数挤在一起，
//! 排序近乎退化。它足以回答「索引建得够快吗 / 查询够快吗」，
//! 但**完全不足以**回答「排序有没有区分度」。
//!
//! 所以本文件另建一套语料：词表直接取自真实 vendor 树的 search_blob
//! （506 条资产的中英夹杂描述与能力声明），而不是人工编造的词表。
//! 两条结论必须分开读：
//!
//! - **合成语料 -> 只支持延迟结论**（建索引耗时、P95 查询延迟）；
//! - **真实语料 -> 用来检验词表多样性与 BM25 分数分布**，
//!   若它的 P95 仍 < 50ms，则延迟结论在真实词表上同样成立。
//!
//! # 关于「不要碰用户的 registry.db」（M2 事故后的硬纪律）
//!
//! 词表从**副本**里取：std::fs::copy 之后才 Registry::open。
//! 原库只用 std::fs::read 算 SHA256 前后对账，不经过 SQLite。
//! 守门见 ac-registry/src/db_safety_guard.rs。

use ac_core::Authority;
use ac_retrieval::fulltext::{analyze_terms, FullTextIndex, SyncPolicy};
use ac_retrieval::health::{HealthSnapshot, Stage, StageState};
use ac_retrieval::store::{AssetRow, AssetStore};
use ac_retrieval::{InMemoryStore, RetrievalEngine, SearchQuery};
use ac_registry::Registry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 与 m2_perf 完全相同的两条阈值，不允许为真实语料放宽。
const CRITERION_BUILD_SECS: f64 = 10.0;
const CRITERION_P95_MS: f64 = 50.0;

/// 真实语料同样造 1 万条，与合成语料同量级，延迟才可比。
const TARGET_ASSETS: usize = 10_000;
/// 每篇目标字符数，对齐真实库 search_blob 均值 680.9。
const TARGET_CHARS: usize = 681;
const QUERIES: usize = 500;
const QUERY_LIMIT: u32 = 20;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn truncate_to_chars(s: &mut String, n: usize) {
    match s.char_indices().nth(n) {
        Some((b, _)) => s.truncate(b),
        None => {}
    }
}

fn pct(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = (((sorted.len() as f64) * p / 100.0).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[idx]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// 文件 SHA256：std::fs::read 读原始字节，不经过 SQLite。
fn sha256_of(p: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(p).expect("读取文件算 SHA256");
    let mut h = Sha256::new();
    h.update(&bytes);
    format!("{:x}", h.finalize())
}

fn scratch(tag: &str) -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    if p.file_name().map(|n| n == "deps").unwrap_or(false) {
        p.pop();
    }
    p.push("m2-perf-real");
    p.push(tag);
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("建例程工作目录");
    p
}

/// 语料的公平性画像。
///
/// 这三个指标就是评审用来判「语料是否退化」的，合成与真实两套都算一遍。
struct CorpusProfile {
    docs: usize,
    total_tokens: usize,
    distinct_tokens: usize,
    tokens_per_doc: Vec<usize>,
    /// 出现在**每一篇**文档里的 token —— 越多越退化。
    in_every_doc: usize,
}

/// `min_token_chars` = 复现 mine_vocabulary 的 `chars().count() >= 2` 口径，
/// 使得「词表规模」与「语料去重 token 数」**是同一个口径下的两个数**，
/// 可以直接对照；`None` 表示不过滤（analyze_terms 的原始输出口径）。
fn profile(rows: &[AssetRow], min_token_chars: Option<usize>) -> CorpusProfile {
    let keep = |t: &String| min_token_chars.is_none_or(|n| t.chars().count() >= n);
    let mut doc_freq: HashMap<String, u32> = HashMap::new();
    let mut tokens_per_doc = Vec::with_capacity(rows.len());
    let mut total = 0usize;
    for r in rows {
        let terms: Vec<String> = analyze_terms(&r.text).into_iter().filter(keep).collect();
        total += terms.len();
        tokens_per_doc.push(terms.len());
        let mut seen: std::collections::HashSet<&String> = std::collections::HashSet::new();
        for t in &terms {
            if seen.insert(t) {
                *doc_freq.entry(t.clone()).or_insert(0) += 1;
            }
        }
    }
    let in_every_doc = doc_freq.values().filter(|c| **c as usize == rows.len()).count();
    CorpusProfile {
        docs: rows.len(),
        total_tokens: total,
        distinct_tokens: doc_freq.len(),
        tokens_per_doc,
        in_every_doc,
    }
}

impl CorpusProfile {
    fn mean(&self) -> f64 {
        self.total_tokens as f64 / self.docs as f64
    }
    /// 每篇的 token 数分布（min / p50 / max）。
    fn dist(&self) -> (usize, usize, usize) {
        let mut v = self.tokens_per_doc.clone();
        v.sort_unstable();
        let mid = v.len() / 2;
        (v[0], v[mid], v[v.len() - 1])
    }
}

/// 从真实库副本里挖词表：所有 search_blob 切出来的 token。
fn mine_vocabulary(real_db: &Path) -> (Vec<String>, Vec<(String, String)>) {
    let work = scratch("mine");
    let cp = work.join("registry.db");
    std::fs::copy(real_db, &cp).expect("复制库到工作目录");
    let reg = Registry::open(&cp).expect("打开副本");
    let snapshot = reg.index_source().expect("取 index_source");
    let mut vocab: HashMap<String, u32> = HashMap::new();
    let mut real_docs: Vec<(String, String)> = Vec::new();
    for e in &snapshot.entries {
        real_docs.push((e.id.clone(), e.text.clone()));
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for t in analyze_terms(&e.text) {
            if t.chars().count() >= 2 && seen.insert(t.clone()) {
                *vocab.entry(t).or_insert(0) += 1;
            }
        }
    }
    let mut v: Vec<String> = vocab.into_keys().collect();
    v.sort();
    (v, real_docs)
}

/// 真实库 search_blob 字符数的实测分布（2026-10-16，506 条只读采样）：
///
/// | 分位 | 字符数 |
/// |---|---|
/// | min | 101 |
/// | p50 | ~640 |
/// | p95 | 1293 |
/// | max | 4203 |
///
/// 这是一个**长尾分布**，所以采用对数尾轻度加载的 log 抽样：
/// 小文档密集、大文档稀疏。直接均匀抽样会过度拉长尾，
/// 而简单线性抽样又会过度稀疏大文档。
const REAL_CHARS_MIN: usize = 101;
const REAL_CHARS_P50: usize = 640;
const REAL_CHARS_P95: usize = 1293;
const REAL_CHARS_MAX: usize = 4203;

/// 按上述分布抽一个目标长度。
fn sample_target_chars(rng: &mut Rng) -> usize {
    let u = rng.below(1000) as f64 / 1000.0;
    let v = if u < 0.5 {
        // 下半段：min..p50，尾部密集
        REAL_CHARS_MIN as f64
            + (REAL_CHARS_P50 - REAL_CHARS_MIN) as f64 * (u / 0.5).sqrt()
    } else {
        // 上半段：p50..p95，按 p95 的尾部信度拉长
        let t = (u - 0.5) / 0.5;
        REAL_CHARS_P50 as f64
            + (REAL_CHARS_P95 - REAL_CHARS_P50) as f64 * t.powf(1.0 / 3.0)
    };
    let v = v.round() as usize;
    // 尾部 5% 拉到真实的长文：保留真库里确实存在的长文。
    if rng.below(100) < 5 {
        let hi = REAL_CHARS_P50 + (REAL_CHARS_MAX - REAL_CHARS_P50) * rng.below(100) / 100;
        return hi;
    }
    v.clamp(REAL_CHARS_MIN, REAL_CHARS_MAX)
}

/// 用真实词表 + 真实文档片段，混合造 1 万条。
///
/// 刻意让每篇由若干**真实 search_blob 片段**拼成，而不是随机词袋：
/// 真实描述里有大量低频专有名词与不规则搭配（框架名、命令、路径），
/// 随机抽词袋会丢掉这些恰恰是真实排序差异的来源。
fn build_corpus(vocab: &[String], real_docs: &[(String, String)], n: usize) -> Vec<AssetRow> {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let mut rows = Vec::with_capacity(n);
    for i in 0..n {
        let r = rng.below(100);
        let kind = if r < 91 { "skill" } else if r < 98 { "plugin" } else { "mcp" };
        let authority = match rng.below(100) {
            0..=39 => Authority::Read,
            40..=64 => Authority::Observe,
            65..=89 => Authority::Write,
            _ => Authority::Destructive,
        };
        // D6：不再把每篇都补/截到恰好 681 字符。
        // 真实库的 search_blob 是 101..4203 的长尾分布；把所有文档压成
        // 同一个长度，便会把「主题数」变成确定常数——连带着
        // BM25 的文档频率差异也被抹平了，而这正是「排序区分度」的源。
        // 因此按真实分布抽一个长度，而不是常数。
        let target_chars = sample_target_chars(&mut rng);
        // 取 2-4 段真实文档片段，再按需补真实词到目标长度。
        let nfrag = 2 + rng.below(3);
        let mut text = String::new();
        for _ in 0..nfrag {
            let d = &real_docs[rng.below(real_docs.len())];
            text.push_str(&d.1);
            text.push(' ');
        }
        let id = format!("{kind}::realvocab-{i}");
        let name = format!("{kind}-asset-{i}");
        text.insert_str(0, &format!("{id} {name} "));
        while text.chars().count() < target_chars {
            let t = &vocab[rng.below(vocab.len())];
            text.push_str(&t);
            text.push(' ');
        }
        truncate_to_chars(&mut text, target_chars);
        let mut row = AssetRow::new(id, kind, name, authority);
        row.text = text;
        rows.push(row);
    }
    rows
}

/// 查询集：全部取自**真实词表**里的高频词（真实用户会输入的真实词）。
fn real_queries(vocab: &[String], real_docs: &[(String, String)], n: usize) -> Vec<String> {
    let mut freq: HashMap<&str, u32> = HashMap::new();
    for (_, t) in real_docs {
        for tok in analyze_terms(t) {
            *freq.entry(Box::leak(tok.clone().into_boxed_str())).or_insert(0) += 1;
        }
    }
    let mut v: Vec<(&str, u32)> = freq.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let pool: Vec<&str> = v.into_iter().take(4000).map(|(s, _)| s).collect();
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    let mut out = Vec::with_capacity(n);
    let _ = vocab;
    while out.len() < n && !pool.is_empty() {
        let q = pool[rng.below(pool.len())].to_string();
        if !out.contains(&q) {
            out.push(q);
        }
    }
    out
}

fn main() {
    let db_arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../data/registry.db".to_string());
    let real_db = PathBuf::from(&db_arg);
    assert!(real_db.exists(), "找不到 {}", real_db.display());
    let before = sha256_of(&real_db);
    println!("============================================================");
    println!("CapabilityCore M2 基准之二 —— 真实词表语料");
    println!("============================================================");
    println!("原库 SHA256(前) : {}", &before[..32]);

    let (vocab, real_docs) = mine_vocabulary(&real_db);
    println!("\n[0] 词表来自真实 vendor 树");
    println!("  真实资产条数    : {}", real_docs.len());
    println!("  去重词表大小    : {}", vocab.len());
    // D5：上面这个数用的是 chars>=2 过滤（mine_vocabulary），
    // 而下面 profile() 的 analyze_terms 不做这个过滤。两个口径不一致就会
    // 报出一个与语料不对应的数字。这里把两个都列出来，并强制口径一致。
    println!("  词表过滤      : chars().count() >= 2（过滤后的真实词表）");
    println!("  说明            : profile() 计的 distinct_tokens 不做这个过滤，所以它 >= 本行，两者不可混用");
    assert!(!vocab.is_empty(), "词表为空，无法建语料");

    let rows = build_corpus(&vocab, &real_docs, TARGET_ASSETS);
    // D5：主口径必须与 mine_vocabulary 的 chars>=2 一致，否则两个数不可比。
    let prof_chars = profile(&rows, Some(2));
    let (mn, p50, mx) = prof_chars.dist();
    let prof_raw = profile(&rows, None);
    let _ = prof_raw;
    let (rn, rp50, rmx) = prof_raw.dist();
    println!("\n[1] 语料画像（真实词表）");
    println!("  文档数          : {}", prof_chars.docs);
    println!("  token 总数      : {}", prof_chars.total_tokens);
    println!("  去重 token 数   : {}", prof_chars.distinct_tokens);
    println!("    口径            : chars().count() >= 2（与上面词表**同一个过滤**，可直接对照）");
    println!("  每篇 token 数   : mean {:.1} / min {} / p50 {} / max {}", prof_chars.mean(), mn, p50, mx);
    println!("  每篇都出现的    : {} 个 token", prof_chars.in_every_doc);
    println!("  另计（无过滤口径，仅供参考）: 去重 {} / 每篇 token mean {:.1} / min {} / p50 {} / max {}",
        prof_raw.distinct_tokens, prof_raw.mean(), rn, rp50, rmx);
    println!("  每篇字符数    : mean {:.1} / min {} / p50 {} / max {}",
        rows.iter().map(|r| r.text.chars().count()).sum::<usize>() as f64 / rows.len() as f64,
        rows.iter().map(|r| r.text.chars().count()).min().unwrap_or(0),
        { let mut v: Vec<usize> = rows.iter().map(|r| r.text.chars().count()).collect(); v.sort_unstable(); v[v.len()/2] },
        rows.iter().map(|r| r.text.chars().count()).max().unwrap_or(0));
    println!("  （对照 m2_perf 合成语料：去重 11175 / 每篇都出现 18 个）");

    let store = InMemoryStore::new(rows.clone());

    // ── 冷启动建索引 ────────────────────────────────────────────
    let index_dir = scratch("index");
    println!("\n[2] 冷启动全量建索引（真实词表语料）");
    let cold = Instant::now();
    let idx = FullTextIndex::attach(&index_dir, &store, SyncPolicy::Auto);
    let cold_secs = cold.elapsed().as_secs_f64();
    println!("  状态            : {}", idx.state());
    println!("  文档数          : {}", idx.doc_count());
    println!("  **建索引耗时    : {:.3} s**", cold_secs);
    // 与 m2_perf 同一条红线：建索成功必须先由索引自身确认。
    // attach() 永不返回 Err，失败全部落到 state()；只看耗时会把
    // "什么都没做、因此很快" 的失败建索评成极快。
    let build_verdict = judge_build(&idx, cold_secs, rows.len());
    let build_ok = build_verdict.ok;
    let build_met = build_ok && cold_secs < CRITERION_BUILD_SECS;
    println!("  判定            : {} -> {}",
        build_verdict.verdict,
        if build_met { "达标" } else { "未达标" });

    // ── BM25 分数分布（相关性维度）───────────────────────────────
    println!("\n[3] BM25 分数分布（top1..top20 的区分度）");
    let queries = real_queries(&vocab, &real_docs, QUERIES);
    println!("  查询样本数      : {}", queries.len());
    let mut spreads: Vec<f64> = Vec::new();
    let mut distinct_scores: Vec<usize> = Vec::new();
    for q in queries.iter().take(60) {
        let hits = idx.search(q, QUERY_LIMIT as usize).unwrap_or_default();
        if hits.len() < 5 {
            continue;
        }
        let top = hits[0].score as f64;
        let last = hits[hits.len() - 1].score as f64;
        if top > 0.0 {
            spreads.push((top - last) / top);
        }
        let mut uniq: Vec<u32> = hits.iter().map(|h| (h.score * 1e6) as u32).collect();
        uniq.sort_unstable();
        uniq.dedup();
        distinct_scores.push(uniq.len());
    }
    if !spreads.is_empty() {
        spreads.sort_by(|a, b| a.partial_cmp(b).unwrap());
        distinct_scores.sort_unstable();
        let mean_spread: f64 = spreads.iter().sum::<f64>() / spreads.len() as f64;
        let mean_dist: f64 = distinct_scores.iter().sum::<usize>() as f64 / distinct_scores.len() as f64;
        println!("  top1-top20 分差  : 均值 {:.1}%（min {:.1}% / max {:.1}%）",
            mean_spread * 100.0, spreads[0] * 100.0, spreads[spreads.len() - 1] * 100.0);
        println!("  20 个结果里不同分: 均值 {:.1} 个", mean_dist);
    }
    println!("  （对照 m2_perf 合成语料：分差 2.3–5.2%，不同分 13–19 个 => 近乎退化）");

    // ── 查询延迟 ────────────────────────────────────────────────
    println!("\n[4] 查询延迟（完整流水线）");
    let engine = RetrievalEngine::new(
        store.clone(),
        HealthSnapshot::new([(Stage::Structured, StageState::Ready)]),
    )
    .with_fulltext(std::sync::Arc::new(idx));
    for q in queries.iter().take(20) {
        let _ = engine.search(&SearchQuery {
            filter: Default::default(),
            text: Some(q.clone()),
            limit: QUERY_LIMIT,
        });
    }
    let mut samples: Vec<Duration> = Vec::with_capacity(queries.len());
    // 同 m2_perf：退化查询不进样本池。索引死了的情景下退化查询会更快，
    // 把它们算进 P95 只会让数字更好看——就是误导的方向。
    let mut degraded = 0usize;
    let mut degraded_examples: Vec<String> = Vec::new();
    for q in &queries {
        let qq = q.clone();
        let t = Instant::now();
        let r = engine.search(&SearchQuery {
            filter: Default::default(),
            text: Some(qq),
            limit: QUERY_LIMIT,
        });
        let elapsed = t.elapsed();
        if r.degradation.skipped_stage(Stage::FullText) || r.degradation.baseline_failed() {
            degraded += 1;
            if degraded_examples.len() < 5 {
                degraded_examples.push(format!("{:?} -> {}", q, r.degradation.summary()));
            }
            continue;
        }
        samples.push(elapsed);
    }
    println!("  降级查询      : {} 个（已从 P95 样本池中排除）", degraded);
    for q in &degraded_examples {
        println!("    降级样例: {}", q);
    }
    if degraded * 10 > queries.len() {
        panic!(
            "查询集中有 {degraded}/{} 次查询发生降级：P95 量的是一个已经掉了的索引的耗时，不能报数。\n{}",
            queries.len(), degraded_examples.join("\n")
        );
    }
    samples.sort();
    let p95 = pct(&samples, 95.0);
    println!("  mean/p50/p95/p99 : {:.2} / {:.2} / {:.2} / {:.2} ms",
        ms(samples.iter().sum::<Duration>() / samples.len() as u32),
        ms(pct(&samples, 50.0)), ms(p95), ms(pct(&samples, 99.0)));
    let p95_ok = ms(p95) < CRITERION_P95_MS;
    println!("  判定            : {:.2} ms {} {:.1} ms -> {}",
        ms(p95), if p95_ok { "<" } else { ">=" }, CRITERION_P95_MS,
        if p95_ok { "达标" } else { "未达标" });

    let after = sha256_of(&real_db);
    println!("\n[5] 原库完整性");
    println!("  SHA256 前/后    : {} / {}", &before[..32], &after[..32]);
    assert_eq!(before, after, "原库被改动了！本例程只打开副本");

    println!("============================================================");
    println!("裁定（真实词表语料）");
    println!("============================================================");
    println!("  建索引 < 10s     : {} -> {}", build_verdict.verdict, if build_met { "达标" } else { "未达标" });
    println!("  P95 查询 < 50ms  : {:.2} ms -> {}", ms(p95), if p95_ok { "达标" } else { "未达标" });
    println!("  注意：合成语料(m2_perf)只支持**延迟**结论；");
    println!("        真实词表语料才用于检验词表多样性/BM25 区分度。");
    println!("============================================================");
    let _ = scratch("index");
    if !(build_met && p95_ok) {
        std::process::exit(2);
    }
}

/// 建索裁定（与 m2_perf.rs 同一逻辑）：先问“可用吗”，再问“快吗”。
///
/// 需求（三条任一不满足 = 不达标）：
///   1. `state()` 是 `Ready`（`Failed`/`Absent` 均不算；
///   2. `doc_count()` > 0 ——零文档的索引没有功能；
///   3. 文档数覆盖语料（`expect_docs`）——否则报可能被静默截断的索引。
struct BuildVerdict {
    ok: bool,
    verdict: String,
}

fn judge_build(idx: &FullTextIndex, elapsed_secs: f64, expect_docs: usize) -> BuildVerdict {
    let state = idx.state();
    let docs = idx.doc_count();
    if !matches!(state, StageState::Ready) {
        return BuildVerdict {
            ok: false,
            verdict: format!(
                "**建索未完成**（状态 {state:?}，耗时 {elapsed_secs:.3} s 不能证明任何事）"
            ),
        };
    }
    if docs == 0 {
        return BuildVerdict {
            ok: false,
            verdict: format!(
                "**建索产出零文档的索引**（状态 Ready 但 doc_count=0；耗时 {elapsed_secs:.3} s）"
            ),
        };
    }
    if docs < expect_docs {
        return BuildVerdict {
            ok: false,
            verdict: format!(
                "**建索不完整**（索引 {docs} 篇 < 语料 {expect_docs} 篇，有文档被静默丢弃）"
            ),
        };
    }
    BuildVerdict {
        ok: true,
        verdict: format!("建索成功（{docs} 篇文档），耗时 {elapsed_secs:.3} s"),
    }
}
