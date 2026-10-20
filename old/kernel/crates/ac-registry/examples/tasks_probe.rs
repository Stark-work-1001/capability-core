//! 任务路由表探针：打印 `ac_registry::tasks::catalog()` 的完整 JSON。
//!
//! # 为什么需要它
//!
//! 这张表是**静态目录**：不读库、不碰磁盘、不调模型。也正因为如此，
//! 它正确的验证方式不是「跑一遍看有没有崩」，而是「把它的产物原样打出来，
//! 逐条核对」——包括每个节点的执行者、状态、依赖、交叉与判定证据。
//!
//! 前端 RoutingPanel 消费的是同一个 TaskCatalog 结构（经 Tauri 命令
//! list_capability_tasks 跨 IPC），所以这份输出同时就是 IPC 契约的样本。
//!
//! # 边界
//!
//! 本探针**只打印**，不写任何文件、不打开任何数据库。
//! 因此它不需要 --db，也不会被 db_safety_guard 的「兜底到用户库」红线盯上；
//! 登记进 WATCHED 是为了让 examples/ 与守门清单保持一致
//! （every_example_is_registered_in_the_guard 会核对这份一致性）。
//!
//! 用法：
//! `text
//! cargo run --release --example tasks_probe            # 打印完整 JSON
//! cargo run --release --example tasks_probe -- --brief # 只打印计数摘要
//! `

fn main() {
    let brief = std::env::args().any(|a| a == "--brief");
    let catalog = ac_registry::tasks::catalog();

    if brief {
        let c = &catalog.counts;
        println!("任务节点总计 {}", c.total);
        println!(
            "执行者：脚本 {} / 混合 {} / 模型 {}",
            c.script, c.hybrid, c.model
        );
        println!(
            "状态：已实现 {} / 部分 {} / 计划 {}",
            c.implemented, c.partial, c.planned
        );
        println!("脚本型节点占比 {:.1}%", catalog.script_share * 100.0);
        println!("需求映射 {} 条", catalog.requirements.len());
        for t in &catalog.tasks {
            println!(
                "  {:<6} {:<11} {:<34} {}",
                t.executor.as_str(),
                t.status.as_str(),
                t.id,
                t.display_name
            );
        }
        // 交叉关系逐条打印：用户点名要「不止一条线，还有一些交叉的项」，
        // 这里给出的是这份声明的实际条数，而不是一句形容。
        let mut edges: Vec<(String, String)> = Vec::new();
        for t in &catalog.tasks {
            for other in &t.crosses_with {
                let (a, b) = if t.id < *other {
                    (t.id.clone(), other.clone())
                } else {
                    (other.clone(), t.id.clone())
                };
                if !edges.iter().any(|(x, y)| *x == a && *y == b) {
                    edges.push((a, b));
                }
            }
        }
        edges.sort();
        println!("无向交叉关系 {} 条（对称声明去重后）", edges.len());
        for (a, b) in &edges {
            println!("  {a} <-> {b}");
        }
        return;
    }

    match serde_json::to_string_pretty(&catalog) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("序列化失败: {e}");
            std::process::exit(1);
        }
    }
}
