//! 打印维修站诊断的向量条目，用于人工核对「是否如实上报」。
//!
//! 用法（**必须显式给 --db，绝不默认指向用户的 data/registry.db**）：
//!
//!     cargo run --release -p ac-registry --example diag_probe -- --db <副本路径>
//!
//! 为什么必须显式给：db_safety_guard 记着那次事故——某个 example
//! 默认打开了用户的 registry.db，而 Registry::open 会迁移、会重写 SQLite 页。
//! 本工具只是**读诊断**，但守门是文件名级的，不给它一个默认路径的机会。
//! 写法刻意与 m4_sweep.rs 保持一致：守门是按**数据流形状**判定的，
//! 换个写法（let-else 直接接 arg）就追不到命令行实参，会被判红。

use ac_registry::diagnostics::{self, DiagContext, LlmStatus, VectorProbe};
use ac_registry::Registry;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let db = arg(&args, "--db");
    let Some(db) = db else {
        eprintln!("必须显式给 --db <副本路径>：本工具绝不默认打开用户的 data/registry.db");
        std::process::exit(2);
    };
    let root = arg(&args, "--root");
    let reg = match Registry::open(Path::new(&db)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("打不开库 {db}: {e}");
            std::process::exit(1);
        }
    };

    // 扫描根：显式 --root 优先；否则取 db 的上一级目录的 vendor/，存在才用。
    let scan_root = root
        .map(PathBuf::from)
        .or_else(|| {
            PathBuf::from(&db)
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.join("vendor"))
        })
        .filter(|p| p.is_dir());

    // 两种形态都打印：未探测 / 真探。这一步是本例程存在的全部意义：
    // 证明「没测过」和「测了但挂了」在报告里是两句不同的话。
    let db_path = PathBuf::from(&db);
    let probes: [(&str, Option<VectorProbe>); 2] = [
        ("未探测（调用方没提供）", None),
        ("真探（probe_from_env）", Some(VectorProbe::probe_from_env())),
    ];
    for (label, probe) in probes {
        let ctx = DiagContext {
            db_path: db_path.clone(),
            scan_root: scan_root.clone(),
            preview_json: None,
            llm: LlmStatus::default(),
            vector: probe,
        };
        let rep = diagnostics::collect(&reg, &ctx);
        let d = rep
            .diagnostics
            .iter()
            .find(|d| d.category.as_str() == "vector_layer");
        match d {
            Some(d) => {
                println!("[{label}]");
                println!("  severity = {}", d.severity);
                println!("  title    = {}", d.title);
                println!("  subject  = {}", d.subject);
                println!("  detail   = {}", d.detail);
            }
            None => println!("[{label}] 没有 vector_layer 诊断（这是缺陷）"),
        }
        println!();
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
