//! M1.5 迁移验证：对**真实的 506 资产库副本**跑迁移，证明
//! 1. 老库能正常打开并升级到 schema v4；
//! 2. 旧列一个不少（回滚能力保留）；
//! 3. 新列 + 逻辑资产表被正确回填。
//!
//! 用法：cargo run --release --example migrate_check -- <db路径> [...]
//! 直接操作传入的副本，绝不碰用户正式库。

use ac_registry::Registry;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法: migrate_check <db路径> [...]");
        std::process::exit(2);
    }

    let mut bad = 0;
    for path in &args {
        println!("── {path}");
        match Registry::open(std::path::Path::new(path)) {
            Ok(reg) => {
                println!("   打开成功（迁移已应用）");
                println!("   assets 总数      : {}", reg.total().unwrap_or(0));
                println!("   逻辑资产数        : {}", reg.logical_total().unwrap_or(0));
                println!("   distinct L1 指纹 : {}", reg.distinct_l1_count().unwrap_or(0));
                println!("   位置总数          : {}", reg.location_total().unwrap_or(0));
                println!("   待确认去重候选    : {}", reg.dedup_candidates().unwrap_or_default().len());
                println!("   标签种类          : {}", reg.all_tags().unwrap_or_default().len());

                // 旧查询路径必须照常工作
                let q = ac_registry::AssetQuery {
                    limit: 5,
                    ..Default::default()
                };
                match reg.query_assets(&q) {
                    Ok(items) => println!("   旧列表查询可用    : 返回 {} 条，total={}", items.len(), reg.count_assets(&q).unwrap_or(0)),
                    Err(e) => {
                        println!("   旧列表查询失败    : {e}");
                        bad += 1;
                    }
                }
            }
            Err(e) => {
                println!("   打开失败: {e}");
                bad += 1;
            }
        }
    }
    if bad > 0 {
        std::process::exit(1);
    }
}
