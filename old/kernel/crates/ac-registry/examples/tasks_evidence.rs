//! 把任务路由表的**可核验证据**导出成 Markdown。
//!
//! 输出：25 个节点的逐条定义（id/名称/执行者/状态/依赖/交叉/脚本入口）
//! + 六项需求映射 + 全部交叉边。用于交付验收，不参与产品运行。

use ac_registry::tasks;

fn main() {
    let cat = tasks::catalog();
    println!("# 模型分工路由表 · 可核验证据\n");
    println!(
        "总计 {} 个节点：脚本 {} / 混合 {} / 模型 {}；已实现 {} / 部分 {} / 计划 {}；交叉边 {} 条。\n",
        cat.counts.total,
        cat.counts.script,
        cat.counts.hybrid,
        cat.counts.model,
        cat.counts.implemented,
        cat.counts.partial,
        cat.counts.planned,
        { let mut e: Vec<(String,String)> = Vec::new(); for t in &cat.tasks { for c in &t.crosses_with { let (a,b) = if t.id < *c { (t.id.clone(), c.clone()) } else { (c.clone(), t.id.clone()) }; if !e.contains(&(a.clone(), b.clone())) { e.push((a,b)); } } } e.len() }
    );

    // ── 六项需求映射 ──
    println!("## 一、六项用户需求 → 任务节点\n");
    for r in &cat.requirements {
        println!("### {} {}\n", r.id, r.title);
        println!("> 用户原话：{}\n", r.statement);
        println!("归属节点（{} 个）：", r.task_ids.len());
        for id in &r.task_ids {
            if let Some(t) = cat.tasks.iter().find(|t| &t.id == id) {
                println!(
                    "- `{}` {} · {} · {}",
                    t.id,
                    t.display_name,
                    t.executor.as_str(),
                    t.status.as_str()
                );
            }
        }
        println!("\n落点说明：{}\n", r.note);
    }

    // ── 25 节点总表 ──
    println!("## 二、25 个任务节点逐条定义\n");
    println!("| id | 名称 | 执行者 | 状态 | 依赖 | 交叉数 | 脚本入口数 |");
    println!("|---|---|---|---|---|---|---|");
    for t in &cat.tasks {
        println!(
            "| `{}` | {} | {} | {} | {} | {} | {} |",
            t.id,
            t.display_name,
            t.executor.as_str(),
            t.status.as_str(),
            if t.depends_on.is_empty() {
                "—".to_string()
            } else {
                t.depends_on.join(", ")
            },
            t.crosses_with.len(),
            t.script_entry.len()
        );
    }

    // ── 脚本型：真实函数 ──
    println!("\n## 三、脚本型节点 → 内核真实函数\n");
    for t in cat.tasks.iter().filter(|t| t.executor.as_str() == "script") {
        println!("### `{}` {}", t.id, t.display_name);
        println!("- 实现：{}", t.implementation);
        if !t.script_entry.is_empty() {
            println!("- 函数：");
            for f in &t.script_entry {
                println!("  - `{}`", f);
            }
        }
        println!();
    }

    // ── 模型型：契约 ──
    println!("\n## 四、模型型节点 → provider 契约\n");
    let model_like: Vec<_> = cat
        .tasks
        .iter()
        .filter(|t| t.executor.as_str() != "script")
        .collect();
    if model_like.is_empty() {
        println!("（无）\n");
    }
    for t in model_like {
        println!("### `{}` {} · {} · {}", t.id, t.display_name, t.executor.as_str(), t.status.as_str());
        println!("- 实现：{}", t.implementation);
        match &t.model_contract {
            Some(c) => {
                println!("- 需要能力：{}", c.capabilities.join(", "));
                println!("- 输入：{}", c.input_schema);
                println!("- 输出：{}", c.output_schema);
                println!("- 为什么脚本做不了：{}", c.why_not_script);
            }
            None => println!("- （无模型契约）"),
        }
        println!();
    }

    // ── 交叉边 ──
    println!("\n## 五、全部交叉关系\n");
    let mut edges: Vec<(String, String)> = Vec::new();
    for t in &cat.tasks {
        for c in &t.crosses_with {
            let (a, b) = if t.id < *c {
                (t.id.clone(), c.clone())
            } else {
                (c.clone(), t.id.clone())
            };
            if !edges.contains(&(a.clone(), b.clone())) {
                edges.push((a, b));
            }
        }
    }
    edges.sort();
    println!("共 {} 条无向交叉边：\n", edges.len());
    for (a, b) in &edges {
        println!("- `{}` ↔ `{}`", a, b);
    }

    println!("\n## 六、说明\n");
    println!("{}", cat.note);
}
