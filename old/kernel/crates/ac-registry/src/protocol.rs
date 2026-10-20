//! M3 产品化：**模型接入**侧栏的协议兼容性检查器。
//!
//! # 这一层要回答的问题
//!
//! 用户原话：「能力核心自带算法驱动（检索、修复），先建多协议支持；
//! 协议测试不通过时，再接云端或本地模型来驱动故障解决。」

//! 所以本模块只做**前半句**：把每项能力按其声明的协议/格式做一次
//! 「能不能被读懂」的检查，产出通过/不通过与原因。
//! 后半句（接模型）由 `ac-llm` 的 provider 抽象承接，
//! **本轮不实现任何具体厂商调用**。

//! # 为什么检查的是「运行时真正在用的那份解析结果」

//! 检查全部基于**库里已存的解析结果 + 磁盘上的源文件是否还在**，
//! 而不是当场重跑解析器再比 id。原因很实际：
//! 入库时给同目录多份清单做过判别后缀（`IdScope::tag`），
//! 那个后缀没有落库，重跑必然对不上 id，于是凭空造出一片假报警。
//! 「解析成功率」报假数比不报更糟。
//!
//! 覆盖面因此落在**协议与内容本身**：协议是否受支持、manifest 是否读得回来、
//! schema 版本是否匹配、源文件是否还在、能力条目字段是否完整、
//! 声明的能力是否真的落进了 `capabilities` 表。
//! 这六条任何一条不成立，中枢就是"读不懂"这项能力。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 当前**受支持**的三种资产协议。解析器只认这三种（见 `parse.rs`）。
pub const SUPPORTED_PROTOCOLS: &[&str] = &["skill_md", "plugin_json", "mcp_json", "managed_manifest", "openapi_json", "a2a_agent_card"];

/// 一条协议支持矩阵的一行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolRow {
    pub protocol: String,
    /// 是否在中枢的解析器支持列表里。
    pub supported: bool,
    pub assets: u32,
    pub capabilities: u32,
    pub passed_assets: u32,
    pub failed_assets: u32,
    /// 能力级：通过 / 不通过。
    pub passed_capabilities: u32,
    pub failed_capabilities: u32,
    /// 成功率（能力级，0.0–1.0）。分母为 0 时是 0.0，
    /// **不是 1.0** —— 没有样本不等于全部通过。
    pub success_rate: f32,
    /// 失败原因 top N（去重后按出现次数倒序）。
    pub top_reasons: Vec<String>,
}

/// 协议支持矩阵。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProtocolMatrix {
    pub rows: Vec<ProtocolRow>,
    pub total_assets: u32,
    pub total_capabilities: u32,
    pub total_passed: u32,
    pub total_failed: u32,
}

/// 单项检查结果（资产级或能力级）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolCheck {
    pub asset_id: String,
    pub asset_name: String,
    /// 能力级检查填能力 id；资产级检查留空串。
    pub capability_id: String,
    pub protocol: String,
    pub supported: bool,
    pub passed: bool,
    /// 不通过的原因。**每条都带证据**（版本号、路径、字段名）。
    pub reasons: Vec<String>,
}

impl ProtocolCheck {
    fn asset_level(asset_id: &str, name: &str, protocol: &str) -> Self {
        Self {
            asset_id: asset_id.to_string(),
            asset_name: name.to_string(),
            capability_id: String::new(),
            protocol: protocol.to_string(),
            supported: true,
            passed: true,
            reasons: Vec::new(),
        }
    }
}

/// 一个资产在库里的原始行（检查用）。
struct AssetRow {
    id: String,
    name: String,
    source_format: String,
    source_path: String,
    manifest: String,
}

/// 协议是否在支持列表里。**未知协议必须显式判否**，
/// 不能默认通过——库里真出现过 source_format 为空/未知的历史行。
pub fn is_supported(protocol: &str) -> bool {
    SUPPORTED_PROTOCOLS.contains(&protocol)
}

fn load_assets(reg: &crate::store::Registry) -> Result<Vec<AssetRow>, String> {
    let mut stmt = reg
        .conn
        .prepare("SELECT id, name, COALESCE(source_format, ''), COALESCE(source_path, ''), manifest FROM assets ORDER BY id")
        .map_err(|e| e.to_string())?;
    let it = stmt
        .query_map([], |r| {
            Ok(AssetRow {
                id: r.get(0)?,
                name: r.get(1)?,
                source_format: r.get(2)?,
                source_path: r.get(3)?,
                manifest: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in it {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn persisted_capability_count(reg: &crate::store::Registry, asset_id: &str) -> Result<i64, String> {
    reg.conn
        .query_row(
            "SELECT COUNT(*) FROM capabilities WHERE asset_id = ?1",
            rusqlite::params![asset_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())
}

/// 逐资产检查。**只读**，不改任何数据。
///
/// 返回资产级检查 + 能力级检查。能力级检查逐条产出，
/// 所以矩阵里的分母是**真实的能力条目数**，不是资产数。
pub fn check_all(
    reg: &crate::store::Registry,
    scan_root: Option<&Path>,
    path_base: Option<&Path>,
) -> Result<Vec<ProtocolCheck>, String> {
    let rows = load_assets(reg)?;
    let mut out = Vec::new();
    for a in &rows {
        out.extend(check_asset(reg, a, scan_root, path_base)?);
    }
    Ok(out)
}

fn check_asset(
    reg: &crate::store::Registry,
    a: &AssetRow,
    scan_root: Option<&Path>,
    path_base: Option<&Path>,
) -> Result<Vec<ProtocolCheck>, String> {
    let mut out = Vec::new();
    let mut asset = ProtocolCheck::asset_level(&a.id, &a.name, &a.source_format);

    // ① 协议受支持吗。
    if a.source_format.trim().is_empty() {
        asset.supported = false;
        asset.reasons.push("source_format 为空：无法判断这份内容按哪种协议读".into());
    } else if !is_supported(&a.source_format) {
        asset.supported = false;
        asset.reasons.push(format!(
            "协议 {} 不在支持列表（{}）",
            a.source_format,
            SUPPORTED_PROTOCOLS.join(" / ")
        ));
    }
    // 协议不受支持 = 读不懂。**supported 为假时必须同时判不通过**，
    // 否则矩阵里会出现"不支持但 100% 通过"的荒谬行。
    if !asset.supported {
        asset.passed = false;
    }

    // ② manifest 读得回来吗。
    let parsed: Option<ac_core::CapabilityManifest> =
        serde_json::from_str(&a.manifest).ok();
    let manifest = match parsed {
        Some(m) => m,
        None => {
            asset.reasons.push(format!(
                "manifest 不是合法的 CapabilityManifest JSON（{} 字节）",
                a.manifest.len()
            ));
            asset.passed = false;
            out.push(asset);
            // 读不出 manifest 就没法继续看能力条目，但**资产级失败已经记下了**。
            return Ok(out);
        }
    };

    // ③ schema 版本对得上吗。
    let v = manifest.schema_version.clone();
    let want = ac_core::SCHEMA_VERSION;
    if v != want {
        asset
            .reasons
            .push(format!("manifest schema 版本 {v} ≠ 程序 {want}"));
        asset.passed = false;
    }

    // ④ 源文件还在吗（给了基准目录才查）。
    //
    // **基准不是扫描根。** 入库时 source_path 记的是相对
    // **库文件父目录的上级**的路径（形如 ..\vendor\mcp\x\.mcp.json），
    // 而扫描根本身就已经是 vendor 目录了。拿扫描根去 join 会拼出
    // vendor\..\vendor\... 这种二次前缀，于是 506 条**全部**
    // 误报「源文件已不存在」，把协议矩阵做成 0% 通过。
    // 正确基准是库文件的父目录；两个基准都试，任一命中即算在。
    if let Some(base) = path_base {
        if a.source_path.trim().is_empty() {
            asset
                .reasons
                .push("source_path 为空：无法回溯源文件".into());
        } else {
            let p = std::path::Path::new(&a.source_path);
            let mut tried: Vec<std::path::PathBuf> = Vec::new();
            let mut found = false;
            let cand = base.join(p);
            if cand.exists() {
                found = true;
            }
            tried.push(cand);
            if let Some(root) = scan_root {
                let cand = root.join(p);
                if cand.exists() {
                    found = true;
                }
                tried.push(cand);
            }
            if !found {
                asset.reasons.push(format!(
                    "源文件已不存在：{}",
                    tried
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join("　或　")
                ));
                asset.passed = false;
            }
        }
    }

    // ⑤⑥ 能力条目：字段完整吗、真的落库了吗。
    let provides = manifest.provides.clone();
    let persisted = persisted_capability_count(reg, &a.id)?;
    if (provides.len() as i64) != persisted {
        asset.reasons.push(format!(
            "manifest 声明 {} 项能力，capabilities 表只有 {persisted} 行",
            provides.len()
        ));
        asset.passed = false;
    }

    let mut asset_has_bad_cap = false;
    for p in &provides {
        let mut c = ProtocolCheck::asset_level(&a.id, &a.name, &a.source_format);
        c.capability_id = p.capability_id.clone();
        c.supported = asset.supported;
        c.passed = true;
        if p.capability_id.trim().is_empty() {
            c.reasons
                .push("能力缺少 capability_id：无法被任何智能体稳定引用".into());
            c.passed = false;
        }
        if p.name.trim().is_empty() {
            c.reasons.push("能力缺少 name".into());
            c.passed = false;
        }
        // 资产级失败会传染到能力级：源文件都没了，能力当然读不懂。
        if !asset.passed {
            c.passed = false;
            c.reasons.extend(asset.reasons.iter().cloned());
        }
        if !c.passed {
            asset_has_bad_cap = true;
        }
        out.push(c);
    }
    if !asset_has_bad_cap && provides.is_empty() {
        asset
            .reasons
            .push("该资产未声明任何能力（不是故障，但协议上无内容可读）".into());
    }

    // 资产级那条排在能力级前面，UI 直接取第一条即可。
    out.insert(0, asset);
    Ok(out)
}

/// 把逐条检查聚成协议支持矩阵。
///
/// 分母口径写在字段注释里，不是随便挑的：
/// `assets` 数资产级行（capability_id 为空的那条），
/// `*_capabilities` 数能力级行（capability_id 非空）。
/// 资产里一条 `provides` 都没有时，两个分母都不会被它撑大——
/// 那正是「协议上无内容可读」，不是「解析失败」。
pub fn matrix(checks: &[ProtocolCheck]) -> ProtocolMatrix {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Acc {
        assets: u32,
        passed_assets: u32,
        failed_assets: u32,
        capabilities: u32,
        passed_capabilities: u32,
        failed_capabilities: u32,
        supported: Option<bool>,
        reasons: BTreeMap<String, u32>,
    }

    let mut by_proto: BTreeMap<String, Acc> = BTreeMap::new();
    for c in checks {
        let acc = by_proto.entry(c.protocol.clone()).or_default();
        // supported 必须是「全部都支持」而不是「任意一个支持」：
        // 一个协议里混进一条不支持的行，整体就不能标成 supported。
        acc.supported = Some(match acc.supported {
            None => c.supported,
            Some(prev) => prev && c.supported,
        });
        if c.capability_id.is_empty() {
            acc.assets += 1;
            if c.passed {
                acc.passed_assets += 1;
            } else {
                acc.failed_assets += 1;
            }
        } else {
            acc.capabilities += 1;
            if c.passed {
                acc.passed_capabilities += 1;
            } else {
                acc.failed_capabilities += 1;
            }
        }
        for r in &c.reasons {
            *acc.reasons.entry(r.clone()).or_insert(0) += 1;
        }
    }

    let mut m = ProtocolMatrix::default();
    for (protocol, acc) in by_proto {
        let total = acc.passed_capabilities + acc.failed_capabilities;
        let success_rate = if total == 0 {
            // 没有能力样本 → 0.0 而不是 1.0。
            // 「没有样本」不等于「全部通过」，这两件事在诊断面板上完全不同。
            0.0
        } else {
            acc.passed_capabilities as f32 / total as f32
        };
        let mut reasons: Vec<(String, u32)> = acc.reasons.into_iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        m.total_assets += acc.assets;
        m.total_capabilities += acc.capabilities;
        m.total_passed += acc.passed_assets + acc.passed_capabilities;
        m.total_failed += acc.failed_assets + acc.failed_capabilities;
        m.rows.push(ProtocolRow {
            protocol,
            supported: acc.supported.unwrap_or(true),
            assets: acc.assets,
            capabilities: acc.capabilities,
            passed_assets: acc.passed_assets,
            failed_assets: acc.failed_assets,
            passed_capabilities: acc.passed_capabilities,
            failed_capabilities: acc.failed_capabilities,
            success_rate,
            top_reasons: reasons.into_iter().take(3).map(|(r, _)| r).collect(),
        });
    }
    // 受支持的协议即使库里一条都没有，也要出现——
    // 「支持但没用到」和「不支持」是两回事，UI 上必须能分开。
    let present: Vec<String> = m.rows.iter().map(|r| r.protocol.clone()).collect();
    for p in SUPPORTED_PROTOCOLS {
        if !present.iter().any(|x| x == p) {
            m.rows.push(ProtocolRow {
                protocol: (*p).to_string(),
                supported: true,
                assets: 0,
                capabilities: 0,
                passed_assets: 0,
                failed_assets: 0,
                passed_capabilities: 0,
                failed_capabilities: 0,
                success_rate: 0.0,
                top_reasons: Vec::new(),
            });
        }
    }
    m.rows
        .sort_by(|a, b| b.capabilities.cmp(&a.capabilities).then(a.protocol.cmp(&b.protocol)));
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;

    fn asset(id: &str, format: &str, caps: &[&str]) -> ac_core::CapabilityManifest {
        let mut m = ac_core::CapabilityManifest::new(id, ac_core::AssetKind::Skill, id);
        m.provenance.source_format = Some(format.into());
        m.provenance.content_hash = Some(format!("h-{id}"));
        for c in caps {
            m.provides.push(ac_core::ProvidedCapability {
                capability_id: format!("{id}::{c}"),
                name: (*c).into(),
                summary: String::new(),
                input_schema: None,
                tags: vec![],
            });
        }
        m
    }

    #[test]
    fn supported_protocols_are_exactly_three() {
        assert!(is_supported("skill_md"));
        assert!(is_supported("plugin_json"));
        assert!(is_supported("mcp_json"));
        // 未知协议必须判否——库里真有过 source_format 为空的行。
        assert!(!is_supported(""));
        assert!(!is_supported("claude_plugin"));
    }

    #[test]
    fn healthy_library_passes_every_check() {
        let s = Scratch::new("proto-ok");
        let root = s.mkdir("vendor");
        std::fs::write(root.join("a").join("b").join("SKILL.md"), "x").ok();
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();
        std::fs::write(root.join("a").join("b").join("SKILL.md"), "x").unwrap();

        let db = s.path().join("registry.db");
        let r = crate::store::Registry::open(&db).unwrap();
        let mut m = asset("skill::a", "skill_md", &["one", "two"]);
        m.provenance.source_path = Some("a/b/SKILL.md".into());
        r.upsert_all(&[m]).unwrap();

        let checks = check_all(&r, Some(&root), Some(&root)).unwrap();
        // 1 条资产级 + 2 条能力级。
        assert_eq!(checks.len(), 3);
        for c in &checks {
            assert!(c.passed, "{:?} 未通过: {:?}", c.capability_id, c.reasons);
            assert!(c.reasons.is_empty());
        }

        let m = matrix(&checks);
        assert_eq!(m.total_assets, 1);
        assert_eq!(m.total_capabilities, 2);
        assert_eq!(m.total_failed, 0);
        assert!((m.rows[0].success_rate - 1.0).abs() < 1e-6);
    }

    #[test]
    fn unknown_protocol_fails_with_reason() {
        let s = Scratch::new("proto-unknown");
        let db = s.path().join("registry.db");
        let r = crate::store::Registry::open(&db).unwrap();
        r.upsert_all(&[asset("skill::a", "claude_plugin", &["one"])])
            .unwrap();

        let checks = check_all(&r, None, None).unwrap();
        let asset_check = &checks[0];
        assert!(!asset_check.supported);
        assert!(!asset_check.passed);
        assert!(
            asset_check
                .reasons
                .iter()
                .any(|r| r.contains("不在支持列表")),
            "{:?}",
            asset_check.reasons
        );
        // 能力级也必须跟着挂：协议都不支持，能力当然读不懂。
        assert!(!checks[1].passed);

        let m = matrix(&checks);
        let row = m.rows.iter().find(|r| r.protocol == "claude_plugin").unwrap();
        assert!(!row.supported);
        assert_eq!(row.failed_capabilities, 1);
    }

    #[test]
    fn declared_but_unpersisted_capability_is_caught() {
        let s = Scratch::new("proto-unpersisted");
        let db = s.path().join("registry.db");
        let r = crate::store::Registry::open(&db).unwrap();
        r.upsert_all(&[asset("skill::a", "skill_md", &["one"])])
            .unwrap();
        // 绕过 upsert 直接改 manifest：声明 3 项，库里只有 1 行。
        let mut m = r.get_manifest("skill::a").unwrap().unwrap();
        m.provides.push(ac_core::ProvidedCapability {
            capability_id: "skill::a::ghost".into(),
            name: "ghost".into(),
            summary: String::new(),
            input_schema: None,
            tags: vec![],
        });
        r.conn
            .execute(
                "UPDATE assets SET manifest = ?2 WHERE id = ?1",
                rusqlite::params!["skill::a", serde_json::to_string(&m).unwrap()],
            )
            .unwrap();

        let checks = check_all(&r, None, None).unwrap();
        assert!(!checks[0].passed);
        assert!(
            checks[0].reasons.iter().any(|x| x.contains("capabilities 表只有")),
            "{:?}",
            checks[0].reasons
        );
    }

    #[test]
    fn missing_source_file_fails_asset_check() {
        let s = Scratch::new("proto-nosrc");
        let root = s.mkdir("vendor");
        let db = s.path().join("registry.db");
        let r = crate::store::Registry::open(&db).unwrap();
        let mut m = asset("skill::a", "skill_md", &["one"]);
        m.provenance.source_path = Some("nowhere/SKILL.md".into());
        r.upsert_all(&[m]).unwrap();

        let checks = check_all(&r, Some(&root), Some(&root)).unwrap();
        assert!(!checks[0].passed);
        assert!(checks[0].reasons.iter().any(|x| x.contains("源文件已不存在")));
    }

    /// 回归：真实库里 source_path 是 `..\\vendor\\...\\SKILL.md` 这种形态
    /// ——相对**库文件的父目录**，而不是相对扫描根。
    ///
    /// 之前拿扫描根去 join，会拼出 `vendor\\..\\vendor\\...`，
    /// 于是**全部 506 条**都误判「源文件已不存在」，
    /// 协议矩阵被做成 0% 通过 / 1017 项不通过。这条测试钉住正确基准。
    #[test]
    fn source_path_relative_to_db_parent_still_resolves() {
        let s = Scratch::new("proto-relpath");
        let root = s.mkdir("vendor");
        s.write("vendor/skill/a/SKILL.md", "---\nname: a\n---\n");
        let db = s.path().join("registry.db");
        let r = crate::store::Registry::open(&db).unwrap();
        let mut m = asset("skill::a", "skill_md", &["one"]);
        // 与入库时一致：相对库文件父目录，扫描根上面还隔一级
        m.provenance.source_path = Some("..\\vendor\\skill\\a\\SKILL.md".into());
        r.upsert_all(&[m]).unwrap();

        // 基准 = 库文件的父目录（s.path()），扫描根 = s.path()/vendor
        let base = db.parent().unwrap();
        let checks = check_all(&r, Some(&root), Some(base)).unwrap();
        assert!(
            checks[0].passed,
            "按库文件父目录解析时源文件应当存在，失败原因：{:?}",
            checks[0].reasons
        );
    }

    #[test]
    fn zero_sample_protocol_is_not_reported_as_100_percent() {
        // 变异敏感：把 success_rate 的 0.0 分支改成 1.0，这条立刻红。
        let checks = vec![ProtocolCheck {
            asset_id: "a".into(),
            asset_name: "a".into(),
            capability_id: String::new(),
            protocol: "skill_md".into(),
            supported: true,
            passed: true,
            reasons: vec!["该资产未声明任何能力（不是故障，但协议上无内容可读）".into()],
        }];
        let m = matrix(&checks);
        let row = m.rows.iter().find(|r| r.protocol == "skill_md").unwrap();
        assert_eq!(row.capabilities, 0);
        assert_eq!(row.success_rate, 0.0, "没有样本不等于全部通过");
    }

    #[test]
    fn supported_but_unused_protocols_still_appear() {
        let m = matrix(&[]);
        for p in SUPPORTED_PROTOCOLS {
            assert!(
                m.rows.iter().any(|r| r.protocol == *p),
                "受支持但没用到的协议必须出现在矩阵里: {p}"
            );
        }
        assert_eq!(m.total_assets, 0);
    }
}
