//! 跑一次真实的故障巡检，把结果落库，并打印成可核对的 JSON。
//!
//! 用法（**必须显式给 --db，绝不默认指向用户的 data/registry.db**）：
//!
//!     cargo run --release --example m4_sweep -- --db <副本路径> --root <项目根>
//!
//! 为什么必须显式给 --db：db_safety_guard 里记着那次事故——
//! 一个 example 默认打开了用户的 registry.db，而 Registry::open 会迁移、
//! 会重写 SQLite 页，于是用户的库变成「字节不同、内容完好」。
//! 这里把默认值**留空并报错**，从源头上不给那条路径第二次机会。

use ac_registry::asset_store::AssetRoot;
use ac_registry::locator::{real_probes, Locator};
use ac_registry::sweep::{mcp_targets, SweepTarget};
use ac_registry::Registry;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let db = arg(&args, "--db");
    let Some(db) = db else {
        eprintln!("必须显式给 --db <副本路径>：本工具绝不默认打开用户的 data/registry.db");
        std::process::exit(2);
    };
    let root = arg(&args, "--root").unwrap_or_else(|| ".".to_string());
    let json_only = args.iter().any(|a| a == "--json");
    let reg = match Registry::open(&PathBuf::from(&db)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("打开数据库失败: {e}");
            std::process::exit(1);
        }
    };
    // 数据库里的 source_path 是相对扫描根的（形如 ..\\vendor\\mcp\\x\\.mcp.json），
    // 落盘位置是 <root>/vendor/...。两种写法都要能解析到。
    let base = PathBuf::from(&root);
    let base_abs = base.canonicalize().unwrap_or_else(|_| base.clone());
    let resolve = |rel: &str| -> PathBuf {
        let p = PathBuf::from(rel);
        if p.is_absolute() {
            return p;
        }
        // 先按 root 直接拼（去掉前导 ..），不行再按 root 的父目录拼
        let cleaned = rel.trim_start_matches("..").trim_start_matches(['/', '\\']);
        let a = base_abs.join(cleaned);
        if a.exists() {
            return a;
        }
        base_abs.join(rel)
    };
    let targets: Vec<SweepTarget> = match mcp_targets(&reg) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("取资产清单失败: {e}");
            std::process::exit(1);
        }
    };
    // 搬家表：**由调用方给**，中枢不自己推断（见 locator::RootRemap）。
    // 格式是 --remap <旧根> <新根>，可重复。
    let mut remaps: Vec<ac_registry::locator::RootRemap> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--remap" && i + 2 < args.len() {
            remaps.push(ac_registry::locator::RootRemap::new(
                args[i + 1].clone(),
                args[i + 2].clone(),
            ));
            i += 3;
            continue;
        }
        i += 1;
    }
    let (fs, path, env) = real_probes();
    let rep = match ac_registry::sweep::sweep_with_remaps(
        &reg,
        &fs,
        &path,
        &env,
        &targets,
        resolve,
        remaps.clone(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("巡检失败: {e}");
            std::process::exit(1);
        }
    };
    // 顺带把纳管目录骨架建出来（幂等）。
    let assets = AssetRoot::from_project(&base_abs);
    let layout = assets.init_layout();
    if !json_only {
        eprintln!("[m4_sweep] 目标 {} 个，故障 {} 个，健康 {} 个",
                  targets.len(), rep.locate.failed, rep.locate.healthy);
        match &layout {
            Ok(l) => eprintln!("[m4_sweep] assets 骨架: 新建 {:?}，已存在 {:?}", l.created, l.existing),
            Err(e) => eprintln!("[m4_sweep] assets 骨架建立失败: {e}"),
        }
    }
    let out = serde_json::json!({
        "targets": targets,
        "total_units": rep.locate.total_units,
        "healthy": rep.locate.healthy,
        "failed": rep.locate.failed,
        "by_class": rep.locate.by_class,
        "by_any_class": rep.locate.by_any_class,
        "assets": rep.locate.asset_rollup(),
        "reports": rep.locate.reports,
        "stored": rep.stored,
        "cleared": rep.cleared,
        "unreadable": rep.unreadable,
        "assets": layout.map(|l| serde_json::json!({
            "root": l.root, "created": l.created, "existing": l.existing,
        })).unwrap_or(serde_json::Value::Null),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    let _ = Locator::new(&fs, &path, &env);
}

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}