//! FIXTURE C —— 守门自检用：**内联 join 拼接**，路径不经任何具名变量。
//!
//! 与 A（只改名）、B（藏进辅助函数）都不同：这个例程里根本
//! **没有**「库路径变量」这个东西 —— 路径是 Registry::open 实参位置上的
//! 一整条表达式：
//!
//!     Registry::open(std::env::temp_dir().join("..").join("data").join("registry.db"))
//!
//! 它含有 temp_dir()，但 temp_dir 是**参照点**而不是落点：从 temp 目录
//! 往回 join ".."/"data" 会落到磁盘上真实的 data/registry.db。
//! 所以「只要表达式里出现 temp_dir 就算安全」是错的 —— 守门必须看
//! **路径的终点**而不是它引用过哪些词元。
//!
//! 这一条同时防住「把 temp_dir 当万能免罪符」这种反向绕过。

use ac_registry::Registry;

fn main() {
    let work = std::env::temp_dir().join("fixture-c");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("建工作目录");
    let copied = work.join("registry.db");
    let _ = std::fs::read(&copied);
    let reg = Registry::open(&copied).expect("打开副本");
    println!("{}", reg.total().unwrap_or(0));

    // 反向绕过：从 temp_dir 反向 join 回真实用户库
    let live = std::env::temp_dir()
        .join("..")
        .join("data")
        .join("registry.db");
    let reg2 = Registry::open(&live).expect("又打开一次");
    println!("{}", reg2.total().unwrap_or(0));
}
