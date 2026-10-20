//! FIXTURE A —— 守门自检用：一次变量改名就能绕过的红线（真实事故形态）。
//!
//! 这个文件**不是** example，永远不会被 cargo 编译或运行。
//! 它只作为字符串喂给 db_safety_guard::rule2_red_line_violations，
//! 由 `bypass_fixtures_are_rejected_by_rule_2` 断言守门必须判它红。
//!
//! 它与 2026-10-16 事故版本逐行同构，唯一差别是危险变量叫 `live` 而不是
//! `path`。旧的红线规则是一张「字面变量名清单」
//! (`db_path()` / `&path)` / `&real_db)` / `&db)`)，因此这个样本会**静默通过**
//! —— 一次改名就把「被抓到的缺陷」变成「绿灯」，而行为完全没变。
//! 守门自己的测试必须能看见这件事。

use ac_registry::Registry;
use std::path::PathBuf;

fn live_path() -> String {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        return args[1].clone();
    }
    "../data/registry.db".to_string()
}

fn sha256_of(p: &std::path::Path) -> String {
    let bytes = std::fs::read(p).expect("读取文件算 SHA256");
    bytes.len().to_string()
}

fn main() {
    let live = PathBuf::from(live_path());
    let _before = sha256_of(&live);
    let reg = Registry::open(&live).expect("打开");
    println!("{}", reg.total().unwrap_or(0));
}
