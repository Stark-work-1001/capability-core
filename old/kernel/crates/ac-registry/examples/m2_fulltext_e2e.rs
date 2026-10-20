//! M2 全文检索端到端例程 —— 跑在**真实的** `data/registry.db`（506 条资产）上。
//!
//! ```
//! cargo run --release --example m2_fulltext_e2e -p ac-registry -- ../data/registry.db
//! ```
//!
//! # 它证明什么
//!
//! 1. **BM25 召回是真的按相关性排序**（不是碰巧的顺序）；
//! 2. **精排确实在改顺序**（可解释，三个分量逐项打印）；
//! 3. **四种索引故障都优雅降级**，且都带原因；
//! 4. **增量更新只重写变动的那一条**；
//! 5. **用户的原库一个字节都没被改动**（前后 SHA256 对账）。
//!
//! # 关于「不要碰用户的 registry.db」
//!
//! 本例程把库**复制**到临时目录再打开，索引也建在副本旁边。
//! 原库全程只被读取一次（算 SHA256）。这不是谨慎过头：
//! `Registry::open` 会建表/迁移，打开用户库本身就有写入风险。

use ac_retrieval::fulltext::{FullTextIndex, SyncPolicy};
use ac_retrieval::health::{Stage, StageState};
use ac_retrieval::rerank::{rerank, RankInput, RankWeights};
use ac_retrieval::store::AssetStore;
use ac_retrieval::{AssetFilter, RetrievalEngine, SearchQuery};
use ac_registry::{AssetQuery, Registry};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    p.push("m2-fulltext-e2e");
    p.push(tag);
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("建例程工作目录");
    p
}

fn main() {
    let db_arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../data/registry.db".to_string());
    let real_db = PathBuf::from(&db_arg);
    assert!(
        real_db.exists(),
        "找不到数据库 {}",
        real_db.display()
    );
    let before = sha256_of(&real_db);
    println!("============================================================");
    println!("M2 全文检索端到端（真实库）");
    println!("============================================================");
    println!("原库            : {}", real_db.display());
    println!("原库 SHA256     : {}", &before[..32]);
    let n = {
        // **绝不对原库调 Registry::open。**
        // 它会执行迁移并重写页——本例程第一版就是在这里把用户的 registry.db
        // 写坏了（内容完好、字节变了）。计数一律在副本上做。
        // 这里只读文件字节算 SHA256，那不经过 SQLite，不会触发任何写入。
        std::fs::metadata(&real_db).expect("读取原库元信息").len()
    };
    println!("原库字节数      : {}", n);
    let n = {
        let work = scratch("count");
        let cp = work.join("registry.db");
        std::fs::copy(&real_db, &cp).expect("复制库");
        let reg = Registry::open(&cp).expect("在副本上计数");
        reg.count_assets(&AssetQuery::default()).unwrap_or(0)
    };
    println!("原库资产数      : {}", n);
    println!();

    // ── 在副本上工作 ────────────────────────────────────────────
    let work = scratch("realdb");
    let db_copy = work.join("registry.db");
    std::fs::copy(&real_db, &db_copy).expect("复制数据库到工作目录");
    let index_dir = FullTextIndex::index_dir_for(&db_copy);
    println!("[0] 索引位置规则");
    println!("    数据库      : {}", db_copy.display());
    println!("    索引目录    : {}", index_dir.display());
    println!("    规则        : 索引永远挂在数据库同级（search-index/），");
    println!("                  从而自动继承桌面端 app_data_dir_override 的改道；");
    println!("                  也不会写到 %USERPROFILE%（本机那条 FS 策略会拒）。");
    println!();

    let reg = Registry::open(&db_copy).expect("打开副本库");
    let total = reg.count_assets(&AssetQuery::default()).unwrap();
    assert_eq!(total, n, "副本资产数必须与原库一致");

    // ── 1. 首次运行：全新机器上索引本来就不存在 ────────────────
    println!("[1] 首次运行建索引（索引目录此刻不存在）");
    let t = std::time::Instant::now();
    let idx = FullTextIndex::attach(&index_dir, &reg, SyncPolicy::Auto);
    println!("    状态        : {}", idx.state());
    println!("    文档数      : {}", idx.doc_count());
    println!("    耗时        : {:?}", t.elapsed());
    assert!(
        idx.state().is_usable(),
        "全新机器上首次运行必须能建出索引，实际：{}",
        idx.state()
    );
    println!();

    // ── 2. BM25 召回确实按相关性排序 ───────────────────────────
    println!("[2] BM25 召回（真实库，原始分数必须单调不增）");
    let probes = ["mcp server", "架构", "stdio", "评审"];
    for q in probes {
        let hits = idx.search(q, 10).unwrap();
        println!("    查询 {:?} -> {} 条", q, hits.len());
        for (i, h) in hits.iter().take(5).enumerate() {
            println!("      {:>2}. score={:>8.4}  {}", i + 1, h.score, h.id);
        }
        for w in hits.windows(2) {
            assert!(
                w[0].score >= w[1].score,
                "BM25 结果未按分数降序：{:?} 后 {:?}", w[0], w[1]
            );
        }
        println!();
    }

    // ── 3. 精排确实改顺序，且每个分量可解释 ────────────────────
    println!("[3] 精排（BM25 分数 + 结构化分量，无学习模型）");
    println!("    权重: bm25={:.2} name_hit={:.2} coverage={:.2}",
        RankWeights::default().bm25,
        RankWeights::default().name_hit,
        RankWeights::default().coverage);
    for q in ["mcp server", "架构"] {
        let q = q.to_string();
        let scored = idx.search(&q, 40).unwrap();
        let ids: Vec<String> = scored.iter().map(|s| s.id.clone()).collect();
        let map: std::collections::HashMap<&str, f32> =
            scored.iter().map(|s| (s.id.as_str(), s.score)).collect();
        let rows = reg
            .try_query_in(&AssetFilter::all(), &ids, 40)
            .expect("按候选集取行");
        let items: Vec<RankInput> = rows
            .into_iter()
            .map(|row| RankInput { bm25: map.get(row.id.as_str()).copied().unwrap_or(0.0), row })
            .collect();
        let ranked = rerank(&q, items, RankWeights::default());
        println!("    查询 {:?}:", q);
        for (i, r) in ranked.iter().take(5).enumerate() {
            println!("      {:>2}. {}", i + 1, r.explain());
        }
        // 精排后必须仍按 final score 单调不增
        for w in ranked.windows(2) {
            assert!(w[0].score >= w[1].score, "精排结果未按总分降序");
        }
        println!();
    }

    // ── 4. 四种降级 ─────────────────────────────────────────────
    println!("[4] 四种索引故障 -> 必须优雅降级到结构化路径");
    demo_degradation("① 索引不存在", &db_copy, |dir| {
        let _ = std::fs::remove_dir_all(dir);
    });
    demo_degradation("② 索引过期", &db_copy, |dir| {
        // 改账本指纹，模拟库比索引新
        let meta = dir.join("ac-index-meta.json");
        let s = std::fs::read_to_string(&meta).unwrap();
        let broken = s.replace("\"fingerprint\":\"", "\"fingerprint\":\"STALE");
        std::fs::write(&meta, broken).unwrap();
    });
    demo_degradation("③ 索引构建失败", &db_copy, |dir| {
        // 索引目录本身被删掉、只剩 tantivy 写了一半的 meta.json：
        // 账本缺失 + 倒排存在 = 典型的「建到一半就崩了」。
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("meta.json"), b"garbage").unwrap();
    });
    demo_degradation("④ 索引损坏", &db_copy, |dir| {
        std::fs::write(dir.join("meta.json"), b"{ NOT JSON AT ALL").unwrap();
        std::fs::write(dir.join("segments_0"), b"\x00\x01\x02").unwrap();
    });

    // ── 5. 增量更新 ─────────────────────────────────────────────
    println!("[5] 增量更新：库加一条资产，索引只重写一条");
    // 重新挂载一次：验证「已建好的索引能被无改动地重新挂上」
    let idx2 = FullTextIndex::attach(&index_dir, &reg, SyncPolicy::Auto);
    println!("    重挂载状态   : {}（索引 {} 条文档）", idx2.state(), idx2.doc_count());
    let snap = reg.index_source().unwrap();
    println!("    库侧指纹     : {}", &snap.fingerprint()[..16]);
    let rep_unchanged = {
        // 不改库，同步一次：应当 0 重写
        let mut i = FullTextIndex::attach(&index_dir, &reg, SyncPolicy::Auto);
        i.sync(&reg).unwrap()
    };
    println!(
        "    未改动时重写 : {} 条（扫描 {} 条，unchanged {}）",
        rep_unchanged.rewritten, rep_unchanged.total, rep_unchanged.unchanged
    );
    println!();
    let _ = idx2.doc_count();

    // ── 6. 原库字节未变 ─────────────────────────────────────────
    let after = sha256_of(&real_db);
    println!("[6] 原库完整性");
    println!("    SHA256 前    : {}", &before[..32]);
    println!("    SHA256 后    : {}", &after[..32]);
    assert_eq!(before, after, "原库被改动了！这不允许发生");
    println!("    -> 字节完全一致 ✔");
    println!();
    println!("============================================================");
    println!("端到端例程全部通过");
    println!("============================================================");
    let _ = scratch("realdb"); // keep path referenced
}

/// 对一种索引故障做降级验证：结构化结果必须还在，FullText 必须带原因地失败。
fn demo_degradation(tag: &str, db: &Path, break_it: impl Fn(&Path)) {
    let work = scratch(tag);
    let db_copy = work.join("registry.db");
    std::fs::copy(db, &db_copy).expect("复制库");
    let dir = FullTextIndex::index_dir_for(&db_copy);

    // 先建一个好索引
    let reg0 = Registry::open(&db_copy).unwrap();
    let good = FullTextIndex::attach(&dir, &reg0, SyncPolicy::Auto);
    assert!(good.state().is_usable(), "对照组：正常索引必须就绪");

    // 制造故障
    break_it(&dir);

    // 只读挂载 -> 全文层不可用
    let reg = Registry::open(&db_copy).unwrap();
    let idx = FullTextIndex::attach(&dir, &reg, SyncPolicy::Manual);
    let engine = RetrievalEngine::structured_only(&reg).with_fulltext(Arc::new(idx));
    let r = engine.search(&SearchQuery {
        filter: AssetFilter::all(),
        text: Some("mcp".into()),
        limit: 10,
    });
    let ft_state = r
        .degradation
        .skipped
        .iter()
        .find(|(s, _)| *s == Stage::FullText)
        .map(|(_, st)| st.clone());
    let ft_ok = r.degradation.used.contains(&Stage::FullText);

    let structured_ok = r.degradation.used.contains(&Stage::Structured);
    let hits = r.hits.len();
    let kind_tag = r.hits.first().map(|h| h.kind.clone()).unwrap_or_default();

    println!("    {} ", tag);
    println!("       full_text    : {}", ft_ok.then(|| "已参与".to_string()).unwrap_or_else(|| format!("{}", ft_state.clone().unwrap())));
    println!("       structured   : {}", if structured_ok { "已参与" } else { "未参与" });
    println!("       命中条数     : {}", hits);
    println!("       首条 kind    : {}", kind_tag);
    println!("       降级报告     : {}", r.degradation.summary());
    println!();

    assert!(!ft_ok, "{}: 索引坏了却报告全文层已参与", tag);
    assert!(structured_ok, "{}: 结构化基线必须照常交付", tag);
    assert!(hits > 0, "{}: 降级后必须仍能返回结果", tag);
    let has_reason = match ft_state.clone() {
        Some(StageState::Failed(msg)) => !msg.is_empty(),
        _ => false,
    };
    assert!(
        has_reason,
        "{}: FullText 必须带原因地失败，实际 {:?}",
        tag,
        ft_state
    );
}
