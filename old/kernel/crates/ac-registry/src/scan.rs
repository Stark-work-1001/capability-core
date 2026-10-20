//! 目录递归扫描。
//!
//! 原则：
//! - **只读**：绝不修改被扫描目录里的任何文件
//! - **容错**：单个文件解析失败只记录不中断
//! - **可跳过**：默认跳过依赖目录，避免把 node_modules 当资产

use crate::parse::{parse_asset, IdScope, ParseOutcome};
use ac_core::{AssetKind, CapabilityManifest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 相对路径的 6 位短哈希，给同目录同类清单做 id 判别后缀。
fn short_hash(s: &str) -> String {
    crate::parse::content_hash(s.as_bytes())[..6].to_string()
}

/// 扫描时始终跳过的目录名。
const DEFAULT_SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".git",
    "target",
    ".cache",
    ".work",
    "dist",
    "build",
    ".next",
    ".turbo",
    ".pytest_cache",
    ".tmp",
    "_tmp",
];

/// 归档 / 临时 / 废弃目录：里面的清单**曾经**是资产，现在不是。
///
/// 与 `DEFAULT_SKIP_DIRS` 的区别很重要：
/// - skip_dirs 是"依赖目录"，里面出现 `SKILL.md` 本身就是异常，静默跳过；
/// - quarantine 是"人为留存的旧版本"，跳过它是一次**有影响的决定**，
///   所以必须带原因记进 `ScanReport::skipped`，让人能查到"少了什么、为什么"。
///
/// **这份名单刻意保守**：只收不会误伤真资产的名字，任意深度都生效。
/// 曾经试过用「前导 `_` / `.`」当通用判据，结果两个方向都翻车——
/// `.` 前缀会误杀 `.qoder-plugin` / `.claude-plugin`（清单就在这层目录里），
/// `_` 前缀会误杀 `_tools`。所以改成显式名单，宁可漏掉也不误删。
///
/// 真实命中：
/// - `_临时文件` → `obsidian-suite/_临时文件/归档_上游原件/obsidian-skills-claude-plugin`
/// - `归档`     → 同上的更深一层
const DEFAULT_QUARANTINE_DIRS: &[&str] = &["_临时文件", "临时文件", "_归档", "归档", "_废弃", "废弃"];

/// 命中隔离目录时给出的原因前缀。
const QUARANTINE_REASON: &str = "隔离目录（归档/临时/废弃）";

/// 带这些后缀的目录段是废弃副本（任意深度）。
///
/// 真实案例：`plugin-hub-plugin.old` —— 整段目录名不叫"归档"，
/// 但它是上一版的全量拷贝（含 47,763 个文件、402 MB 的 `.work/`）。
const QUARANTINE_SUFFIXES: &[&str] = &[".old", ".bak", ".backup", ".orig", ".prev"];

/// 这些普通词**只在直接位于扫描根下**时才判为残留（深度 1）。
///
/// 顶层是「包槽位」，残留物在那儿；嵌套目录名是资产名，不能乱杀。
///
/// 踩过的坑：规则最初对任意深度匹配 `legacy-` 前缀，结果把
/// `architecture-visualization/skills/legacy-system-visualizer`
/// 这个正经技能隔离掉了——它只是名字里带 legacy（可视化遗留系统）。
const QUARANTINE_TOPLEVEL_WORDS: &[&str] = &[
    "backup", "backups", "obsolete", "deprecated", "legacy", "temp", "tmp", "old",
];

fn is_top_level_debris(seg: &str) -> bool {
    let lower = seg.to_ascii_lowercase();
    QUARANTINE_TOPLEVEL_WORDS.iter().any(|q| seg.eq_ignore_ascii_case(q))
        || lower.starts_with("_legacy")
        || lower.starts_with("legacy-")
}

fn has_debris_suffix(seg: &str) -> bool {
    let lower = seg.to_ascii_lowercase();
    QUARANTINE_SUFFIXES.iter().any(|x| lower.ends_with(x))
}

/// 路径的「相对根目录段」里命中隔离规则则返回该段名。
///
/// 三个必须做对的细节：
/// 1. **只看目录段**。最后一个分量是清单文件名，
///    `.mcp.json` / `.claude-plugin` 这类以点开头的文件必须放行——
///    按整条相对路径判的话它们会被当成残留目录，11 个 MCP 全灭。
/// 2. **只看相对路径段**。绝对路径会误伤：扫描根自己落在
///    `...\_work\...` 下时整棵树都得被隔离。
/// 3. **「顶层」按深度算，不是按父目录的最后一段**。
///    `pkg/skills/backup/SKILL.md` 的父目录末段是 `backup`，但它深在第 3 层，
///    不是顶层包槽位，不能按顶层规则杀。
fn quarantine_hit(rel: &Path, list: &[&str]) -> Option<String> {
    let dir = rel.parent()?;
    let mut depth = 0usize;
    for c in dir.components() {
        depth += 1;
        let s = c.as_os_str().to_string_lossy();
        let seg: &str = &s;
        if list.iter().any(|q| q.eq_ignore_ascii_case(seg))
            || has_debris_suffix(seg)
            || (depth == 1 && is_top_level_debris(seg))
        {
            return Some(seg.to_string());
        }
    }
    None
}

#[derive(Debug, Default)]
pub struct ScanReport {
    /// 扫描根本身是否可读。
    ///
    /// 下游据此判断"这次扫描算不算数"：根都打不开时，
    /// 空结果不代表资产全没了，此时**不能**拿它去对账删库。
    pub root_ok: bool,
    /// 成功解析的资产，按 id 排序，保证结果确定
    pub manifests: Vec<CapabilityManifest>,
    /// 跳过的文件及原因
    pub skipped: Vec<SkippedFile>,
    /// 各类型计数
    pub counts: BTreeMap<String, u32>,
    /// 扫描耗时（毫秒）
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone)]
pub struct SkippedFile {
    pub path: String,
    pub reason: String,
}

impl ScanReport {
    pub fn total(&self) -> u32 {
        self.counts.values().sum()
    }
}

pub struct ScanOptions<'a> {
    pub skip_dirs: &'a [&'a str],
    pub max_depth: usize,
}

impl Default for ScanOptions<'_> {
    fn default() -> Self {
        Self {
            skip_dirs: DEFAULT_SKIP_DIRS,
            max_depth: 8,
        }
    }
}

pub fn scan_dir(root: &Path) -> ScanReport {
    scan_dir_with(root, &ScanOptions::default())
}

pub fn scan_dir_with(root: &Path, opts: &ScanOptions<'_>) -> ScanReport {
    let t0 = std::time::Instant::now();
    let mut report = ScanReport::default();

    if !root.exists() {
        report.skipped.push(SkippedFile {
            path: root.to_string_lossy().to_string(),
            reason: "目录不存在".into(),
        });
        return report;
    }
    report.root_ok = true;

    // ── 阶段一：收集候选清单 ──
    // 先收齐再解析，因为 id 是否需要判别后缀，取决于**同目录下有没有
    // 第二份同类清单**（例如 `.mcp.json` 与 `mcp.json` 并存），
    // 单看一个文件是判断不出来的。
    struct Candidate {
        path: PathBuf,
        kind: AssetKind,
        rel_dir: String,
        rel_file: String,
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut per_dir_kind: BTreeMap<(String, String), u32> = BTreeMap::new();

    let walker = walkdir::WalkDir::new(root)
        .max_depth(opts.max_depth)
        .follow_links(false);

    for entry in walker.into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();

        // 跳过检查只针对「相对扫描根」的路径段。
        // 早期版本检查的是整条绝对路径，结果扫描根自己落在
        // `.../target/release/...` 下时整棵树被当成依赖目录跳过——
        // 一条静默的漏扫，比报错难查得多。
        let rel = path.strip_prefix(root).unwrap_or(path);

        if rel
            .components()
            .any(|c| opts.skip_dirs.contains(&c.as_os_str().to_string_lossy().as_ref()))
        {
            continue;
        }

        let Some(kind) = classify(path) else {
            continue;
        };

        // 隔离检查放在 classify 之后：只有「长得像资产」的清单才值得留痕。
        // 放在前面的话，`_临时文件/` 里几千张 png 和 json 会把跳过报告淹掉，
        // 真正要看的归档清单反而被埋了。
        if let Some(seg) = quarantine_hit(rel, DEFAULT_QUARANTINE_DIRS) {
            report.skipped.push(SkippedFile {
                path: path.to_string_lossy().to_string(),
                reason: format!("{QUARANTINE_REASON}：{seg}"),
            });
            continue;
        }

        // 用「相对扫描根的目录路径」当 id 作用域，保证 id 唯一。
        // 曾经只用叶目录名，于是 `.qoder-plugin` 这种清单目录名让
        // 22 个不同插件挤进同一个 id，508 个资产只剩 427 个且不报错。
        let rel_dir = rel
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        *per_dir_kind
            .entry((kind.as_str().to_string(), rel_dir.clone()))
            .or_insert(0) += 1;

        candidates.push(Candidate {
            path: path.to_path_buf(),
            kind,
            rel_dir,
            rel_file: rel.to_string_lossy().to_string(),
        });
    }

    // ── 阶段二：解析 ──
    for c in &candidates {
        let crowded = per_dir_kind
            .get(&(c.kind.as_str().to_string(), c.rel_dir.clone()))
            .is_some_and(|n| *n > 1);
        let hash = short_hash(&c.rel_file);
        let scope = if crowded {
            // 冲突组全体都打标签（而不是"只给后来者打"），
            // 这样以后再往这个目录塞一份清单，已有资产的 id 不会变。
            IdScope::tagged(&c.rel_dir, &hash)
        } else {
            IdScope::new(&c.rel_dir)
        };

        match parse_asset(&c.path, Some(c.kind), scope) {
            ParseOutcome::Ok(m) => {
                // 空壳清单：连名字和描述都读不出来，留着只会污染检索结果。
                // 注意这**不等于**"零能力"——组件写在同级目录、清单没内联声明的
                // 插件能力数为 0，但它是真资产，必须保留（等 M2 抽取组件）。
                if is_empty_shell(&m) {
                    report.skipped.push(SkippedFile {
                        path: c.path.to_string_lossy().to_string(),
                        reason: "空壳清单（无名称且无描述）".into(),
                    });
                    continue;
                }
                *report.counts.entry(m.kind.as_str().to_string()).or_insert(0) += 1;
                report.manifests.push(*m);
            }
            ParseOutcome::Skipped { reason } => report.skipped.push(SkippedFile {
                path: c.path.to_string_lossy().to_string(),
                reason,
            }),
        }
    }

    // 确定性排序：同样的输入必须得到同样的顺序
    report.manifests.sort_by(|a, b| a.id.cmp(&b.id));
    report.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    report.elapsed_ms = t0.elapsed().as_millis();
    report
}

/// 按文件名判定资产类型。
fn classify(path: &Path) -> Option<AssetKind> {
    let name: PathBuf = path.file_name()?.into();
    match name.to_str()? {
        "SKILL.md" => Some(AssetKind::Skill),
        "plugin.json" => Some(AssetKind::Plugin),
        "openapi.json" => Some(AssetKind::Mcp),
        "agent-card.json" | "ghost.json" => Some(AssetKind::Ghost),
        "organization.json" => Some(AssetKind::Organization),
        ".mcp.json" | "mcp.json" | "mcp-servers.json" => Some(AssetKind::Mcp),
        _ => None,
    }
}

/// 空壳清单判定：名称和描述同时为空。
///
/// 刻意**不**拿"能力数为 0"当判据——`.claude-plugin` 这类插件把组件放在同级目录、
/// 清单里不内联声明，能力数天然是 0，但它是真资产。
/// 误删真资产的代价远高于多留一条噪声，所以判据收紧到"两个必填字段同时空"。
fn is_empty_shell(m: &CapabilityManifest) -> bool {
    m.name.trim().is_empty() && m.description.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;

    /// 在临时目录里造一棵测试树。
    fn fixture(tag: &str) -> Scratch {
        let s = Scratch::new(tag);
        s.mkdir("s1");
        s.mkdir("s2");
        s.mkdir("node_modules/junk");
        s.mkdir("p1");

        s.write("s1/SKILL.md", "---\nname: s1\ndescription: 第一个技能\n---\n正文");
        s.write("s2/SKILL.md", "---\nname: s2\ndescription: 第二个技能\n---\n正文");
        // 依赖目录里的 SKILL.md 必须被跳过
        s.write("node_modules/junk/SKILL.md", "---\nname: junk\n---\n");
        s.write(
            "p1/plugin.json",
            r#"{"name":"p1","description":"插件","commands":[{"name":"a"}]}"#,
        );
        s
    }

    #[test]
    fn scans_skills_and_plugin_and_skips_deps() {
        let base = fixture("main");
        let r = scan_dir(base.path());

        let ids: Vec<&str> = r.manifests.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"skill::s1"));
        assert!(ids.contains(&"skill::s2"));
        assert!(ids.contains(&"plugin::p1"));
        assert!(!ids.iter().any(|i| i.contains("junk")), "node_modules 必须被跳过");
        assert_eq!(r.counts.get("skill"), Some(&2));
        assert_eq!(r.counts.get("plugin"), Some(&1));
    }

    #[test]
    fn results_are_deterministic() {
        let base = fixture("det");
        let a = scan_dir(base.path());
        let b = scan_dir(base.path());
        let ids_a: Vec<&str> = a.manifests.iter().map(|m| m.id.as_str()).collect();
        let ids_b: Vec<&str> = b.manifests.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids_a, ids_b);
    }

    #[test]
    fn missing_directory_reports_skip_not_panic() {
        let r = scan_dir(Path::new("Z:/definitely/not/here"));
        assert!(r.manifests.is_empty());
        assert_eq!(r.skipped.len(), 1);
    }

    #[test]
    fn malformed_file_is_skipped_not_fatal() {
        let s = Scratch::new("bad");
        s.write("bad/SKILL.md", "\u{0}\u{1} binary garbage");
        s.write("ok/SKILL.md", "---\nname: ok\n---\nx");

        let r = scan_dir(s.path());
        // 坏文件不应导致整体失败，好文件仍应入库
        assert!(r.manifests.iter().any(|m| m.id == "skill::ok"));
    }

    #[test]
    fn skip_check_is_relative_to_scan_root() {
        // 回归：扫描根自己落在名字像依赖目录的路径段下时不能全被跳过。
        // 过去检查的是绝对路径，于是 target/release/... 里的资产全丢。
        let s = Scratch::new("relskip");
        s.write("real/SKILL.md", "---\nname: kept\n---\nx");
        s.write("node_modules/junk/SKILL.md", "---\nname: dropped\n---\nx");

        let r = scan_dir(s.path());
        let ids: Vec<&str> = r.manifests.iter().map(|m| m.id.as_str()).collect();
        // id 取自所在目录名，不是 frontmatter 里的 name
        assert!(ids.contains(&"skill::real"), "根路径下的资产必须能被扫到");
        assert!(!ids.contains(&"skill::junk"), "根路径内的依赖目录仍要跳过");
    }

    #[test]
    fn same_dir_two_manifest_kinds_do_not_collide() {
        // 回归：同目录里 `.mcp.json` 与 `mcp.json` 并存时，
        // 光按目录取 id 会让它们互相覆盖
        let s = Scratch::new("twomanifest");
        s.write("mcp-local/.mcp.json", r#"{"mcpServers":{"gd":{"command":"uvx"}}}"#);
        s.write("mcp-local/mcp.json", r#"{"mcpServers":{"other":{"command":"npx"}}}"#);

        let r = scan_dir(s.path());
        let mcp_ids: Vec<&str> = r
            .manifests
            .iter()
            .filter(|m| m.kind == AssetKind::Mcp)
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(mcp_ids.len(), 2, "两份清单都要在: {mcp_ids:?}");
        assert_eq!(r.counts.get("mcp"), Some(&2));
    }

    #[test]
    fn cjk_directory_names_get_distinct_ids() {
        // 回归：中文目录名被 slugify 丢弃时，同一批中文技能会全部撞成一个 id
        let s = Scratch::new("cjk");
        s.write("pkg/skills/产品指标复盘/SKILL.md", "---\nname: 产品指标复盘\n---\nx");
        s.write("pkg/skills/竞品分析/SKILL.md", "---\nname: 竞品分析\n---\nx");

        let r = scan_dir(s.path());
        let ids: Vec<&str> = r.manifests.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids.len(), 2, "两个中文技能都要在: {ids:?}");
        assert!(ids.contains(&"skill::pkg-skills-产品指标复盘"), "{ids:?}");
        assert!(ids.contains(&"skill::pkg-skills-竞品分析"), "{ids:?}");
    }

    #[test]
    fn classify_by_filename() {
        assert_eq!(classify(Path::new("/a/SKILL.md")), Some(AssetKind::Skill));
        assert_eq!(classify(Path::new("/a/plugin.json")), Some(AssetKind::Plugin));
        assert_eq!(classify(Path::new("/a/.mcp.json")), Some(AssetKind::Mcp));
        assert_eq!(classify(Path::new("/a/readme.md")), None);
    }

    #[test]
    fn quarantine_dirs_are_excluded_with_reason() {
        // 回归：归档/临时目录里的清单曾经直接进库。
        // 真实案例 `obsidian-suite/_临时文件/归档_上游原件/obsidian-skills-claude-plugin`
        // 在 508 个资产里就是一条零能力的垃圾记录。
        let s = Scratch::new("quar");
        s.write("live/SKILL.md", "---\nname: live\ndescription: 活的\n---\nx");
        s.write(
            "_临时文件/归档_上游原件/old/SKILL.md",
            "---\nname: old\ndescription: 归档的\n---\nx",
        );
        s.write("pkg/废弃/SKILL.md", "---\nname: dep\ndescription: 废弃的\n---\nx");
        s.write(
            "tool.old/SKILL.md",
            "---\nname: prev\ndescription: 上一版\n---\nx",
        );
        s.write(
            "_legacy_bloated/pkg/SKILL.md",
            "---\nname: bloated\ndescription: 膨胀旧拷贝\n---\nx",
        );

        let r = scan_dir(s.path());
        let ids: Vec<&str> = r.manifests.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["skill::live"], "只有活资产入库: {ids:?}");

        let reasons: Vec<&str> = r.skipped.iter().map(|f| f.reason.as_str()).collect();
        assert_eq!(reasons.len(), 4, "四条都要留痕: {reasons:?}");
        assert!(
            reasons.iter().all(|x| x.starts_with(QUARANTINE_REASON)),
            "原因必须写明是隔离而非静默丢弃: {reasons:?}"
        );
    }

    #[test]
    fn quarantine_only_records_asset_shaped_files() {
        // 回归：隔离留痕必须排在 classify 之后。
        // 排在前面的话 `_临时文件/` 里几千张 png/json 会把跳过报告淹掉。
        let s = Scratch::new("quarcls");
        s.write(
            "_临时文件/归档/pkg/SKILL.md",
            "---\nname: old\ndescription: 归档的\n---\nx",
        );
        s.write("_临时文件/扫描件/p034.png", "not-an-asset");
        s.write("_临时文件/扫描件/p034_blocks.json", "{}");

        let r = scan_dir(s.path());
        assert!(r.manifests.is_empty(), "归档资产不入库");
        assert_eq!(
            r.skipped.len(),
            1,
            "只有清单该留痕，图片/数据文件不算: {:?}",
            r.skipped
        );
        assert!(r.skipped[0].path.ends_with("SKILL.md"));
    }

    #[test]
    fn nested_asset_dir_named_legacy_is_not_quarantined() {
        // 回归（本轮真实误伤）：规则最初对任意深度匹配 `legacy-` 前缀，
        // 把 `architecture-visualization/skills/legacy-system-visualizer`
        // 这个正经技能隔离掉了——它只是名字里带 legacy（可视化遗留系统）。
        //
        // 结论：普通词只能当**顶层**判据；嵌套目录名是资产名，不能按词杀。
        let s = Scratch::new("quarlegacy");
        s.write(
            "architecture-visualization/skills/legacy-system-visualizer/SKILL.md",
            "---\nname: legacy-system-visualizer\ndescription: 可视化遗留系统\n---\nx",
        );
        s.write(
            "pkg/skills/backup/SKILL.md",
            "---\nname: backup\ndescription: 备份技能\n---\nx",
        );
        s.write("obsidian-suite/_临时文件/归档/SKILL.md", "---\nname: junk\ndescription: 归档\n---\nx");

        let r = scan_dir(s.path());
        let ids: Vec<&str> = r.manifests.iter().map(|m| m.id.as_str()).collect();
        assert!(
            ids.contains(&"skill::architecture-visualization-skills-legacy-system-visualizer"),
            "嵌套的 legacy- 技能必须保留: {ids:?}"
        );
        assert!(
            ids.contains(&"skill::pkg-skills-backup"),
            "嵌套的 backup 技能必须保留: {ids:?}"
        );
        assert_eq!(r.skipped.len(), 1, "只有归档那条该隔离: {:?}", r.skipped);
    }

    #[test]
    fn quarantine_check_is_relative_to_scan_root() {
        // 回归：隔离检查同样只能看相对路径段。
        // 绝对路径版会让落在 `.../备份/...` 下的扫描根整棵树被隔离。
        let s = Scratch::new("quarrel");
        s.write("real/SKILL.md", "---\nname: kept\ndescription: 保留\n---\nx");
        s.write("_临时文件/old/SKILL.md", "---\nname: dropped\ndescription: 丢弃\n---\nx");

        let r = scan_dir(s.path());
        let ids: Vec<&str> = r.manifests.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"skill::real"), "{ids:?}");
        assert!(!ids.contains(&"skill::dropped"), "{ids:?}");
    }

    #[test]
    fn empty_shell_manifest_is_quarantined_but_zero_capability_is_kept() {
        // 关键区分：「没有能力」不等于「空壳」。
        // `.claude-plugin` 插件把组件放在同级目录，清单不内联声明，
        // 能力数为 0 但必须保留；只有名称和描述同时空才是空壳。
        let s = Scratch::new("shell");
        s.write(
            "shell/plugin.json",
            r#"{"name":"","description":"","commands":[]}"#,
        );
        s.write(
            "zero/plugin.json",
            r#"{"name":"zero-cap","description":"组件在同级目录","commands":[]}"#,
        );

        let r = scan_dir(s.path());
        let ids: Vec<&str> = r.manifests.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["plugin::zero"], "零能力插件必须保留: {ids:?}");

        let shell_skip = r
            .skipped
            .iter()
            .find(|f| f.path.contains("shell"))
            .expect("空壳清单必须留痕");
        assert!(
            shell_skip.reason.contains("空壳"),
            "原因要指明是空壳: {}",
            shell_skip.reason
        );
    }
}
