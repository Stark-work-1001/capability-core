//! 纳管目录（assets/）的骨架与读写工具。
//!
//! # 为什么纳管实体放在文件夹里，而元数据放数据库里
//!
//! 两套存储不是拍脑袋分的：
//!
//! - 数据库擅长**查状态与历史**（哪些坏了、谁修的、修了几次）；
//! - 文件夹擅长**放能直接执行的东西**（启动脚本、venv、依赖）。
//!
//! 混在一起两头不讨好：把 venv 塞进 BLOB 会让备份与调试都变难；
//! 把「谁认领了」写进目录名会让并发认领没法做。
//!
//! # 布局
//!
//! ```text
//! capability-core/
//!   data/registry.db     数据库（元数据 / 状态 / 故障记录 / 修复历史）
//!   assets/             纳管的能力实体
//!     mcp/<id>/          可用的 MCP server + 启动脚本 + 依赖
//!     skill/<id>/        纳管的技能副本
//! ```
//! # 本模块绝不做的事
//!
//! **不维修**。它只建目录、写读纳管实体、登记事实。
//! 把一个坏掉的能力修好是外部智能体的判断，不是中枢的。
//! 中枢能做的边界是：把外部修好的东西放进来，并记住它是什么。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 纳管实体的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Mcp,
    Skill,
}

impl AssetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Skill => "skill",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mcp" => Some(Self::Mcp),
            "skill" => Some(Self::Skill),
            _ => None,
        }
    }
}

/// assets/ 目录的根。
///
/// 刻意用结构体而不是一串函数：根路径是**唯一**的注入点，
/// 测试可以指向临时目录，生产指向 capability-core/assets。
#[derive(Debug, Clone)]
pub struct AssetRoot {
    root: PathBuf,
}

/// 一次目录骨架的初始化结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetLayoutReport {
    pub root: String,
    /// 本次新建的目录（相对 root）。
    pub created: Vec<String>,
    /// 本来就在的目录（相对 root）。
    pub existing: Vec<String>,
}

/// 一次纳管写入的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetWriteReport {
    /// 纳管实体在 assets/ 下的相对路径。
    pub rel_path: String,
    /// 写进去的文件（相对 rel_path）。
    pub files: Vec<String>,
    /// 登记到库里的资产 id。
    pub asset_id: String,
}

impl AssetRoot {
    /// 由 capability-core 的项目根构造（内部会拼上 assets/）。
    pub fn from_project(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into().join("assets") }
    }

    /// 直接用 assets/ 那一层的路径。
    pub fn at(assets_dir: impl Into<PathBuf>) -> Self {
        Self { root: assets_dir.into() }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// 某个实体应该在的目录。
    pub fn entity_dir(&self, kind: AssetKind, id: &str) -> PathBuf {
        self.root.join(kind.as_str()).join(sanitize_id(id))
    }

    /// 建出 assets/、assets/mcp/、assets/skill/。
    ///
    /// 幂等：已经存在的目录不会被当成错误，也不会被清空。
    /// 这一点是硬要求——第二次调用绝不能抹掉上一次纳管的东西。
    pub fn init_layout(&self) -> Result<AssetLayoutReport, String> {
        let mut rep = AssetLayoutReport {
            root: self.root.to_string_lossy().to_string(),
            ..Default::default()
        };
        for (rel, dir) in [
            ("", self.root.clone()),
            ("mcp", self.root.join("mcp")),
            ("skill", self.root.join("skill")),
        ] {
            if dir.is_dir() {
                rep.existing.push(rel.to_string());
                continue;
            }
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("建目录 {} 失败: {e}", dir.display()))?;
            rep.created.push(rel.to_string());
        }
        Ok(rep)
    }

    /// 写入一个 MCP 纳管实体。
    ///
    /// files 是「相对文件名 -> 内容」；**不做任何内容解释**，
    /// 中枢不理解启动脚本写的是什么，它只负责把外部智能体交给它的东西落地。
    ///
    /// 若目标文件已存在，返回错误而**不是**覆盖：
    /// 覆盖会静默抹掉上一次的纳管成果，而调用方从返回值里看不出来。
    /// 要替换必须显式先调用 remove_entity。
    pub fn install_mcp(
        &self,
        id: &str,
        files: &[(String, Vec<u8>)],
        entry: &str,
    ) -> Result<AssetWriteReport, String> {
        self.install(AssetKind::Mcp, id, files, entry)
    }

    /// 写入一个 skill 纳管实体。
    pub fn install_skill(
        &self,
        id: &str,
        files: &[(String, Vec<u8>)],
        entry: &str,
    ) -> Result<AssetWriteReport, String> {
        self.install(AssetKind::Skill, id, files, entry)
    }

    fn install(
        &self,
        kind: AssetKind,
        id: &str,
        files: &[(String, Vec<u8>)],
        entry: &str,
    ) -> Result<AssetWriteReport, String> {
        let safe = sanitize_id(id);
        if safe.is_empty() {
            return Err(format!("非法的实体 id: {id:?}"));
        }
        if files.is_empty() {
            return Err("纳管实体至少要有一个文件".to_string());
        }
        let dir = self.entity_dir(kind, &safe);
        // 入口必须真的在待写入的文件里。不校验的话，一个打错字的 entry
        // 会被原样登记进库，而 verify_entity 之后永远返回 false——
        // 那时没人知道是入口写错了还是文件真的丢了。
        if !entry.trim().is_empty() {
            let want = sanitize_rel(entry)?;
            let in_files = files
                .iter()
                .any(|(n, _)| sanitize_rel(n).map(|s| s == want).unwrap_or(false));
            if !in_files {
                return Err(format!("入口文件 {want} 不在本次纳管的文件清单里"));
            }
        }
        // 先整体检查一遍，再落盘：半途失败会留下一个半个实体，
        // 那比完全没有更难排查。
        for (name, _) in files {
            let rel = sanitize_rel(name)?;
            let target = dir.join(&rel);
            if target.exists() {
                return Err(format!(
                    "纳管目标 {} 已存在；要替换请先 remove_entity",
                    target.display()
                ));
            }
        }
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("建纳管目录 {} 失败: {e}", dir.display()))?;
        let mut written: Vec<String> = Vec::new();
        for (name, content) in files {
            let rel = sanitize_rel(name)?;
            let target = dir.join(&rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("建子目录 {} 失败: {e}", parent.display()))?;
            }
            std::fs::write(&target, content)
                .map_err(|e| format!("写 {} 失败: {e}", target.display()))?;
            written.push(rel);
        }
        Ok(AssetWriteReport {
            rel_path: format!("{}/{}", kind.as_str(), safe),
            files: written,
            asset_id: id.to_string(),
        })
    }

    /// 删除一个纳管实体（**只在 assets/ 内部**）。
    ///
    /// 这是本模块唯一的破坏性动作，因此把范围锁死：
    /// 目标路径必须是 assets/<mcp|skill>/<id> 这一层的直接子目录，
    /// 且规范化之后仍要在 root 之内。
    /// 少了这两条，id 里只要有一个 .. 就能删到 assets/ 外面去。
    pub fn remove_entity(&self, kind: AssetKind, id: &str) -> Result<bool, String> {
        let dir = self.entity_dir(kind, id);
        if !dir.exists() {
            return Ok(false);
        }
        let canon_root = self
            .root
            .canonicalize()
            .map_err(|e| format!("无法规范化 assets 根: {e}"))?;
        let canon_dir = dir
            .canonicalize()
            .map_err(|e| format!("无法规范化目标目录: {e}"))?;
        if !canon_dir.starts_with(&canon_root) || canon_dir == canon_root {
            return Err(format!("拒绝删除 assets 之外的路径: {}", dir.display()));
        }
        // 必须是 root/<kind>/<id> 这一层，不能是 root 本身或 kind 目录本身
        let expected_depth = canon_root.components().count() + 2;
        if canon_dir.components().count() != expected_depth {
            return Err(format!("拒绝删除非实体层目录: {}", dir.display()));
        }
        std::fs::remove_dir_all(&canon_dir)
            .map_err(|e| format!("删除 {} 失败: {e}", canon_dir.display()))?;
        Ok(true)
    }

    /// 纳管实体是否已就绪（目录在、入口文件在）。
    pub fn verify_entity(&self, kind: AssetKind, id: &str, entry: &str) -> bool {
        let dir = self.entity_dir(kind, id);
        if !dir.is_dir() {
            return false;
        }
        if entry.trim().is_empty() {
            return true;
        }
        dir.join(sanitize_rel(entry).unwrap_or_default()).is_file()
    }

    /// 列出磁盘上实际存在的纳管实体（不查库）。
    ///
    /// 与库里登记交叉核对用：目录在而库里没登记 = 有人手工放进去的；
    /// 库里登记而目录没了 = 纳管实体被删了但忘了取消登记。
    /// 两种都需要被看见，而不是被当成一致。
    pub fn list_entities(&self, kind: AssetKind) -> Result<Vec<String>, String> {
        let dir = self.root.join(kind.as_str());
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out: Vec<String> = std::fs::read_dir(&dir)
            .map_err(|e| format!("读 {} 失败: {e}", dir.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        out.sort();
        Ok(out)
    }
}

/// 把 id 洗成安全的单段路径名。
///
/// 保留 Unicode（真实资产里有中文），但把任何可能是路径分隔符、
/// 或者能构成 .. 的字符换掉——纳管 id 来自资产 id，而资产 id 来自磁盘路径，
/// 里头出现 ../ 不是不可能。
pub fn sanitize_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for c in id.chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
            if c == '.' {
                // 连续的 .. 会把目录跳出去
                if out.ends_with('.') {
                    continue;
                }
            }
            out.push(c);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    // 纯点号或空串都不合法
    if trimmed.chars().all(|c| c == '.') {
        String::new()
    } else {
        trimmed
    }
}

/// 校验纳管实体内部的相对路径：必须相对、且不跳出实体目录。
pub fn sanitize_rel(name: &str) -> Result<String, String> {
    let p = Path::new(name);
    if p.is_absolute() {
        return Err(format!("纳管文件名必须是相对路径: {name:?}"));
    }
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            std::path::Component::Normal(s) => parts.push(s.to_string_lossy().to_string()),
            std::path::Component::CurDir => {}
            _ => return Err(format!("纳管文件名里不允许出现 .. 或根: {name:?}")),
        }
    }
    if parts.is_empty() {
        return Err(format!("纳管文件名为空: {name:?}"));
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试落盘目录。
    ///
    /// **必须用 crate::testutil::Scratch，不能用 std::env::temp_dir()**：
    /// 本机执行环境给子进程加了路径白名单，往 %TEMP% 建目录一律
    /// PermissionDenied (os error 5)，而且只拦「创建」不拦「读取」，
    /// 所以它以 ACCESS_DENIED 的样子出现，极易误判成代码 bug。
    /// 这条坑已经写在 testutil.rs 的模块文档里了，这里直接复用它的解。
    fn tmp_root() -> (crate::testutil::Scratch, AssetRoot) {
        let d = crate::testutil::Scratch::new("assets");
        let root = AssetRoot::from_project(d.path());
        (d, root)
    }

    #[test]
    fn init_layout_creates_the_three_levels_and_is_idempotent() {
        let (_d, root) = tmp_root();
        let rep = root.init_layout().unwrap();
        assert!(rep.created.contains(&"mcp".to_string()));
        assert!(rep.created.contains(&"skill".to_string()));
        assert!(root.path().join("mcp").is_dir());
        assert!(root.path().join("skill").is_dir());
        // 第二次调用不能把上一次的东西删掉
        let rep2 = root.init_layout().unwrap();
        assert!(rep2.created.is_empty());
        assert!(rep2.existing.contains(&"mcp".to_string()));
    }

    #[test]
    fn install_mcp_writes_files_and_reports_the_relative_path() {
        let (_d, root) = tmp_root();
        root.init_layout().unwrap();
        let files = vec![
            ("run.cmd".to_string(), b"@echo off".to_vec()),
            ("mcp.json".to_string(), b"{}".to_vec()),
        ];
        let rep = root.install_mcp("obsidian-suite", &files, "run.cmd").unwrap();
        assert_eq!(rep.rel_path, "mcp/obsidian-suite");
        assert_eq!(rep.files.len(), 2);
        assert!(root.path().join("mcp/obsidian-suite/run.cmd").is_file());
        assert!(root.verify_entity(AssetKind::Mcp, "obsidian-suite", "run.cmd"));
    }

    #[test]
    fn install_refuses_to_silently_overwrite_an_existing_entity() {
        // 静默覆盖会抹掉上一次纳管的成果，而调用方从返回值里看不出来。
        let (_d, root) = tmp_root();
        root.init_layout().unwrap();
        let files = vec![("run.cmd".to_string(), b"a".to_vec())];
        root.install_mcp("x", &files, "run.cmd").unwrap();
        let err = root.install_mcp("x", &files, "run.cmd").unwrap_err();
        assert!(err.contains("已存在"), "实际错误：{err}");
    }

    #[test]
    fn install_rejects_an_entry_that_is_not_among_the_files() {
        let (_d, root) = tmp_root();
        root.init_layout().unwrap();
        let files = vec![("run.cmd".to_string(), b"a".to_vec())];
        assert!(root.install_mcp("x", &files, "other.cmd").is_err());
    }

    #[test]
    fn install_rejects_path_traversal_in_file_names() {
        let (_d, root) = tmp_root();
        root.init_layout().unwrap();
        let files = vec![("../escape.txt".to_string(), b"a".to_vec())];
        assert!(root.install_mcp("x", &files, "").is_err());
    }

    #[test]
    fn install_rejects_an_empty_file_set() {
        let (_d, root) = tmp_root();
        root.init_layout().unwrap();
        assert!(root.install_mcp("x", &[], "").is_err());
    }

    #[test]
    fn remove_entity_deletes_only_the_entity_and_refuses_paths_outside() {
        let (_d, root) = tmp_root();
        root.init_layout().unwrap();
        let files = vec![("run.cmd".to_string(), b"a".to_vec())];
        root.install_mcp("x", &files, "run.cmd").unwrap();
        assert!(root.remove_entity(AssetKind::Mcp, "x").unwrap());
        assert!(!root.path().join("mcp/x").exists());
        // 目录骨架还在
        assert!(root.path().join("mcp").is_dir());
        // 不存在的实体：返回 false 而不是报错
        assert!(!root.remove_entity(AssetKind::Mcp, "nope").unwrap());
    }

    #[test]
    fn remove_entity_cannot_escape_via_a_dotdot_id() {
        let (_d, root) = tmp_root();
        root.init_layout().unwrap();
        // 洗过的 id 不可能含 ..，所以这里实际删的是 mcp/..- 之类的目录名；
        // 关键是它绝不能返回成功并删到 root 外面。
        let outside = root.path().parent().map(|p| p.to_path_buf());
        let _ = root.remove_entity(AssetKind::Mcp, "../..");
        if let Some(p) = outside {
            assert!(p.exists(), "assets 之外的东西一个都不许没");
        }
    }

    #[test]
    fn sanitize_id_keeps_unicode_but_strips_separators() {
        assert_eq!(sanitize_id("obsidian-suite"), "obsidian-suite");
        assert_eq!(sanitize_id("产品指标复盘"), "产品指标复盘");
        assert!(!sanitize_id("a/b").contains('/'));
        assert!(!sanitize_id("a\\b").contains('\\'));
        assert_eq!(sanitize_id(".."), "");
        assert_eq!(sanitize_id(""), "");
    }

    #[test]
    fn sanitize_rel_rejects_absolute_and_parent_segments() {
        assert!(sanitize_rel("/etc/passwd").is_err());
        assert!(sanitize_rel("../x").is_err());
        assert!(sanitize_rel("").is_err());
        assert_eq!(sanitize_rel("mcp/run.cmd").unwrap(), "mcp/run.cmd");
        assert_eq!(sanitize_rel("./run.cmd").unwrap(), "run.cmd");
    }

    #[test]
    fn list_entities_reads_the_disk_not_the_database() {
        let (_d, root) = tmp_root();
        root.init_layout().unwrap();
        let files = vec![("run.cmd".to_string(), b"a".to_vec())];
        root.install_mcp("a", &files, "run.cmd").unwrap();
        root.install_mcp("b", &files, "run.cmd").unwrap();
        assert_eq!(root.list_entities(AssetKind::Mcp).unwrap(), vec!["a", "b"]);
        assert!(root.list_entities(AssetKind::Skill).unwrap().is_empty());
    }

    #[test]
    fn skill_and_mcp_namespaces_do_not_collide() {
        let (_d, root) = tmp_root();
        root.init_layout().unwrap();
        let files = vec![("SKILL.md".to_string(), b"# x".to_vec())];
        root.install_skill("same", &files, "SKILL.md").unwrap();
        let f2 = vec![("run.cmd".to_string(), b"a".to_vec())];
        root.install_mcp("same", &f2, "run.cmd").unwrap();
        assert!(root.path().join("skill/same/SKILL.md").is_file());
        assert!(root.path().join("mcp/same/run.cmd").is_file());
    }
}