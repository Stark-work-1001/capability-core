//! 数据层体检探针：块标量垃圾 / 重复去重队列 / 来源与注册方式分布。
//!
//! 用法（**必须显式给 --db，绝不默认指向用户的 data/registry.db**）：
//!
//!     cargo run --release -p ac-registry --example data_probe -- --db <副本绝对路径> [--root <扫描根>] [--ingest]
//!
//! 为什么必须显式给：Registry::open 会执行迁移（写操作）。
//! 守门规则见 db_safety_guard.rs 的 WATCHED —— 本例程已登记，
//! 写法与 diag_probe.rs / m4_sweep.rs 保持一致：
//! 换个写法（let-else 直接接 arg）就追不到命令行实参，会被判红。
//!
//! 三种量法对应 2026-10-18 数据层三处缺陷：
//! 1. frontmatter 块标量：description 是 > / >- / | / string 字面量的条数；
//! 2. 去重队列虚高：dedup_candidates() 返回多少组，L1 / L2 各多少，
//!    以及 L2 里「跨 logical_id」的组数（修好后应为 0）；
//! 3. 来源与注册方式：origin_kind / registration_kind / origin_agent /
//!    self_contained 四列的分布。

use ac_registry::{ingest, AssetQuery, Registry};
use rusqlite::Connection;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let db = arg(&args, "--db");
    let Some(db) = db else {
        eprintln!("必须显式给 --db <副本路径>：本工具绝不默认打开用户的 data/registry.db");
        std::process::exit(2);
    };
    let root = arg(&args, "--root");
    let do_ingest = args.iter().any(|a| a == "--ingest");

    // 扫描根先算出来：没有 --root 时退到库的上一级目录的 vendor/，存在才用。
    let scan_root = root
        .clone()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::path::PathBuf::from(&db)
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.join("vendor"))
        })
        .filter(|p| p.is_dir());

    println!("库      : {db}");
    if let Some(r) = &scan_root {
        println!("扫描根  : {}", r.display());
    }
    println!();

    let reg = match Registry::open(Path::new(&db)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("打不开库 {db}: {e}");
            std::process::exit(1);
        }
    };

    println!("── 迁移后版本 ──");
    println!("代码 SCHEMA_VERSION = {}", ac_registry::store::SCHEMA_VERSION);
    println!("代码 PARSER_REV     = {}", ac_registry::store::PARSER_REV);
    println!();

    if do_ingest {
        let Some(root) = scan_root.clone() else {
            eprintln!("--ingest 需要 --root <扫描根>");
            std::process::exit(2);
        };
        println!("── 重新扫描入库 ──");
        match ingest(&reg, &root) {
            Ok(r) => println!(
                "解析 {} / 新增 {} / 更新 {} / 未变化 {} / 清除 {} / 跳过 {}",
                r.parsed, r.inserted, r.updated, r.unchanged, r.removed, r.skipped.len()
            ),
            Err(e) => {
                eprintln!("入库失败: {e}");
                std::process::exit(1);
            }
        }
        println!();
    }

    let conn = Connection::open(Path::new(&db)).expect("二次只读连接");
    let q = |sql: &str| -> i64 {
        conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(-1)
    };

    println!("── 缺陷 B：frontmatter 块标量 ──");
    println!(
        "description 垃圾条数（> >- | string） = {}",
        q("SELECT COUNT(*) FROM assets WHERE TRIM(description) IN ('>', '>-', '|', 'string')")
    );
    println!(
        "description 以 > 或 | 开头             = {}",
        q("SELECT COUNT(*) FROM assets WHERE description LIKE '>%' OR description LIKE '|%'")
    );
    println!(
        "name = 'string' 条数                   = {}",
        q("SELECT COUNT(*) FROM assets WHERE name = 'string'")
    );
    println!("parser_rev 分布：");
    {
        let mut st = conn
            .prepare("SELECT parser_rev, COUNT(*) FROM assets GROUP BY parser_rev ORDER BY parser_rev")
            .expect("parser_rev 查询");
        let rows = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .expect("parser_rev 读行");
        for row in rows {
            let (rev, n) = row.expect("parser_rev 行");
            println!("  parser_rev={rev:<3} {n}");
        }
    }
    println!();

    println!("── 缺陷 C：去重候选队列 ──");
    let cands = reg.dedup_candidates().unwrap_or_default();
    let l1 = cands
        .iter()
        .filter(|c| c.match_level == ac_core::MatchLevel::L1)
        .count();
    let l2 = cands
        .iter()
        .filter(|c| c.match_level == ac_core::MatchLevel::L2)
        .count();
    println!("dedup_candidates() 返回组数 = {}（L1 {l1} + L2 {l2}）", cands.len());

    // 同一条库上把**修法之前**的 L2 公式也算一遍。
    // 不做这一步就只能引用别人的数字，而"修前 54"正是本缺陷的全部论据。
    // 旧公式 = 只判 COUNT(DISTINCT location_id) > 1，不排除同一 logical_id 的多位置。
    let l1_groups = q(
        "SELECT COUNT(*) FROM (SELECT logical_id FROM asset_locations GROUP BY logical_id HAVING COUNT(*) > 1)",
    );
    let l2_old = q(
        "SELECT COUNT(*) FROM (SELECT g.normalized_hash FROM asset_locations l JOIN logical_assets g ON g.logical_id = l.logical_id WHERE g.normalized_hash IS NOT NULL AND g.normalized_hash <> '' GROUP BY g.normalized_hash HAVING COUNT(DISTINCT l.location_id) > 1)",
    );
    let l2_new = q(
        "SELECT COUNT(*) FROM (SELECT g.normalized_hash FROM asset_locations l JOIN logical_assets g ON g.logical_id = l.logical_id WHERE g.normalized_hash IS NOT NULL AND g.normalized_hash <> '' GROUP BY g.normalized_hash HAVING COUNT(DISTINCT l.location_id) > 1 AND COUNT(DISTINCT l.logical_id) > 1)",
    );
    println!("L1 真实重复组（SQL 直算）        = {l1_groups}");
    println!("L2 修前公式（仅 DISTINCT 位置）  = {l2_old}");
    println!("L2 修后公式（再要求跨 logical_id）= {l2_new}");
    println!("合计 修前 L1+L2 = {} / 修后 L1+L2 = {}", l1_groups + l2_old, l1_groups + l2_new);
    println!(
        "L2 跨 logical_id 的组（应为0）    = {}",
        q("SELECT COUNT(*) FROM (SELECT g.normalized_hash FROM asset_locations l JOIN logical_assets g ON g.logical_id = l.logical_id WHERE g.normalized_hash IS NOT NULL AND g.normalized_hash <> '' GROUP BY g.normalized_hash HAVING COUNT(DISTINCT l.logical_id) > 1)")
    );
    println!();

    println!("── 缺陷 A：来源 / 注册方式四列 ──");
    let cols: Vec<String> = {
        let mut st = conn.prepare("PRAGMA table_info(assets)").expect("table_info");
        let rows = st.query_map([], |r| r.get::<_, String>(1)).expect("列名");
        rows.filter_map(|r| r.ok()).collect()
    };
    let has4 = ["origin_kind", "registration_kind", "origin_agent", "self_contained"]
        .iter()
        .all(|c| cols.iter().any(|x| x == c));
    if !has4 {
        println!("四列尚不存在（这是修前状态）");
        println!("当前 assets 列: {cols:?}");
    } else {
        print_dist(&conn, "origin_kind");
        print_dist(&conn, "registration_kind");
        print_dist(&conn, "origin_agent");
        print_dist(&conn, "self_contained");
        println!(
            "自构建（origin_kind = 'self_built'） = {}",
            q("SELECT COUNT(*) FROM assets WHERE origin_kind = 'self_built'")
        );
    }
    println!();

    // 顺带把列表查询跑一遍：四列加进来之后 UI 那条路径不能坏。
    let items = reg
        .query_assets(&AssetQuery {
            limit: 5,
            ..Default::default()
        })
        .unwrap_or_default();
    println!("── 列表查询（UI 路径）──");
    println!("返回 {} 条", items.len());
    for it in items.iter().take(3) {
        println!(
            "  {} | origin={:?} reg={:?} agent={:?} self_contained={:?}",
            it.id, it.origin_kind, it.registration_kind, it.origin_agent, it.self_contained
        );
    }
}

fn print_dist(conn: &Connection, col: &str) {
    // 四列里 self_contained 是 INTEGER，其余是 TEXT：统一 CAST 成 TEXT 再读，
    // 否则 rusqlite 会按请求的类型取列而报 InvalidColumnType。
    let sql = format!(
        "SELECT CAST(COALESCE({col}, '<null>') AS TEXT), COUNT(*) FROM assets GROUP BY {col} ORDER BY COUNT(*) DESC"
    );
    let mut st = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            println!("{col}: 查询失败 {e}");
            return;
        }
    };
    let rows = match st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
        Ok(r) => r,
        Err(e) => {
            println!("{col}: 读行失败 {e}");
            return;
        }
    };
    let mut parts: Vec<String> = Vec::new();
    for row in rows.flatten() {
        parts.push(format!("{}={}", row.0, row.1));
    }
    println!("{col:<18}= {}", parts.join(", "));
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
