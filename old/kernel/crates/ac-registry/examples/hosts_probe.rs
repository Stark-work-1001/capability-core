//! v8 后端能力实测探针：智能体协议注册表 / 资产生命周期 / 作者 / 资产↔宿主支持。
//!
//! 用法（**必须显式给 --db 与 --data-dir**）：
//!
//!     cargo run --release -p ac-registry --example hosts_probe -- \
//!         --db <registry.db 副本的绝对路径> --data-dir <可写的临时目录>
//!
//! # 为什么必须显式给
//!
//! Registry::open 会执行迁移（写操作），而 db_safety_guard.rs 记着那次事故。
//! 本例程已登记进 WATCHED，写法与 data_probe / diag_probe 保持一致：
//! 换个写法（let-else 直接接 arg）就追不到命令行实参，会被判红。
//!
//! --data-dir 同样必须显式：seed_defaults 会往 `<data-dir>/hosts/` 写清单文件。
//! 不给它默认值，是为了让"会写哪些文件"这件事在命令行上可见。
//!
//! # 它量什么
//!
//! 1. 迁移后 schema 版本与既有表行数（证明确实没删行）；
//! 2. agent_hosts 行数 / detected / connected / 协议条数与分档；
//! 3. assets 的 lifecycle 分布、author 非空条数、author_source 分布；
//! 4. host_support 抽样（三种 kind 各一条）+ 判据。

use ac_registry::hosts::{self, RealEnv};
use ac_registry::{AssetQuery, Registry};
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let db = arg(&args, "--db");
    let Some(db) = db else {
        eprintln!("必须显式给 --db <副本路径>：本工具绝不默认打开用户的 registry.db");
        std::process::exit(2);
    };
    let data_dir = arg(&args, "--data-dir");
    let Some(data_dir) = data_dir else {
        eprintln!("必须显式给 --data-dir <可写目录>：seed_defaults 会往 <data-dir>/hosts/ 写清单");
        std::process::exit(2);
    };

    let db_path = PathBuf::from(&db);
    let data_path = PathBuf::from(&data_dir);

    println!("库       : {db}");
    println!("数据目录 : {data_dir}");
    println!();

    // ── 迁移前快照（用只读连接，不开 Registry）────────────────────
    let before = {
        let conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("只读打开副本");
        snapshot(&conn)
    };
    println!("── 迁移前（只读连接量到）──");
    println!("user_version = {}", before.0);
    println!(
        "assets={} capabilities={} logical_assets={} asset_locations={}",
        before.1, before.2, before.3, before.4
    );
    println!();

    let reg = match Registry::open(Path::new(&db)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("打不开库 {db}: {e}");
            std::process::exit(1);
        }
    };
    // 原始 SQL 走**另一条只读连接**：Registry::conn 是 pub(crate)，
    // 刻意不扩大对外 API（那会让 example 与内部实现耦合）。
    // 第二条连接以 mode=ro 打开，不参与迁移、不改任何页。
    let ro = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("只读连接（原始 SQL 统计用）");

    println!("── 迁移后 ──");
    println!(
        "代码 SCHEMA_VERSION = {}",
        ac_registry::store::SCHEMA_VERSION
    );
    println!("代码 PARSER_REV     = {}", ac_registry::store::PARSER_REV);
    println!(
        "assets={} 能力={} 逻辑资产={} 位置={}",
        reg.total().unwrap_or(0),
        scalar(&ro, "SELECT COUNT(*) FROM capabilities"),
        reg.logical_total().unwrap_or(0),
        reg.location_total().unwrap_or(0)
    );
    println!(
        "行数变化: assets {:+} / capabilities {:+} / logical {:+} / locations {:+}",
        reg.total().unwrap_or(0) as i64 - before.1,
        scalar(&ro, "SELECT COUNT(*) FROM capabilities") - before.2,
        reg.logical_total().unwrap_or(0) as i64 - before.3,
        reg.location_total().unwrap_or(0) as i64 - before.4,
    );
    println!();

    // ── A. 智能体协议注册表 ──────────────────────────────────────
    println!("── A. 智能体协议注册表 ──");
    match reg.sync_hosts(&data_path, &RealEnv::default()) {
        Ok(rep) => {
            println!("清单目录            : {}", rep.seed.dir);
            println!("本次新写清单文件    : {}", rep.seed.created.len());
            println!("既有未覆盖清单文件  : {}", rep.seed.existing.len());
            println!("读到的宿主数        : {}", rep.specs_loaded);
            println!("写入 agent_hosts 行数: {}", rep.rows_written);
            println!("detected 数         : {}", rep.detected);
            println!("connected 数        : {}", rep.connected);
            println!("读不懂的清单        : {}", rep.rejected.len());
            for r in &rep.rejected {
                println!("  ! {} -> {}", r.file, r.reason);
            }
            if !rep.orphans.is_empty() {
                println!("库里有但清单已无    : {:?}", rep.orphans);
            }
        }
        Err(e) => {
            eprintln!("sync_hosts 失败: {e}");
            std::process::exit(1);
        }
    }
    let rows = reg.list_agent_hosts().unwrap_or_default();
    println!("库内 agent_hosts 行数: {}", rows.len());
    println!(
        "  其中 detected={} connected={}",
        rows.iter().filter(|h| h.detected).count(),
        rows.iter().filter(|h| h.connected).count()
    );
    println!(
        "{:<18} {:<10} {:<8} {:<8} {:<10} {:<4} {}",
        "id", "kind", "detected", "connect", "last_seen", "协议", "display_name"
    );
    for h in &rows {
        println!(
            "{:<18} {:<10} {:<8} {:<8} {:<10} {:<4} {}",
            h.id,
            h.kind,
            h.detected,
            h.connected,
            h.last_seen,
            h.protocols.len(),
            h.display_name
        );
    }
    if let Ok(ov) = reg.hosts_overview(&data_path) {
        println!(
            "总览: total={} detected={} connected={} protocol_rows={}",
            ov.total, ov.detected, ov.connected, ov.protocol_rows
        );
        let fmt = |v: &[hosts::HostCount]| {
            v.iter()
                .map(|c| format!("{}={}", c.key, c.count))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!("按 support 分档: {}", fmt(&ov.by_support));
        println!("按 kind   分档: {}", fmt(&ov.by_kind));
    }
    for id in ["deepseek-harness", "codex", "qoder"] {
        if let Ok(Some(h)) = reg.get_agent_host(id) {
            println!("证据 · {}:", h.id);
            for e in h.evidence.iter().take(4) {
                println!("    {e}");
            }
        }
    }
    println!();

    // ── B. 生命周期 + 作者 ──────────────────────────────────────
    println!("── B. 资产生命周期 / 作者 ──");
    print_dist(&ro, "lifecycle");
    let total = reg.total().unwrap_or(0);
    let author_nonempty = scalar(
        &ro,
        "SELECT COUNT(*) FROM assets WHERE TRIM(COALESCE(author, '')) <> ''",
    );
    println!(
        "author 非空条数     : {} / {}（{:.1}%）",
        author_nonempty,
        total,
        if total == 0 {
            0.0
        } else {
            author_nonempty as f64 * 100.0 / total as f64
        }
    );
    print_dist(&ro, "author_source");
    if let Ok(counts) = reg.lifecycle_counts() {
        println!(
            "lifecycle_counts(): {}",
            counts
                .iter()
                .map(|k| format!("{}={}", k.kind, k.count))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!("作者抽样（每档最多 3 条）:");
    for src in ["manifest", "frontmatter", "package", ""] {
        let items: Vec<(String, String)> = {
            let mut st = ro
                .prepare(
                    "SELECT id, author FROM assets
                     WHERE COALESCE(author_source,'') = ?1 AND TRIM(COALESCE(author,'')) <> ''
                     ORDER BY id LIMIT 3",
                )
                .expect("作者抽样查询");
            let it = st
                .query_map(rusqlite::params![src], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .expect("作者抽样读行");
            it.flatten().collect()
        };
        let label = if src.is_empty() { "(空)" } else { src };
        if items.is_empty() {
            println!("  {label:<12}: 无");
        } else {
            for (id, a) in items {
                println!("  {label:<12}: {a}  ←  {id}");
            }
        }
    }
    println!();

    // ── C. 资产 ↔ 宿主支持 ──────────────────────────────────────
    println!("── C. 资产 ↔ 宿主支持 ──");
    let mut sampled = 0;
    for kind in ["skill", "plugin", "mcp"] {
        let items = reg
            .query_assets(&AssetQuery {
                kind: Some(kind.to_string()),
                limit: 1,
                ..Default::default()
            })
            .unwrap_or_default();
        let Some(item) = items.first() else {
            println!("{kind}: 库里没有这种资产");
            continue;
        };
        sampled += 1;
        println!(
            "{kind} · {}\n  {} 个宿主支持: {:?}",
            item.id,
            item.host_support.len(),
            item.host_support
        );
        for b in item.host_support_basis.iter() {
            println!("    {b}");
        }
        match reg.asset_host_support(&item.id) {
            Ok(Some(v)) => println!("  asset_host_support() -> {:?}", v),
            Ok(None) => println!("  asset_host_support() -> None（资产不存在）"),
            Err(e) => println!("  asset_host_support() 失败: {e}"),
        }
    }
    // 全库统计：有多少资产能判出至少一个宿主
    let all = reg
        .query_assets(&AssetQuery {
            include_archived: true,
            limit: 1000,
            ..Default::default()
        })
        .unwrap_or_default();
    let with_support = all.iter().filter(|a| !a.host_support.is_empty()).count();
    println!(
        "全库 {} 条资产中，能判出宿主支持的有 {} 条（{} 条判不出来）",
        all.len(),
        with_support,
        all.len() - with_support
    );
    println!("抽样条数: {sampled}");
}

fn snapshot(conn: &rusqlite::Connection) -> (i64, i64, i64, i64, i64) {
    let one = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1) };
    (
        one("PRAGMA user_version"),
        one("SELECT COUNT(*) FROM assets"),
        one("SELECT COUNT(*) FROM capabilities"),
        one("SELECT COUNT(*) FROM logical_assets"),
        one("SELECT COUNT(*) FROM asset_locations"),
    )
}

fn scalar(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
}

fn print_dist(conn: &rusqlite::Connection, col: &str) {
    let sql = format!(
        "SELECT CAST(COALESCE({col}, '<null>') AS TEXT), COUNT(*) FROM assets
         GROUP BY {col} ORDER BY COUNT(*) DESC"
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