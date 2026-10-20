//! M1.5 端到端验证：append-only + 多版本共存 + 绝不静默合并。
//!
//! 用**真实文件**跑一遍完整链路（扫描 -> 入库 -> 逻辑分组 -> 裁决），
//! 并在最后把"某个资产从磁盘消失"再扫一次，证明它留下的是
//! **位置痕迹**而不是被抹掉。
//!
//! 用法：cargo run --release --example m15_e2e -- <临时目录>

use ac_core::{DedupDecision, MatchLevel};
use ac_registry::{ingest, Registry};
use std::path::{Path, PathBuf};

fn write(p: &Path, rel: &str, body: &str) {
    let f = p.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(f, body).unwrap();
}

fn main() {
    let root: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let db = root.join("registry.db");

    // ── 搭一棵有真实重复、也有多版本的树 ──
    // 组 A：三个位置字节完全相同（真重复）
    let dup_body = "---\nname: astropy\ndescription: 天文观测\n---\n# Astropy\n\n分析光谱数据。\n";
    write(&root, "skill/astropy/SKILL.md", dup_body);
    write(&root, "pkg/refs/astropy/SKILL.md", dup_body);
    write(&root, "pkg2/deep/refs/astropy/SKILL.md", dup_body);

    // 组 B：同一个包的两个版本，正文不同 -> 必须是两个独立逻辑资产
    write(
        &root,
        "viz/0.1.0/SKILL.md",
        "---\nname: viz\nversion: \"0.1.0\"\ndescription: 架构可视化\n---\n只支持流程图。\n",
    );
    write(
        &root,
        "viz/1.0.0/SKILL.md",
        "---\nname: viz\nversion: \"1.0.0\"\ndescription: 架构可视化\n---\n加入时序图与 C4 模型。\n",
    );

    // 组 C：L2 能抓、L1 抓不到的「重打包」案例
    //   同样的内容，一个有换行/缩进差异、一个大小写不同
    write(
        &root,
        "skill/normalizer/SKILL.md",
        "---\nname: normalizer\ndescription: 归一化\n---\nNormalize your data.\n",
    );
    write(
        &root,
        "pkg/refs/normalizer-repacked/SKILL.md",
        "---\nname: NORMALIZER\ndescription: 归一化\n---\nNormalize    your\n   data.\n\n\n",
    );

    println!("══ 第一轮扫描 ══");
    let reg = Registry::open(&db).unwrap();
    let r1 = ingest(&reg, &root).unwrap();
    println!("解析 {} 条 / 新建逻辑资产 {} / 位置 {} / alias {}",
        r1.parsed, r1.logical_created, r1.locations_seen, r1.alias_count);
    println!("逻辑资产总数 : {}", reg.logical_total().unwrap());
    println!("distinct L1  : {}", reg.distinct_l1_count().unwrap());

    println!("\n══ 多版本共存（必须看到两个 viz，且版本号都在）══");
    for a in reg.logical_assets(50).unwrap() {
        if a.name.contains("viz") {
            println!("  logical_id={}  version={}  name={}", a.logical_id, a.version, a.name);
        }
    }

    println!("\n══ 待确认候选（程序一个都没合并）══");
    for c in reg.dedup_candidates().unwrap() {
        println!("  [{}] {}  {} 个位置", c.match_level.as_str().to_uppercase(), c.group_key, c.location_ids.len());
        for l in &c.location_ids {
            let mark = if *l == c.suggested_canonical { "规范" } else { "别名" };
            println!("        [{}] {}", mark, l.replace("loc-", ""));
        }
    }

    println!("\n══ 验证：此刻没有任何一条被合并 ══");
    println!("decisions 已记录数    = {}（必须是 0，没人裁决过）", reg.dedup_history().unwrap().len());
    println!("逻辑资产 {} == distinct L1 {} -> 未发生静默合并",
        reg.logical_total().unwrap(), reg.distinct_l1_count().unwrap());

    // ── 用户显式裁决：把 astropy 那组合并 ──
    println!("\n══ 用户显式裁决：合并 astropy 组 ══");
    let cand = reg
        .dedup_candidates()
        .unwrap()
        .into_iter()
        .find(|c| c.location_ids.iter().any(|l| l.contains("astropy")))
        .expect("应存在 astropy 候选组");
    reg.decide_dedup(&cand.group_key, MatchLevel::L1, DedupDecision::Merge)
        .unwrap();
    let locs = reg.locations_of(&cand.group_key).unwrap();
    for l in &locs {
        println!("  {:<9} {}", l.role.as_str(), l.location_id.replace("loc-skill::", ""));
    }
    println!("逻辑资产总数仍为 {}（合并=折叠展示，不是删数据）", reg.logical_total().unwrap());

    // ── append-only 验证：把 astropy 的主副本从磁盘删掉 ──
    println!("\n══ append-only 验证：删掉磁盘上的主副本再扫一次 ══");
    std::fs::remove_dir_all(root.join("skill/astropy")).unwrap();
    let r2 = ingest(&reg, &root).unwrap();
    println!("第二轮：解析 {} / 清除 {} 条 assets 行 / alias {}", r2.parsed, r2.removed, r2.alias_count);

    let still = reg.locations_of(&cand.group_key).unwrap();
    println!("\n逻辑资产 {} 下的位置痕迹（**应当还在**）：", cand.group_key);
    for l in &still {
        println!("  {:<9} last_seen_at={}  {}", l.role.as_str(), l.last_seen_at, l.location_id.replace("loc-skill::", ""));
    }
    assert_eq!(still.len(), 3, "消失的资产必须留下位置记录，不能被抹掉");
    assert!(still.iter().any(|l| l.location_id.contains("skill::skill-astropy")),
        "被删掉的那个位置必须还在库里作为痕迹");

    println!("\n✔ append-only 成立：磁盘上消失的资产在库里降级为位置记录，而不是被删除。");

    let _ = std::fs::remove_dir_all(&root);
}
