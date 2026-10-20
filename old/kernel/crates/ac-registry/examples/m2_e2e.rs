//! M2 基础切片端到端演示：在**真实语料**（用户 data/registry.db 的副本）上跑检索。
//!
//! 这个 example 刻意放在 ac-registry 而不是 ac-retrieval：
//! 它需要同时用到检索层的 RetrievalEngine/Searcher 与 registry 侧的
//! AssetStore 实现，而依赖方向是 ac-registry -> ac-retrieval，
//! 反过来在 ac-retrieval 里引用 registry 就违反硬性约束 #2 了。
//!
//! 运行：
//!   cd kernel
//!   cargo run --release --example m2_e2e -- <path-to-registry.db>
//!
//! 默认路径按本仓库布局取 ../data/registry.db。
//!
//! # 关于「不要碰用户的 registry.db」（M2 事故后的硬纪律）
//!
//! 本例程把库**复制**到工作目录再打开，绝不对原库调 `Registry::open`。
//! 原因不是谨慎过头：`Registry::open` 会执行建表/迁移并重写 SQLite 页，
//! **打开本身就是一个写操作**。M2 切片的第一版正是「对原库直接 open」，
//! 把用户的 registry.db 写成了字节不同、内容完好的状态。
//!
//! 因此这里的纪律是：
//! 1. 要在真实数据上验证，先 `std::fs::copy`，再 open **副本**；
//! 2. 要证明「原库没被动过」，用 `std::fs::read` 算 SHA256 —— 那是读文件字节，
//!    **不经过 SQLite**，不会触发任何写入；不要用「只读打开的 SQLite」去取证，
//!    哪怕带 mode=ro，迁移/建表的代码路径仍在。
//! 3. 跑完必须 assert 前后 SHA256 相等。
//!
//! 这条纪律由 `guard_no_example_opens_real_registry_db` 守门（见 ac-registry/src/lib.rs）。
//!
//! 它证明三件事（对应本切片的完成标志）：
//! 1. 结构化四维过滤（kind/tag/min_authority/text）在真实数据上有效，
//!    并把每条命中数与「绕开 trait 直接打 SQL」的结果对照；
//! 2. 向量库与 LLM 全部不可用时，查询依然返回结果并给出明确降级报告；
//! 3. ac-retrieval 与 ac-registry 通过 trait 通信，retrieval 不含任何
//!    SQLite / ac-registry 引用。

use ac_core::Authority;
use ac_registry::Registry;
use ac_retrieval::health::Stage;
use ac_retrieval::searcher::no_vector_no_llm;
use ac_retrieval::searcher::Searcher;
use ac_retrieval::store::AssetFilter;
use ac_retrieval::{RetrievalSearcher, SearchQuery};
use std::path::{Path, PathBuf};

/// 取数据库路径。默认指向用户正式库，但**只在只读的意义上使用它**。
fn db_path() -> String {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        return args[1].clone();
    }
    // kernel/ 下运行时 -> ../data/registry.db
    "../data/registry.db".to_string()
}

/// 算文件 SHA256。
///
/// 只用 `std::fs::read` 读原始字节，**不经过 SQLite**，因此不可能触发
/// 迁移、建表或页重写。要取证「原库有没有被动过」，这是唯一安全的做法。
fn sha256_of(p: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(p).expect("读取文件算 SHA256");
    let mut h = Sha256::new();
    h.update(&bytes);
    format!("{:x}", h.finalize())
}

/// 在 target/ 下开一个干净的工作目录（短路径，避免 MSVC 深路径问题）。
fn scratch() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    if p.file_name().map(|n| n == "deps").unwrap_or(false) {
        p.pop();
    }
    p.push("m2-e2e-work");
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("建例程工作目录");
    p
}

fn main() {
    let real_db = PathBuf::from(db_path());
    assert!(
        real_db.exists(),
        "找不到数据库 {}",
        real_db.display()
    );

    // ── copy-first：先取证，再复制，最后只在副本上工作 ──────────────
    //
    // 绝不对原库调 Registry::open（见文件头）。
    let before = sha256_of(&real_db);
    println!("== M2 基础切片端到端（真实库副本）==");
    println!("原库          : {}", real_db.display());
    println!("原库 SHA256   : {}", &before[..32]);

    let work = scratch();
    let db_copy = work.join("registry.db");
    std::fs::copy(&real_db, &db_copy).expect("复制数据库到工作目录");
    println!("工作副本      : {}", db_copy.display());

    let reg = Registry::open(&db_copy).expect("打开 registry.db 副本失败");
    println!("总资产数: {}", reg.total().unwrap_or(0));

    // 先看看真实库里到底有哪些标签 —— 用真实存在的标签做对账才有意义。
    let top_tags = reg.all_tags().unwrap_or_default();
    println!("\n-- 真实标签（前 8）--");
    for t in top_tags.iter().take(8) {
        println!("  {:<28} {}", t.tag, t.count);
    }

    // ── 1) 结构化过滤经 trait，并对账直接 SQL ────────────────────
    //
    // 每条用例都同时给出：
    //   - 经 AssetStore trait 的命中数（走 RetrievalSearcher）
    //   - 直接对同一库执行的 SQL 命中数（绕过 trait）
    // 两者必须相等 —— 这既验证过滤有效，也验证「经 trait」与「不经 trait」
    // 得到同一语义。
    println!("\n-- 结构化过滤（经 trait vs 直接 SQL 对账）--");
    // 取一个真实存在的搜索词，保证 text 维度打到非零命中而不是空集。
    let real_text = reg
        .query_assets(&ac_registry::AssetQuery { limit: 1, ..Default::default() })
        .ok()
        .and_then(|v| v.first().map(|s| s.name.chars().take(4).collect::<String>()))
        .unwrap_or_else(|| "skill".to_string());

    let owned_cases: Vec<(String, AssetFilter)> = vec![
        ("kind=skill".to_string(), AssetFilter { kind: Some("skill".into()), ..Default::default() }),
        ("kind=mcp".to_string(), AssetFilter { kind: Some("mcp".into()), ..Default::default() }),
        ("kind=plugin".to_string(), AssetFilter { kind: Some("plugin".into()), ..Default::default() }),
        (
            format!("tag={}", top_tags.first().map(|t| t.tag.as_str()).unwrap_or("__none__")),
            AssetFilter { tag: top_tags.first().map(|t| t.tag.clone()), ..Default::default() },
        ),
        (
            format!("tag={}", top_tags.get(1).map(|t| t.tag.as_str()).unwrap_or("__none__")),
            AssetFilter { tag: top_tags.get(1).map(|t| t.tag.clone()), ..Default::default() },
        ),
        ("min_authority=write".to_string(), AssetFilter { min_authority: Some(Authority::Write), ..Default::default() }),
        ("min_authority=destructive".to_string(), AssetFilter { min_authority: Some(Authority::Destructive), ..Default::default() }),
        ("text=skill".to_string(), AssetFilter { text: Some("skill".into()), ..Default::default() }),
        (
            "text=<真实资产名>".to_string(),
            AssetFilter { text: Some(real_text), ..Default::default() },
        ),
    ];

    let searcher: Box<dyn Searcher> = Box::new(RetrievalSearcher::structured_only(&reg));
    let mut all_ok = true;
    for (name, f) in owned_cases {
        let trait_count = searcher.search(&SearchQuery::new(f.clone(), 1000)).len() as u32;
        let sql_count = reg.count_assets(&ac_registry::AssetQuery {
            kind: f.kind.clone(),
            min_authority: f.min_authority.map(|a| a.as_str().to_string()),
            tag: f.tag.clone(),
            text: f.text.clone(),
            limit: 1000,
            offset: 0,
            // v8：默认不检出归档，与检索层 to_asset_query 的语义一致——
            // 对账必须两边口径相同，否则 trait 与 SQL 的命中数会差出归档那几条。
            include_archived: false,
        }).unwrap_or(0);
        let ok = trait_count == sql_count;
        all_ok &= ok;
        println!("  {:<22} trait={:<4} sql={:<4} {}", name, trait_count, sql_count, if ok { "OK" } else { "MISMATCH" });
    }
    println!("四维过滤对账: {}", if all_ok { "全部一致" } else { "存在不一致" });

    // 组合过滤（AND）也走一遍，验证各维度是真 AND 而不是被后者覆盖。
    let combo = AssetFilter {
        kind: Some("skill".into()),
        min_authority: Some(Authority::Write),
        ..Default::default()
    };
    let combo_trait = searcher.search(&SearchQuery::new(combo.clone(), 1000)).len() as u32;
    let combo_sql = reg.count_assets(&ac_registry::AssetQuery {
        kind: combo.kind.clone(),
        min_authority: combo.min_authority.map(|a| a.as_str().to_string()),
        tag: None, text: None, limit: 1000, offset: 0, include_archived: false,
    }).unwrap_or(0);
    println!("  {:<22} trait={:<4} sql={:<4} {}", "kind+authority(AND)", combo_trait, combo_sql,
        if combo_trait == combo_sql { "OK" } else { "MISMATCH" });

    // ── 2) 降级：向量 + LLM 全不可用 ──────────────────────────
    println!("\n-- 降级演示：向量库与 LLM 全部不可用 --");
    let degraded = no_vector_no_llm(&reg);
    println!("  健康快照: {}", degraded.health().summary());
    let q = SearchQuery::new(AssetFilter { kind: Some("skill".into()), ..Default::default() }, 1000);
    let r = degraded.search(&q);
    println!("  查询 kind=skill -> 命中 {} 条", r.len());
    println!("  降级报告: {}", r.degradation.summary());
    println!("  向量被跳过: {} / LLM 被跳过: {} / 已降级: {}",
        r.degradation.skipped_stage(Stage::Vector),
        r.degradation.skipped_stage(Stage::Llm),
        r.degradation.is_degraded());
    println!("  结论: 在向量库和 LLM 全部不可用时{}仍返回结果",
        if r.len() > 0 { "**" } else { "" });

    // ── 3) 一个可选层都不在（纯结构化）────────────────────────
    let pure = RetrievalSearcher::structured_only(&reg);
    let pr = pure.search(&q);
    println!("\n-- 纯结构化路径（无任何可选层）--");
    println!("  kind=skill 命中 {} 条；每个可选阶段都被显式记为跳过: {}",
        pr.len(),
        [Stage::FullText, Stage::Vector, Stage::Llm].iter().all(|s| pr.degradation.skipped_stage(*s)));

    // ── 原库字节未变（本次运行的唯一安全取证）──────────────────────
    let after = sha256_of(&real_db);
    println!("\n-- 原库完整性 --");
    println!("  SHA256 前 : {}", &before[..32]);
    println!("  SHA256 后 : {}", &after[..32]);
    assert_eq!(before, after, "原库被改动了！本例程只允许打开副本");

    if all_ok && combo_trait == combo_sql && r.len() > 0 {
        println!("\nALL OK");
    } else {
        println!("\nSOME CHECK FAILED");
        std::process::exit(1);
    }
}