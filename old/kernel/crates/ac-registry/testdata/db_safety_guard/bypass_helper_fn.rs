//! FIXTURE B —— 守门自检用：**结构不同**的绕行写法。
//!
//! FIXTURE A 只改了变量名。这个连数据流形状都改了：
//!   1. 路径经过一个辅助函数 `resolve()` 才到达 `Registry::open`，
//!      open 那一行根本看不到任何「库路径」的痕迹；
//!   2. 路径由 `PathBuf::from("..").join("data").join("registry.db")` 拼出来，
//!      磁盘上**不存在`data/registry.db` 这个字面量**，所以任何按字面量搜
//!      `data/registry.db` 的规则都会直接漏判；
//!   3. 文件里照常出现 `fs::copy` / `fs::read`，让规则 1 的 copy-first 通过。
//!
//! 守门必须判它红。任何靠「搜字面量」「搜变量名」「看 open 那一行」
//! 的规则都会在这里失效 —— 这正是规则必须与命名无关的原因。

use ac_registry::Registry;
use std::path::{Path, PathBuf};

fn resolve() -> PathBuf {
    PathBuf::from("..").join("data").join("registry.db")
}

fn stage(p: &Path) -> PathBuf {
    let work = std::env::temp_dir().join("fixture-b-helper");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("建工作目录");
    let dst = work.join("registry.db");
    std::fs::copy(p, &dst).expect("复制");
    let _ = std::fs::read(&dst).expect("取证");
    dst
}

fn main() {
    let target = resolve();
    let copied = stage(&target);
    let reg = Registry::open(&copied).expect("打开副本");
    println!("{}", reg.total().unwrap_or(0));
    let live = resolve();
    let reg2 = Registry::open(&live).expect("又打开一次");
    println!("{}", reg2.total().unwrap_or(0));
}
