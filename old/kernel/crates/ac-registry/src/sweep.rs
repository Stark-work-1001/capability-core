//! 把「检测」和「落库」串成一次可执行的巡检。
//!
//! # 为什么需要一个 runner 而不是让调用方自己拼
//!
//! 检测 + 落库 + 清理三步必须**同源**：清理依据的是「这次检出了什么」，
//! 而它只能由同一次检测给出。分开调用时，第二次检测的结果会覆盖第一次
//! 的清理依据，于是已修好的故障被留在了清单上——那种残留最难发现，
//! 因为它长得和「还没修」一模一样。
//!
//! # 边界
//!
//! runner **只读 vendor/**。它不写、不改、不修。
//! 唯一的写入是中枢自己的库（faults 三表）。

use crate::faults::{FaultStoreReport, StoredFault};
use crate::locator::{
    AssetProbe, FaultReport, LocateReport, Locator, ProbeEnv, ProbeFs, ProbePath,
};
use crate::store::Registry;

/// 一次巡检的结果。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SweepReport {
    /// 检测汇总（含健康的条目）。
    pub locate: LocateReport,
    /// 落库结果。
    pub stored: FaultStoreReport,
    /// 被本次检测证实已消失、因而清掉的旧故障 id。
    pub cleared: Vec<String>,
    /// 过程中读不到 / 读不了的文件。**不静默吞掉**：
    /// 一个读不到的资产和一个人健康的资产在计数上看起来一样，
    /// 但前者需要有人去看。
    pub unreadable: Vec<String>,
}

/// 一个待巡检的资产来源：资产 id / 类型 / 名字 / 源文件路径。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SweepTarget {
    pub asset_id: String,
    pub kind: String,
    pub asset_name: String,
    pub source_file: String,
}

/// 读文件。**只读**，读不到就如实返回 None。
fn read_source(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// 跑一次巡检并把结果落库。
///
/// 对每个目标：读配置 -> 检测 -> 落库 -> 用**本次检出集合**清理该资产的旧故障。
pub fn sweep<F: ProbeFs, P: ProbePath, E: ProbeEnv>(
    reg: &Registry,
    fs: &F,
    path: &P,
    env: &E,
    targets: &[SweepTarget],
    resolve: impl Fn(&str) -> std::path::PathBuf,
) -> Result<SweepReport, String> {
    sweep_with_remaps(reg, fs, path, env, targets, resolve, Vec::new())
}

/// 带搬家表的巡检。表由调用方给（见 crate::locator::RootRemap）。
pub fn sweep_with_remaps<F: ProbeFs, P: ProbePath, E: ProbeEnv>(
    reg: &Registry,
    fs: &F,
    path: &P,
    env: &E,
    targets: &[SweepTarget],
    resolve: impl Fn(&str) -> std::path::PathBuf,
    remaps: Vec<crate::locator::RootRemap>,
) -> Result<SweepReport, String> {
    let mut l = Locator::new(fs, path, env).with_remaps(remaps);
    let now = crate::locator::now_unix();
    l.now = now;
    let mut out = SweepReport::default();
    let mut all: Vec<FaultReport> = Vec::new();
    // 每个资产本次的实际检出集合，用于清理
    let mut per_asset: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for t in targets {
        let p = resolve(&t.source_file);
        let text = read_source(&p);
        if text.is_none() {
            out.unreadable.push(t.source_file.clone());
        }
        let a = AssetProbe {
            asset_id: t.asset_id.clone(),
            kind: t.kind.clone(),
            asset_name: t.asset_name.clone(),
            source_file: t.source_file.clone(),
            text,
        };
        let reps = l.locate(&a);
        let ids: Vec<String> = reps
            .iter()
            .filter(|r| r.is_failure())
            .map(|r| crate::faults::fault_id_of(&r.asset_id, r.server.as_deref()))
            .collect();
        per_asset.insert(t.asset_id.clone(), ids);
        all.extend(reps);
    }
    out.stored = reg.record_faults(&all)?;
    // 清理必须在落库**之后**：落库会把仍然存在的故障更新成最新状态，
    // 反过来先清理的话，一个刚刚被复检确认的故障会被删掉再插回来，
    // 期间它短暂地不存在——并发读会看到一条不存在的故障。
    for (asset_id, ids) in &per_asset {
        let before = reg.fault_ids_of_asset(asset_id, true)?;
        reg.reconcile_asset_faults(asset_id, ids)?;
        for b in before {
            if !ids.contains(&b) {
                out.cleared.push(b);
            }
        }
    }
    out.locate = LocateReport::from_reports(all);
    Ok(out)
}

/// 从库里取全部 MCP 资产作为巡检目标。
///
/// 只取 mcp：本轮的任务边界是「11 个 MCP 资产的可执行性」。
/// plugin / skill 的检测口径不同（它们没有 command 字段），
/// 硬套同一套判据会产出大量假故障。
pub fn mcp_targets(reg: &Registry) -> Result<Vec<SweepTarget>, String> {
    let rows = reg.query_assets(&crate::store::AssetQuery {
        kind: Some("mcp".to_string()),
        limit: 1000,
        ..Default::default()
    })?;
    Ok(rows
        .into_iter()
        .map(|a| SweepTarget {
            asset_id: a.id.clone(),
            kind: "mcp".to_string(),
            asset_name: a.name.clone(),
            source_file: a.source_path.clone().unwrap_or_default(),
        })
        .collect())
}

/// 取某个资产当前挂着的故障。
pub fn open_faults_of(reg: &Registry, asset_id: &str) -> Result<Vec<StoredFault>, String> {
    reg.list_faults(&crate::faults::FaultQuery {
        asset_id: Some(asset_id.to_string()),
        ..Default::default()
    })
}

/// 便于测试与调用方：直接对一份文本跑检测（不碰磁盘）。
pub fn locate_text(
    fs: &impl ProbeFs,
    path: &impl ProbePath,
    env: &impl ProbeEnv,
    asset_id: &str,
    kind: &str,
    asset_name: &str,
    source_file: &str,
    text: &str,
) -> Vec<FaultReport> {
    let l = Locator::new(fs, path, env);
    l.locate(&AssetProbe {
        asset_id: asset_id.to_string(),
        kind: kind.to_string(),
        asset_name: asset_name.to_string(),
        source_file: source_file.to_string(),
        text: Some(text.to_string()),
    })
}
