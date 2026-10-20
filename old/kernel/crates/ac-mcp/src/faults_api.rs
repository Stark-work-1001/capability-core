//! 故障定位与纳管的对外接口层。
//!
//! # 为什么单独一个模块，而不是塞进 gateway
//!
//! gateway 的**只读承诺**是硬契约（lib.rs 里有守着它的测试）。
//! 故障这一块有一半是只读（列清单、查详情、取修复任务单），
//! 另一半会写库（认领、释放、结束）。
//! 混进 gateway 就必须在 gateway 里开一个写口子，
//! 那条只读守门就成了「带例外的一刀切」——一旦有了例外，
//! 下一次加写操作时没人还会去核对边界。
//!
//! 这里写的是**协调元数据**（asset_faults / fault_repairs / asset_entries），
//! 与 agents / agent_capability_usage 同一类：不碰 assets / capabilities。
//! 这条边界由 guard_faults_api_never_touches_asset_tables 守着。

use crate::gateway::GatewayError;
use crate::service::CapabilityService;
use ac_registry::faults::{ClaimOutcome, FaultQuery, FaultStatus, StoredFault};
use ac_registry::locator::LocateReport;
use serde::{Deserialize, Serialize};

/// 一份给智能体直接接手的修复任务单。
///
/// 与「故障清单」的差别在**读者**：清单是给人看的概览，
/// 任务单是给智能体看的作业。所以每一条都必须带上
/// 它能据以动手的全部信息，而不只是「哪里坏了」。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairTask {
    /// 认领 / 结束这个任务时用的 id。
    pub fault_id: String,
    pub asset_id: String,
    pub asset_name: String,
    pub failure_class: String,
    /// 同一处还命中的其它类别。只看主分类会漏掉一半事实。
    pub also_detected: Vec<String>,
    /// 改哪一行。
    pub location: ac_registry::faults::FaultLocationJson,
    /// 凭什么判定它坏了（实测证据链）。
    pub evidence: serde_json::Value,
    /// 动手前必须先知道什么。
    pub prerequisites: Vec<String>,
    /// **建议**的维修大类。不是指令：最终由智能体自己判断。
    pub suggested_repair_kind: String,
    pub status: String,
    pub claimed_by: String,
    /// 做这件事的边界。**写在任务单里**，而不是让下游自己读文档猜。
    pub boundaries: Vec<String>,
}

/// 一次修复任务单。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairTaskList {
    /// 现在还有几条没处理。
    pub open: u32,
    /// 已被认领、正在修的有几条。
    pub in_progress: u32,
    pub tasks: Vec<RepairTask>,
    /// 中枢这一侧的职责边界。整份任务单共享一段，不必每条重复。
    pub central_agent_scope: Vec<String>,
}

/// 一条修复任务里给智能体看的边界。
fn scope_of_repair() -> Vec<String> {
    vec![
        "中枢只做检测与定位，不猜怎么修；维修动作由你（外部智能体）决定并执行".into(),
        "suggested_repair_kind 是建议，不是指令；你可以完全不采纳".into(),
        "vendor/ 与用户的原始文件不在中枢的写入范围内；改动须由你在自己的职权内完成".into(),
        "改完请把可用实体写进 assets/，并在中枢登记纳管，免得下一次巡检又把它报出来".into(),
        "最后一步：修好后调用 resolve 把该 fault_id 结掉，只传 fault_id 与本智能体 id".into(),
    ]
}

fn central_scope() -> Vec<String> {
    vec![
        "中枢（capability-core）的职责边界：检测、定位、落库、对外暴露".into(),
        "中枢绝不自动修改 vendor/ 或用户的原始文件".into(),
        "认领是排他的：同一个故障同一时刻只有一个智能体能持有".into(),
        "已结束（fixed / unrepairable）的故障不会被下一次检测静默改回待修".into(),
    ]
}

/// 一次巡检的对外结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SweepSummary {
    pub total_units: u32,
    pub healthy: u32,
    pub failed: u32,
    pub by_class: std::collections::BTreeMap<String, u32>,
    pub by_any_class: std::collections::BTreeMap<String, u32>,
    pub assets: Vec<ac_registry::locator::AssetVerdict>,
    pub stored: ac_registry::faults::FaultStoreReport,
    pub cleared: Vec<String>,
    pub unreadable: Vec<String>,
}

impl SweepSummary {
    pub fn from_report(r: &ac_registry::sweep::SweepReport) -> Self {
        Self {
            total_units: r.locate.total_units,
            healthy: r.locate.healthy,
            failed: r.locate.failed,
            by_class: r.locate.by_class.clone(),
            by_any_class: r.locate.by_any_class.clone(),
            assets: r.locate.asset_rollup(),
            stored: r.stored.clone(),
            cleared: r.cleared.clone(),
            unreadable: r.unreadable.clone(),
        }
    }
}

/// 检测结果转 JSON（给验收用：要看得到每一条字段）。
pub fn locate_report_json(r: &LocateReport) -> serde_json::Value {
    serde_json::to_value(r).unwrap_or(serde_json::Value::Null)
}

impl CapabilityService {
    /// 列出故障。
    pub fn list_faults(&self, q: &FaultQuery) -> Result<Vec<StoredFault>, GatewayError> {
        // with_registry 的外层是锁中毒，内层是 SQL 失败；两者都要如实变成
        // Unavailable，而不是当成「查不到」——查不到和库坏了是两件事。
        let inner = self.gateway().with_registry(|g| g.list_faults(q))?;
        inner.map_err(GatewayError::Unavailable)
    }

    /// 取一条故障详情（含修复历史）。
    pub fn fault_detail(
        &self,
        fault_id: &str,
    ) -> Result<Option<(StoredFault, Vec<ac_registry::faults::RepairRecord>)>, GatewayError> {
        let inner = self.gateway().with_registry(|g| -> Result<_, String> {
            match g.get_fault(fault_id)? {
                Some(f) => {
                    let h = g.repair_history(fault_id)?;
                    Ok(Some((f, h)))
                }
                None => Ok(None),
            }
        })?;
        inner.map_err(GatewayError::Unavailable)
    }

    /// 某个资产名下的全部故障（含已处理的）。
    pub fn faults_of_asset(&self, asset_id: &str) -> Result<Vec<StoredFault>, GatewayError> {
        let inner = self.gateway().with_registry(|g| g.faults_of_asset(asset_id))?;
        inner.map_err(GatewayError::Unavailable)
    }

    /// 生成给智能体的修复任务单。
    ///
    /// 只收**未处理**的故障：任务单的语义是「这些还没人动手」，
    /// 把已修好的也塞进来会让它变成一个没人愿意读的长清单。
    pub fn repair_tasks(&self) -> Result<RepairTaskList, GatewayError> {
        let open = self.list_faults(&FaultQuery {
            status: Some(FaultStatus::Pending),
            ..Default::default()
        })?;
        let wip = self.list_faults(&FaultQuery {
            status: Some(FaultStatus::Repairing),
            ..Default::default()
        })?;
        let mut tasks: Vec<RepairTask> = Vec::new();
        let mut push = |f: &StoredFault| {
            let also: Vec<String> = f
                .classes
                .iter()
                .filter(|c| **c != f.failure_class && **c != "healthy")
                .cloned()
                .collect();
            tasks.push(RepairTask {
                fault_id: f.fault_id.clone(),
                asset_id: f.asset_id.clone(),
                asset_name: f.asset_name.clone(),
                failure_class: f.failure_class.clone(),
                also_detected: also,
                location: f.location.clone(),
                evidence: f.evidence.clone(),
                prerequisites: f.prerequisites.clone(),
                suggested_repair_kind: f.suggested_repair_kind.clone(),
                status: f.status.clone(),
                claimed_by: f.claimed_by.clone(),
                boundaries: scope_of_repair(),
            });
        };
        for f in &open {
            push(f);
        }
        for f in &wip {
            push(f);
        }
        tasks.sort_by(|a, b| {
            a.failure_class
                .cmp(&b.failure_class)
                .then_with(|| a.asset_id.cmp(&b.asset_id))
        });
        Ok(RepairTaskList {
            open: open.len() as u32,
            in_progress: wip.len() as u32,
            tasks,
            central_agent_scope: central_scope(),
        })
    }
}

impl CapabilityService {
    /// 认领一个故障。
    pub fn claim_fault(&self, fault_id: &str, agent_id: &str) -> Result<ClaimOutcome, String> {
        self.require_registered(agent_id)?;
        self.gateway()
            .with_registry(|g| g.claim_fault(fault_id, agent_id))
            .map_err(|e| e.to_string())?
    }

    /// 释放认领。
    pub fn release_fault(&self, fault_id: &str, agent_id: &str) -> Result<ClaimOutcome, String> {
        self.require_registered(agent_id)?;
        self.gateway()
            .with_registry(|g| g.release_fault(fault_id, agent_id))
            .map_err(|e| e.to_string())?
    }

    /// 结束一个故障（fixed / unrepairable）。
    pub fn resolve_fault(
        &self,
        fault_id: &str,
        agent_id: &str,
        to: FaultStatus,
        note: &str,
    ) -> Result<ClaimOutcome, String> {
        self.require_registered(agent_id)?;
        self.gateway()
            .with_registry(|g| g.resolve_fault(fault_id, agent_id, to, note))
            .map_err(|e| e.to_string())?
    }

    /// 认领者必须是**已注册**的智能体。
    ///
    /// 与 M2.5 的语义一致：不留一个「随便写个 id 就能认领」的口子。
    /// 否则认领表里会堆满拼错的名字，而「谁在修」也就无从审计。
    fn require_registered(&self, agent_id: &str) -> Result<(), String> {
        if agent_id.trim().is_empty() {
            return Err("认领 / 结束故障需要 agent_id".to_string());
        }
        match self.resolve_caller(agent_id) {
            crate::service::CallerResolution::Resolved(_) => Ok(()),
            crate::service::CallerResolution::Unregistered(a) => {
                Err(format!("智能体 {a} 未注册：请先 assets-serve register --id {a}"))
            }
            crate::service::CallerResolution::Unavailable(m) => Err(m),
        }
    }
}

#[cfg(test)]
mod tests {
    /// 只读承诺的边界：本模块**不得**碰资产表。
    ///
    /// 与 lib.rs 里守着 gateway 的那条同一个思路，但盯的是不同的表：
    /// gateway 一个写操作都不许有；本模块许写协调元数据，
    /// 但**绝不许**碰 assets / capabilities。
    #[test]
    fn guard_faults_api_never_touches_asset_tables() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/faults_api.rs"
        ))
        .expect("读 faults_api.rs");
        let code: String = src
            .lines()
            .map(|l| {
                let t = l.trim_start();
                if t.starts_with("//") {
                    String::new()
                } else {
                    t.split("//").next().unwrap_or("").to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        // 关键词**在运行时拼出来**，不写成完整字面量：
        // 写成字面量的话，这段守门自己就命中了它要拦的东西，
        // 于是它永远报红——一个永远报红的守门等于没有守门。
        // （gateway 那条读的是另一个文件，所以没这个问题。）
        let verbs = ["INSERT", "UPDATE", "DELETE"];
        let tables = ["assets", "capabilities"];
        for v in verbs {
            for t in tables {
                let needle = format!("{v} INTO {t}");
                let needle2 = format!("{v} {t}");
                for n in [needle, needle2] {
                    assert!(
                        !code.contains(&n),
                        "故障接口不得改动用户资产：faults_api.rs 出现了 {n}"
                    );
                }
            }
        }
    }
}