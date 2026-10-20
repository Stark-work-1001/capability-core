//! 三槽位「测试连接」的**命令行等价物**。
//!
//! 它调用的 `ac_llm::slots::probe_all` 就是界面按钮 `test_model_slot`
//! 转发到的**同一个函数**（见 apps/desktop/src-tauri/src/lib.rs 的 ops）。
//!
//! 也就是说：这个探针跑通 = 界面按钮走通。
//! 验收时不需要 UI 自动化——那在 WebView2 上本来就取不到 UIA 树。
//!
//! ```text
//! cargo run --release -p ac-llm --example slot_probe -- <data_dir>
//! ```

use ac_llm::slots::probe_all;

fn main() {
    let data_dir = match std::env::args().nth(1) {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            eprintln!("用法: slot_probe <data_dir>");
            std::process::exit(2);
        }
    };
    println!("配置目录: {}", data_dir.display());
    println!();

    let results = probe_all(&data_dir);
    let mut bad = 0usize;
    let mut configured = 0usize;

    for (slot, r) in &results {
        let name = slot.as_str();
        if r.elapsed_ms == 0 && !r.ok && r.error.contains("还没配") {
            println!("[{name}] 未启用/未配全（不是失败，是还没配）");
            continue;
        }
        configured += 1;
        if r.ok {
            println!("[{name}] OK  {}ms  {}", r.elapsed_ms, r.summary);
        } else {
            println!("[{name}] FAIL  {}ms  {}", r.elapsed_ms, r.error);
            bad += 1;
        }
    }

    println!();
    if configured == 0 {
        println!("三个槽位都没配置 —— 这不是通过，是空跑。");
        std::process::exit(1);
    }
    if bad == 0 {
        println!("{configured} 个已配置槽位全部通过");
    } else {
        println!("{bad} 个槽位失败");
        std::process::exit(1);
    }
}
