//! 扫描 → 入库 的编排层。
//!
//! `scan_dir` 负责"磁盘上有什么"，`Registry` 负责"库里有什么"，
//! 这里把两者接起来，并补上**全量对账**：磁盘上删掉的资产，
//! 库里必须跟着消失，否则界面会一直挂着早就删掉的东西。
//!
//! 三种写入结果（`inserted` / `updated` / `unchanged`）由
//! `content_hash` 决定，不看 mtime —— 复制文件、换行符变化都会
//! 让 mtime 变，但内容没变就不该重建索引（M2 的索引开销不小）。

use crate::scan::{scan_dir_with, ScanOptions, SkippedFile};
use crate::store::{KindCount, Registry};
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct IngestReport {
    /// 扫描根
    pub root: String,
    /// 磁盘上解析成功的资产数
    pub parsed: u32,
    /// 新写入
    pub inserted: u32,
    /// 内容有变，索引需重建
    pub updated: u32,
    /// 内容未变，跳过
    pub unchanged: u32,
    /// 磁盘上已不存在，本次清除
    pub removed: u32,
    /// 入库后的各类型计数
    pub counts: Vec<KindCount>,
    /// 解析失败的文件
    pub skipped: Vec<SkippedFile>,
    /// 委派图是否成环
    pub has_cycle: bool,
    pub elapsed_ms: u128,

    // ── M1.5 去重 ──
    /// 本次新建的逻辑资产数
    pub logical_created: u32,
    /// 本次刷新的位置数
    pub locations_seen: u32,
    /// 全库 alias 位置数（>0 即存在内容重复的位置）
    pub alias_count: u32,
    /// 待用户确认的去重候选。**绝不自动执行**。
    pub dedup_candidates: Vec<ac_core::DedupCandidate>,
}

impl IngestReport {
    /// 是否发生了需要下游（M2 索引）跟进的变更。
    pub fn has_changes(&self) -> bool {
        self.inserted > 0 || self.updated > 0 || self.removed > 0
    }
}

/// 扫描 `root` 下的全部资产并同步进 `registry`。
pub fn ingest(registry: &Registry, root: &Path) -> Result<IngestReport, String> {
    ingest_with(registry, root, &ScanOptions::default())
}

pub fn ingest_with(
    registry: &Registry,
    root: &Path,
    opts: &ScanOptions<'_>,
) -> Result<IngestReport, String> {
    let t0 = std::time::Instant::now();
    let scan = scan_dir_with(root, opts);

    let u = registry.upsert_all(&scan.manifests)?;

    // ── 全量对账：把库里存在、磁盘上已消失的资产清掉 ──
    // 只在"这次扫描确实覆盖了东西"时才做，否则扫描失败会误删整库。
    //
    // M1.5 说明：这里删的只是 `assets` 行（manifest 本体）。
    // **位置痕迹不会被删**——`asset_locations` 里那一条留在原地、
    // 不再被刷新，用户因此能查到"上次这里还有一版"。
    let mut removed = 0u32;
    if scan.root_ok {
        let seen: std::collections::BTreeSet<&str> =
            scan.manifests.iter().map(|m| m.id.as_str()).collect();
        for id in registry.list_ids()? {
            // A scan owns only its own root. Scanning vendor must never delete
            // managed imports or assets registered from another directory.
            let owned = registry.get_manifest(&id)?.and_then(|m| m.provenance.source_path)
                .map(|p| Path::new(&p).is_absolute() && Path::new(&p).starts_with(root))
                .unwrap_or(false);
            if owned && !seen.contains(id.as_str()) {
                registry.remove(&id)?;
                removed += 1;
            }
        }
    }

    // ── M1.5：同步逻辑资产 + 位置集（append-only）──
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let logical = registry.sync_logical(&scan.manifests, now)?;

    // 去重候选：**只是候选**，绝不自动合并。UI 必须让用户显式裁决。
    let candidates = registry.dedup_candidates()?;

    Ok(IngestReport {
        root: root.display().to_string(),
        parsed: scan.manifests.len() as u32,
        inserted: u.inserted,
        updated: u.updated,
        unchanged: u.unchanged,
        removed,
        counts: registry.counts_by_kind()?,
        skipped: scan.skipped,
        has_cycle: registry.find_cycle()?.is_some(),
        elapsed_ms: t0.elapsed().as_millis(),
        logical_created: logical.logical_created,
        locations_seen: logical.locations_seen,
        alias_count: logical.alias_count,
        dedup_candidates: candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;

    fn tree(s: &Scratch) {
        s.write("alpha/SKILL.md", "---\nname: alpha\ndescription: a\n---\nx");
        s.write("beta/SKILL.md", "---\nname: beta\ndescription: b\n---\nx");
    }

    #[test]
    fn first_ingest_inserts_everything() {
        let s = Scratch::new("ing1");
        tree(&s);
        let reg = Registry::open_in_memory().unwrap();

        let r = ingest(&reg, s.path()).unwrap();
        assert_eq!(r.parsed, 2);
        assert_eq!(r.inserted, 2);
        assert_eq!(r.updated, 0);
        assert_eq!(r.unchanged, 0);
        assert_eq!(r.removed, 0);
        assert_eq!(reg.total().unwrap(), 2);
        assert!(r.has_changes());
    }

    #[test]
    fn second_ingest_without_changes_is_all_unchanged() {
        let s = Scratch::new("ing2");
        tree(&s);
        let reg = Registry::open_in_memory().unwrap();
        ingest(&reg, s.path()).unwrap();

        let r = ingest(&reg, s.path()).unwrap();
        assert_eq!(r.unchanged, 2);
        assert_eq!(r.inserted, 0);
        assert_eq!(r.updated, 0);
        assert!(!r.has_changes(), "内容没变就不该让下游重建索引");
    }

    #[test]
    fn content_change_is_reported_as_updated() {
        let s = Scratch::new("ing3");
        tree(&s);
        let reg = Registry::open_in_memory().unwrap();
        ingest(&reg, s.path()).unwrap();

        s.write("alpha/SKILL.md", "---\nname: alpha\ndescription: 改了\n---\ny");
        let r = ingest(&reg, s.path()).unwrap();
        assert_eq!(r.updated, 1);
        assert_eq!(r.unchanged, 1);
        assert_eq!(r.removed, 0);
        assert!(r.has_changes());
    }

    #[test]
    fn deleted_source_is_purged_from_registry() {
        let s = Scratch::new("ing4");
        tree(&s);
        let reg = Registry::open_in_memory().unwrap();
        ingest(&reg, s.path()).unwrap();

        std::fs::remove_dir_all(s.path().join("beta")).unwrap();
        let r = ingest(&reg, s.path()).unwrap();
        assert_eq!(r.removed, 1);
        assert_eq!(reg.total().unwrap(), 1);
        assert!(reg.get_manifest("skill::beta").unwrap().is_none());
    }

    #[test]
    fn failed_scan_does_not_wipe_the_registry() {
        // 回归：扫描根整个消失时，绝不能当成"资产全被删了"而清空库
        let s = Scratch::new("ing5");
        tree(&s);
        let reg = Registry::open_in_memory().unwrap();
        ingest(&reg, s.path()).unwrap();

        let r = ingest(&reg, Path::new("Z:/nope/not/here")).unwrap();
        assert_eq!(r.removed, 0, "扫描失败不等于资产被删");
        assert_eq!(reg.total().unwrap(), 2);
    }

    #[test]
    fn scanning_one_root_preserves_other_roots() {
        let a = Scratch::new("ing-root-a");
        let b = Scratch::new("ing-root-b");
        a.write("alpha/SKILL.md", "---\nname: alpha\n---\na");
        b.write("beta/SKILL.md", "---\nname: beta\n---\nb");
        let reg = Registry::open_in_memory().unwrap();
        ingest(&reg, a.path()).unwrap();
        ingest(&reg, b.path()).unwrap();
        assert_eq!(reg.total().unwrap(), 2);
        ingest(&reg, a.path()).unwrap();
        assert_eq!(reg.total().unwrap(), 2);
    }

    #[test]
    fn counts_reflect_all_three_kinds() {
        let s = Scratch::new("ing6");
        s.write("alpha/SKILL.md", "---\nname: alpha\n---\nx");
        s.write("plug/plugin.json", r#"{"name":"plug","description":"p"}"#);
        s.write("mcp/.mcp.json", r#"{"mcpServers":{"gd":{"command":"uvx"}}}"#);
        let reg = Registry::open_in_memory().unwrap();

        let r = ingest(&reg, s.path()).unwrap();
        let by: std::collections::BTreeMap<_, _> =
            r.counts.iter().map(|k| (k.kind.clone(), k.count)).collect();
        assert_eq!(by.get("skill"), Some(&1));
        assert_eq!(by.get("plugin"), Some(&1));
        assert_eq!(by.get("mcp"), Some(&1));
    }
}
