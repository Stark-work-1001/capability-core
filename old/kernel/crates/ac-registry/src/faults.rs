//! 故障记录与纳管的持久化。
//!
//! # 与 locator 的分工
//!
//! locator 只管**算**：给它磁盘就能算出一份 FaultReport。本模块只管**存**：
//! 把报告落进 asset_faults，把人的/智能体的动作落进 fault_repairs。
//! 两者刻意不互相调用——算的逻辑要能脱离数据库单测，
//! 存的逻辑要能脱离探测单测。
//!
//! # 当前状态 vs 历史
//!
//! - asset_faults 是**当前状态**：一次检测会整体替换某个资产的记录。
//!   不整体替换的话，已经修好的故障会永远挂在清单上，清单就废了。
//! - fault_repairs 是**历史**：只增不改。认领、修好、判定不可修都记一条。
//!
//! 合并两者会让「当前」和「历史」里必有一个变成谎话，所以不合并。

use crate::locator::FaultReport;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// 故障的当前状态。
///
/// 状态机：pending --claim--> repairing --fixed--> fixed
///                        \--unrepairable--> unrepairable
///
/// fixed / unrepairable 是终态：再认领会报错，而不是把终态悄悄改回 pending。
/// 重新打开一个已修好的故障，只能等下一次检测把它重新报出来——
/// 那才是「它现在还坏着」的证据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultStatus {
    /// 已检出，无人认领。
    Pending,
    /// 已被某个智能体认领，正在修。
    Repairing,
    /// 已修好。
    Fixed,
    /// 判定不可修（例如上游已停止维护）。
    Unrepairable,
}

impl FaultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Repairing => "repairing",
            Self::Fixed => "fixed",
            Self::Unrepairable => "unrepairable",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "repairing" => Some(Self::Repairing),
            "fixed" => Some(Self::Fixed),
            "unrepairable" => Some(Self::Unrepairable),
            _ => None,
        }
    }

    /// 是否终态。终态不能再被认领。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Fixed | Self::Unrepairable)
    }
}

/// 库里的一条故障记录（= FaultReport + 状态 + 认领人）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFault {
    pub fault_id: String,
    pub asset_id: String,
    pub kind: String,
    pub asset_name: String,
    pub server: Option<String>,
    pub failure_class: String,
    /// 主分类 + 其它命中分类（去重、按根因优先序）。
    pub classes: Vec<String>,
    /// 定位详情：field / value / source_file。
    pub location: FaultLocationJson,
    /// 实测证据链（原样保存 locator 的 Evidence 数组）。
    pub evidence: serde_json::Value,
    pub prerequisites: Vec<String>,
    pub suggested_repair_kind: String,
    pub status: String,
    pub claimed_by: String,
    pub claimed_at: Option<i64>,
    pub detected_at: i64,
    pub updated_at: i64,
}

impl StoredFault {
    pub fn status_enum(&self) -> FaultStatus {
        FaultStatus::parse(&self.status).unwrap_or(FaultStatus::Pending)
    }

    /// 一条报告里至少要有这个字段，调用方才可能真的去修。
    /// 缺了它，报告就退化成「有个东西坏了」这种没用的话。
    pub fn is_actionable(&self) -> bool {
        !self.location.field.is_empty() && !self.evidence.is_null()
    }
}

/// 定位详情（与 locator::Location 同形，这里是落库后的形状）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultLocationJson {
    pub field: String,
    pub value: String,
    pub source_file: String,
}

/// 一条修复历史。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairRecord {
    pub id: i64,
    pub fault_id: String,
    pub asset_id: String,
    pub action: String,
    pub actor: String,
    pub note: String,
    pub at: i64,
}

/// 一条已纳管的能力实体（assets/ 目录下真正能跑的那一份）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetEntry {
    pub asset_id: String,
    pub kind: String,
    pub rel_path: String,
    pub entry: String,
    pub source_asset: String,
    pub managed_at: i64,
    pub verified_at: Option<i64>,
    pub note: String,
}

/// 故障清单的查询条件。
#[derive(Debug, Clone, Default)]
pub struct FaultQuery {
    /// 只看某个状态。
    pub status: Option<FaultStatus>,
    /// 只看某个资产。
    pub asset_id: Option<String>,
    /// 只看某个故障类别。
    pub failure_class: Option<String>,
    /// 只看某个认领者。
    pub claimed_by: Option<String>,
}

/// 一次落库的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultStoreReport {
    /// 新写入 / 更新的故障数。
    pub inserted: u32,
    /// 已处理过、本次仍检出，因此只刷新了检测时间的数量。
    pub still_resolved: u32,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn jstr(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

/// 由资产 id 与 server 名推出稳定的 fault_id。
///
/// 稳定是硬要求：认领、修复历史、复检都靠它对齐。
/// 一份配置里多个 server 必须能区分开，否则两个 server 的认领会打架。
pub fn fault_id_of(asset_id: &str, server: Option<&str>) -> String {
    match server {
        Some(s) if !s.is_empty() => format!("{}#{}", asset_id, s),
        _ => asset_id.to_string(),
    }
}

fn row_to_fault(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoredFault> {
    let location_raw: String = r.get("location")?;
    let evidence_raw: String = r.get("evidence")?;
    let classes_raw: String = r.get("classes")?;
    let prereq_raw: String = r.get("prerequisites")?;
    let location: FaultLocationJson =
        serde_json::from_str(&location_raw).unwrap_or_default();
    let classes: Vec<String> = serde_json::from_str(&classes_raw).unwrap_or_default();
    let prerequisites: Vec<String> = serde_json::from_str(&prereq_raw).unwrap_or_default();
    let evidence: serde_json::Value =
        serde_json::from_str(&evidence_raw).unwrap_or(serde_json::Value::Null);
    Ok(StoredFault {
        fault_id: r.get("fault_id")?,
        asset_id: r.get("asset_id")?,
        kind: r.get("kind")?,
        asset_name: r.get("asset_name")?,
        server: r.get("server")?,
        failure_class: r.get("failure_class")?,
        classes,
        location,
        evidence,
        prerequisites,
        suggested_repair_kind: r.get("suggested_repair_kind")?,
        status: r.get("status")?,
        claimed_by: r.get("claimed_by")?,
        claimed_at: r.get("claimed_at")?,
        detected_at: r.get("detected_at")?,
        updated_at: r.get("updated_at")?,
    })
}

const FAULT_COLS: &str = "fault_id, asset_id, kind, asset_name, server, failure_class, \
     classes, location, evidence, prerequisites, suggested_repair_kind, status, \
     claimed_by, claimed_at, detected_at, updated_at";

impl crate::store::Registry {
    /// 把一次检测的结果落库。
    ///
    /// # 语义
    ///
    /// - pending / repairing 的旧记录由调用方先清理（见 reconcile_asset_faults）；
    /// - 已 fixed / unrepairable 的旧记录**保留结论**，只刷新检测时间与定位详情。
    ///   因为「它现在还坏着」是新证据，但「上次谁把它修好了」不该被抹掉。
    pub fn record_faults(&self, reports: &[FaultReport]) -> Result<FaultStoreReport, String> {
        let now = now_unix();
        let mut rep = FaultStoreReport::default();
        for r in reports.iter().filter(|x| x.is_failure()) {
            let fault_id = fault_id_of(&r.asset_id, r.server.as_deref());
            let classes: Vec<String> =
                r.classes().iter().map(|c| c.as_str().to_string()).collect();
            let loc = serde_json::json!({
                "field": r.location.field,
                "value": r.location.value,
                "source_file": r.location.source_file,
            });
            let ev = serde_json::to_value(&r.evidence).unwrap_or(serde_json::Value::Null);
            let pre = serde_json::to_string(&r.prerequisites).unwrap_or_else(|_| "[]".into());
            let cls = serde_json::to_string(&classes).unwrap_or_else(|_| "[]".into());
            // 先看这个故障是不是已经被处理过。
            let prior: Option<String> = self
                .conn
                .query_row(
                    "SELECT status FROM asset_faults WHERE fault_id = ?1",
                    params![fault_id],
                    |x| x.get::<_, String>(0),
                )
                .ok();
            let resolved = matches!(prior.as_deref(), Some("fixed") | Some("unrepairable"));
            if resolved {
                self.conn
                    .execute(
                        "UPDATE asset_faults SET detected_at = ?1, updated_at = ?1, \
                         failure_class = ?2, classes = ?3, location = ?4, evidence = ?5, \
                         prerequisites = ?6, suggested_repair_kind = ?7 \
                         WHERE fault_id = ?8",
                        params![now, r.failure_class.as_str(), cls, jstr(&loc), jstr(&ev), pre,
                                r.suggested_repair_kind.as_str(), fault_id],
                    )
                    .map_err(|e| format!("更新故障失败: {e}"))?;
                rep.still_resolved += 1;
                continue;
            }
            self.conn
                .execute(
                    "INSERT INTO asset_faults (\
                        fault_id, asset_id, kind, asset_name, server, failure_class, classes, \
                        location, evidence, prerequisites, suggested_repair_kind, status, \
                        claimed_by, claimed_at, detected_at, updated_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending','',NULL,?12,?12) \
                     ON CONFLICT(fault_id) DO UPDATE SET \
                        failure_class = excluded.failure_class, \
                        classes       = excluded.classes, \
                        location      = excluded.location, \
                        evidence      = excluded.evidence, \
                        prerequisites = excluded.prerequisites, \
                        suggested_repair_kind = excluded.suggested_repair_kind, \
                        detected_at   = excluded.detected_at, \
                        updated_at    = excluded.updated_at",
                    params![
                        fault_id,
                        r.asset_id,
                        r.kind,
                        r.asset_name,
                        r.server,
                        r.failure_class.as_str(),
                        cls,
                        jstr(&loc),
                        jstr(&ev),
                        pre,
                        r.suggested_repair_kind.as_str(),
                        now
                    ],
                )
                .map_err(|e| format!("写入故障失败: {e}"))?;
            rep.inserted += 1;
        }
        Ok(rep)
    }
}

impl crate::store::Registry {
    /// 清掉该资产名下**本次没再检出**的 pending / repairing 故障。
    ///
    /// 单独一个方法而不是塞进 record_faults：调用方可能只想追加不想清理。
    /// 清理是明确动作——它意味着「上一次说坏的那个，这次实测没坏」。
    pub fn reconcile_asset_faults(
        &self,
        asset_id: &str,
        still_failing: &[String],
    ) -> Result<u32, String> {
        let ids = self.fault_ids_of_asset(asset_id, true)?;
        let mut cleared = 0u32;
        for id in ids {
            if still_failing.iter().any(|s| s == &id) {
                continue;
            }
            self.conn
                .execute("DELETE FROM asset_faults WHERE fault_id = ?1", params![id])
                .map_err(|e| format!("清理已消失的故障失败: {e}"))?;
            self.log_repair(&id, asset_id, "auto_cleared", "locator",
                             "本次检测未再检出该故障")?;
            cleared += 1;
        }
        Ok(cleared)
    }

    /// 该资产名下的 fault_id 列表；only_open 为真时只取 pending / repairing。
    pub fn fault_ids_of_asset(&self, asset_id: &str, only_open: bool) -> Result<Vec<String>, String> {
        let sql = if only_open {
            "SELECT fault_id FROM asset_faults WHERE asset_id = ?1 \
             AND status IN ('pending','repairing') ORDER BY fault_id"
        } else {
            "SELECT fault_id FROM asset_faults WHERE asset_id = ?1 ORDER BY fault_id"
        };
        let mut stmt = self.conn.prepare(sql).map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(params![asset_id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        Ok(it.filter_map(|x| x.ok()).collect())
    }

    /// 列出故障。**默认只列未处理的**：清单的用途是「还有哪些要修」。
    pub fn list_faults(&self, q: &FaultQuery) -> Result<Vec<StoredFault>, String> {
        let mut sql = format!("SELECT {FAULT_COLS} FROM asset_faults WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(s) = q.status {
            sql.push_str(" AND status = ?");
            sql.push_str(&(args.len() + 1).to_string());
            args.push(s.as_str().to_string().into());
        }
        if let Some(a) = &q.asset_id {
            sql.push_str(" AND asset_id = ?");
            sql.push_str(&(args.len() + 1).to_string());
            args.push(a.clone().into());
        }
        if let Some(c) = &q.failure_class {
            sql.push_str(" AND failure_class = ?");
            sql.push_str(&(args.len() + 1).to_string());
            args.push(c.clone().into());
        }
        if let Some(w) = &q.claimed_by {
            sql.push_str(" AND claimed_by = ?");
            sql.push_str(&(args.len() + 1).to_string());
            args.push(w.clone().into());
        }
        sql.push_str(" ORDER BY asset_id, server");
        let mut stmt = self.conn.prepare(&sql).map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), row_to_fault)
            .map_err(|e| e.to_string())?;
        Ok(it.filter_map(|x| x.ok()).collect())
    }

    /// 取一条故障。
    pub fn get_fault(&self, fault_id: &str) -> Result<Option<StoredFault>, String> {
        let sql = format!("SELECT {FAULT_COLS} FROM asset_faults WHERE fault_id = ?1");
        let mut stmt = self.conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut it = stmt
            .query_map(params![fault_id], row_to_fault)
            .map_err(|e| e.to_string())?;
        Ok(it.next().and_then(|x| x.ok()))
    }

    /// 某个资产名下的全部故障（含已处理的）。
    pub fn faults_of_asset(&self, asset_id: &str) -> Result<Vec<StoredFault>, String> {
        self.list_faults(&FaultQuery {
            asset_id: Some(asset_id.to_string()),
            ..Default::default()
        })
    }
}

impl crate::store::Registry {
    /// 记一条修复历史。**只增不改**：这是审计，不是状态。
    pub fn log_repair(
        &self,
        fault_id: &str,
        asset_id: &str,
        action: &str,
        actor: &str,
        note: &str,
    ) -> Result<i64, String> {
        self.conn
            .execute(
                "INSERT INTO fault_repairs (fault_id, asset_id, action, actor, note, at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![fault_id, asset_id, action, actor, note, now_unix()],
            )
            .map_err(|e| format!("写修复历史失败: {e}"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 认领一个故障。
    ///
    /// # 为什么认领必须走一条单独的、带条件的 UPDATE
    ///
    /// 「先读状态再写」在两个智能体同时认领时会双双读到 pending，
    /// 双双写入成功——重复劳动正是这一步要防的。
    /// 所以条件（status 不是终态、认领人为空或就是自己）写在 UPDATE 的
    /// WHERE 里，由 SQLite 保证只有一个能命中。
    pub fn claim_fault(&self, fault_id: &str, agent_id: &str) -> Result<ClaimOutcome, String> {
        if agent_id.trim().is_empty() {
            return Err("认领需要 agent_id".to_string());
        }
        let now = now_unix();
        let cur = self.get_fault(fault_id)?;
        let Some(f) = cur else {
            return Ok(ClaimOutcome::NotFound);
        };
        let st = f.status_enum();
        if st.is_terminal() {
            return Ok(ClaimOutcome::AlreadyResolved(st));
        }
        if !f.claimed_by.is_empty() && f.claimed_by != agent_id {
            return Ok(ClaimOutcome::TakenByOther(f.claimed_by.clone()));
        }
        if f.claimed_by == agent_id && st == FaultStatus::Repairing {
            return Ok(ClaimOutcome::AlreadyYours);
        }
        let n = self
            .conn
            .execute(
                "UPDATE asset_faults SET status = 'repairing', claimed_by = ?1, \
                 claimed_at = ?2, updated_at = ?2 \
                 WHERE fault_id = ?3 AND status IN ('pending','repairing') \
                 AND (claimed_by = '' OR claimed_by = ?1)",
                params![agent_id, now, fault_id],
            )
            .map_err(|e| format!("认领失败: {e}"))?;
        if n == 0 {
            // 条件没命中——说明在我们读完之后别人抢先认领了。
            // 重新读一次，把「被谁抢走了」如实报出来，而不是笼统报失败。
            if let Some(f2) = self.get_fault(fault_id)? {
                if f2.status_enum().is_terminal() {
                    return Ok(ClaimOutcome::AlreadyResolved(f2.status_enum()));
                }
                return Ok(ClaimOutcome::TakenByOther(f2.claimed_by));
            }
            return Ok(ClaimOutcome::NotFound);
        }
        self.log_repair(fault_id, &f.asset_id, "claim", agent_id, "")?;
        Ok(ClaimOutcome::Claimed)
    }

    /// 释放认领（修不完或改主意时用）。只有认领人自己能释放。
    pub fn release_fault(&self, fault_id: &str, agent_id: &str) -> Result<ClaimOutcome, String> {
        let Some(f) = self.get_fault(fault_id)? else {
            return Ok(ClaimOutcome::NotFound);
        };
        if f.claimed_by.is_empty() {
            return Ok(ClaimOutcome::NotClaimed);
        }
        if f.claimed_by != agent_id {
            return Ok(ClaimOutcome::TakenByOther(f.claimed_by.clone()));
        }
        let now = now_unix();
        self.conn
            .execute(
                "UPDATE asset_faults SET status = 'pending', claimed_by = '', \
                 claimed_at = NULL, updated_at = ?1 WHERE fault_id = ?2",
                params![now, fault_id],
            )
            .map_err(|e| format!("释放失败: {e}"))?;
        self.log_repair(fault_id, &f.asset_id, "release", agent_id, "")?;
        Ok(ClaimOutcome::Released)
    }

    /// 结束一个故障：修好 / 判定不可修。
    ///
    /// 只允许认领人（或没人认领时的任意智能体，用于人工判定）来结束。
    pub fn resolve_fault(
        &self,
        fault_id: &str,
        agent_id: &str,
        to: FaultStatus,
        note: &str,
    ) -> Result<ClaimOutcome, String> {
        if !to.is_terminal() {
            return Err(format!("只能结束为 fixed / unrepairable，收到 {}", to.as_str()));
        }
        let Some(f) = self.get_fault(fault_id)? else {
            return Ok(ClaimOutcome::NotFound);
        };
        if !f.claimed_by.is_empty() && f.claimed_by != agent_id {
            return Ok(ClaimOutcome::TakenByOther(f.claimed_by.clone()));
        }
        let now = now_unix();
        self.conn
            .execute(
                "UPDATE asset_faults SET status = ?1, updated_at = ?2 \
                 WHERE fault_id = ?3",
                params![to.as_str(), now, fault_id],
            )
            .map_err(|e| format!("结束故障失败: {e}"))?;
        self.log_repair(fault_id, &f.asset_id, to.as_str(), agent_id, note)?;
        Ok(ClaimOutcome::Resolved(to))
    }

    /// 某个故障的修复历史，最新在前。
    pub fn repair_history(&self, fault_id: &str) -> Result<Vec<RepairRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, fault_id, asset_id, action, actor, note, at \
                 FROM fault_repairs WHERE fault_id = ?1 ORDER BY id DESC",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(params![fault_id], |r| {
                Ok(RepairRecord {
                    id: r.get(0)?,
                    fault_id: r.get(1)?,
                    asset_id: r.get(2)?,
                    action: r.get(3)?,
                    actor: r.get(4)?,
                    note: r.get(5)?,
                    at: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        Ok(it.filter_map(|x| x.ok()).collect())
    }
}

/// 认领 / 结束的结局。
///
/// 刻意做成枚举而不是 bool：调用方需要区分「被谁抢走了」和「这个早修好了」，
/// 两者的下一步动作完全不同，压成一个 false 就没法区分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// **必须是相邻标记（tag + content），不能只写 tag。**
// 只写 tag 的内部标记形式**不支持携带数据的变体**：TakenByOther(String)
// 这类会被 serde 直接拒绝序列化。而 rejection 在调用点被 unwrap_or 吞掉，
// 于是第二个认领者拿到的是一个空对象 {}——看起来像「成功但没内容」，
// 既不报错也没有任何信息。实测踩到过：{outcome:"claimed"} 正常，
// 但 {outcome:"taken_by_other"} 变成了 {}。
#[serde(rename_all = "snake_case", tag = "outcome", content = "detail")]
pub enum ClaimOutcome {
    /// 认领成功。
    Claimed,
    /// 本来就是你的。
    AlreadyYours,
    /// 已被别的智能体认领（附其 id）。
    TakenByOther(String),
    /// 已经是终态（附终态）。
    AlreadyResolved(FaultStatus),
    /// 没人认领，无需释放。
    NotClaimed,
    /// 已释放。
    Released,
    /// 已结束为某终态。
    Resolved(FaultStatus),
    /// 没有这个故障。
    NotFound,
}

impl crate::store::Registry {
    /// 登记一条已纳管的能力实体。
    ///
    /// 调用方负责把文件真的写进 assets/ 目录；本方法只登记事实。
    /// 顺序刻意是「先写文件、后登记」：先登记后写文件的话，
    /// 任何一步失败都会留下一条指向空目录的纳管记录，
    /// 而那种记录比没有记录更糟——它会让人以为已经纳管好了。
    pub fn upsert_asset_entry(&self, e: &AssetEntry) -> Result<(), String> {
        if !matches!(e.kind.as_str(), "mcp" | "skill") {
            return Err(format!("纳管类型只能是 mcp / skill，收到 {}", e.kind));
        }
        if e.rel_path.trim().is_empty() {
            return Err("纳管条目缺少 rel_path".to_string());
        }
        self.conn
            .execute(
                "INSERT INTO asset_entries \
                 (asset_id, kind, rel_path, entry, source_asset, managed_at, verified_at, note) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8) \
                 ON CONFLICT(asset_id) DO UPDATE SET \
                   kind = excluded.kind, rel_path = excluded.rel_path, \
                   entry = excluded.entry, source_asset = excluded.source_asset, \
                   managed_at = excluded.managed_at, verified_at = excluded.verified_at, \
                   note = excluded.note",
                params![
                    e.asset_id,
                    e.kind,
                    e.rel_path,
                    e.entry,
                    e.source_asset,
                    e.managed_at,
                    e.verified_at,
                    e.note
                ],
            )
            .map_err(|e| format!("登记纳管失败: {e}"))?;
        Ok(())
    }

    /// 取消纳管。**只删登记，不动磁盘上的文件。**
    ///
    /// 删文件是不可逆动作，本中枢没有权限替用户决定它该不该留。
    /// 需要清理磁盘时，由外部智能体在其职权内执行。
    pub fn remove_asset_entry(&self, asset_id: &str) -> Result<bool, String> {
        let n = self
            .conn
            .execute("DELETE FROM asset_entries WHERE asset_id = ?1", params![asset_id])
            .map_err(|e| format!("取消纳管失败: {e}"))?;
        Ok(n > 0)
    }

    /// 取一条纳管记录。
    pub fn get_asset_entry(&self, asset_id: &str) -> Result<Option<AssetEntry>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT asset_id, kind, rel_path, entry, source_asset, \
                 managed_at, verified_at, note FROM asset_entries WHERE asset_id = ?1",
            )
            .map_err(|e| e.to_string())?;
        let mut it = stmt
            .query_map(params![asset_id], |r| {
                Ok(AssetEntry {
                    asset_id: r.get(0)?,
                    kind: r.get(1)?,
                    rel_path: r.get(2)?,
                    entry: r.get(3)?,
                    source_asset: r.get(4)?,
                    managed_at: r.get(5)?,
                    verified_at: r.get(6)?,
                    note: r.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?;
        Ok(it.next().and_then(|x| x.ok()))
    }

    /// 列出纳管实体，可按 mcp / skill 过滤。
    pub fn list_assets(&self, kind: Option<&str>) -> Result<Vec<AssetEntry>, String> {
        let rows = |sql: &str, p: Vec<rusqlite::types::Value>| -> Result<Vec<AssetEntry>, String> {
            let mut stmt = self.conn.prepare(sql).map_err(|e| e.to_string())?;
            let it = stmt
                .query_map(rusqlite::params_from_iter(p.iter()), |r| {
                    Ok(AssetEntry {
                        asset_id: r.get(0)?,
                        kind: r.get(1)?,
                        rel_path: r.get(2)?,
                        entry: r.get(3)?,
                        source_asset: r.get(4)?,
                        managed_at: r.get(5)?,
                        verified_at: r.get(6)?,
                        note: r.get(7)?,
                    })
                })
                .map_err(|e| e.to_string())?;
            Ok(it.filter_map(|x| x.ok()).collect())
        };
        let cols = "asset_id, kind, rel_path, entry, source_asset, managed_at, verified_at, note";
        match kind {
            Some(k) => rows(
                &format!("SELECT {cols} FROM asset_entries WHERE kind = ?1 ORDER BY asset_id"),
                vec![k.to_string().into()],
            ),
            None => rows(
                &format!("SELECT {cols} FROM asset_entries ORDER BY kind, asset_id"),
                vec![],
            ),
        }
    }

    /// 把一个资产标记为「已纳管」，并清掉它名下仍然挂着的未处理故障。
    ///
    /// 「纳管」的定义是：**纳管实体实测可用，且它已经能被中枢识别**。
    /// 所以已纳管等于这一路的故障处理完了——但历史里那几条故障记录不删，
    /// 它们是「当初为什么需要纳管」的证据。
    pub fn mark_managed(&self, asset_id: &str, actor: &str) -> Result<u32, String> {
        let ids = self.fault_ids_of_asset(asset_id, true)?;
        let mut resolved = 0u32;
        for id in ids {
            let out = self.resolve_fault(
                &id,
                actor,
                FaultStatus::Fixed,
                "已写入 assets/ 并以纳管实体替代原故障入口",
            )?;
            if matches!(out, ClaimOutcome::Resolved(_)) {
                resolved += 1;
            }
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locator::{
        Evidence, FailureClass, FaultReport, Location,
    };

    fn reg() -> crate::store::Registry {
        crate::store::Registry::open_in_memory().expect("内存库")
    }

    fn report(asset: &str, server: &str, class: FailureClass) -> FaultReport {
        FaultReport {
            asset_id: asset.to_string(),
            kind: "mcp".to_string(),
            asset_name: asset.trim_start_matches("mcp::").to_string(),
            server: Some(server.to_string()),
            failure_class: class,
            also_detected: Vec::new(),
            location: Location {
                field: format!("mcpServers.{server}.command"),
                value: "D:/.my-agent/x.exe".to_string(),
                source_file: format!("vendor/{asset}/.mcp.json"),
            },
            evidence: vec![Evidence::probed_exists(
                "根目录实测不存在",
                "D:/.my-agent",
                false,
            )],
            prerequisites: vec!["需要知道新根位置".to_string()],
            suggested_repair_kind: class.suggested_repair_kind(),
            detected_at: 1_700_000_000,
        }
    }

    #[test]
    fn record_then_read_roundtrips_every_field() {
        let r = reg();
        let rep = r
            .record_faults(&[report("mcp::a", "s1", FailureClass::DeadRoot)])
            .unwrap();
        assert_eq!(rep.inserted, 1);
        let f = r.get_fault("mcp::a#s1").unwrap().expect("应有记录");
        assert_eq!(f.failure_class, "dead_root");
        assert_eq!(f.status, "pending");
        assert_eq!(f.location.field, "mcpServers.s1.command");
        assert_eq!(f.prerequisites.len(), 1);
        assert_eq!(f.suggested_repair_kind, "path_remap");
        assert!(f.claimed_by.is_empty());
        // 证据必须能原样读回来，而不是被压成一句话
        assert_eq!(f.evidence[0]["exists"], serde_json::json!(false));
    }

    #[test]
    fn healthy_reports_are_not_stored() {
        let r = reg();
        let rep = r
            .record_faults(&[report("mcp::ok", "s", FailureClass::Healthy)])
            .unwrap();
        assert_eq!(rep.inserted, 0);
        assert!(r.list_faults(&FaultQuery::default()).unwrap().is_empty());
    }

    #[test]
    fn two_servers_under_one_asset_get_distinct_rows() {
        let r = reg();
        r.record_faults(&[
            report("mcp::x", "a", FailureClass::DeadRoot),
            report("mcp::x", "b", FailureClass::CommandNotInPath),
        ])
        .unwrap();
        let fs = r.faults_of_asset("mcp::x").unwrap();
        assert_eq!(fs.len(), 2);
        let ids: Vec<&str> = fs.iter().map(|f| f.fault_id.as_str()).collect();
        assert!(ids.contains(&"mcp::x#a"));
        assert!(ids.contains(&"mcp::x#b"));
    }

    #[test]
    fn claim_is_exclusive_and_a_second_claimant_is_told_who_has_it() {
        let r = reg();
        r.record_faults(&[report("mcp::a", "s", FailureClass::DeadRoot)])
            .unwrap();
        let id = "mcp::a#s";
        assert_eq!(r.claim_fault(id, "agent-1").unwrap(), ClaimOutcome::Claimed);
        // 第二个智能体来认领：必须被告知被谁拿走了，而不是悄悄失败
        assert_eq!(
            r.claim_fault(id, "agent-2").unwrap(),
            ClaimOutcome::TakenByOther("agent-1".to_string())
        );
        // 同一个智能体重复认领是幂等的
        assert_eq!(r.claim_fault(id, "agent-1").unwrap(), ClaimOutcome::AlreadyYours);
        let f = r.get_fault(id).unwrap().unwrap();
        assert_eq!(f.status, "repairing");
        assert_eq!(f.claimed_by, "agent-1");
    }

    #[test]
    fn a_terminal_fault_cannot_be_claimed() {
        let r = reg();
        r.record_faults(&[report("mcp::a", "s", FailureClass::DeadRoot)])
            .unwrap();
        let id = "mcp::a#s";
        r.claim_fault(id, "agent-1").unwrap();
        r.resolve_fault(id, "agent-1", FaultStatus::Fixed, "修好了")
            .unwrap();
        assert_eq!(
            r.claim_fault(id, "agent-2").unwrap(),
            ClaimOutcome::AlreadyResolved(FaultStatus::Fixed)
        );
    }

    #[test]
    fn only_the_claimant_can_resolve() {
        let r = reg();
        r.record_faults(&[report("mcp::a", "s", FailureClass::DeadRoot)])
            .unwrap();
        let id = "mcp::a#s";
        r.claim_fault(id, "agent-1").unwrap();
        assert_eq!(
            r.resolve_fault(id, "agent-2", FaultStatus::Fixed, "").unwrap(),
            ClaimOutcome::TakenByOther("agent-1".to_string())
        );
    }

    #[test]
    fn resolving_requires_a_terminal_status() {
        let r = reg();
        r.record_faults(&[report("mcp::a", "s", FailureClass::DeadRoot)])
            .unwrap();
        let err = r
            .resolve_fault("mcp::a#s", "agent-1", FaultStatus::Repairing, "")
            .unwrap_err();
        assert!(err.contains("fixed") && err.contains("unrepairable"));
    }

    #[test]
    fn release_puts_it_back_to_pending_and_clears_the_claimant() {
        let r = reg();
        r.record_faults(&[report("mcp::a", "s", FailureClass::DeadRoot)])
            .unwrap();
        let id = "mcp::a#s";
        r.claim_fault(id, "agent-1").unwrap();
        assert_eq!(r.release_fault(id, "agent-1").unwrap(), ClaimOutcome::Released);
        let f = r.get_fault(id).unwrap().unwrap();
        assert_eq!(f.status, "pending");
        assert!(f.claimed_by.is_empty());
        assert!(f.claimed_at.is_none());
    }

    #[test]
    fn every_state_change_lands_in_the_repair_history() {
        let r = reg();
        r.record_faults(&[report("mcp::a", "s", FailureClass::DeadRoot)])
            .unwrap();
        let id = "mcp::a#s";
        r.claim_fault(id, "agent-1").unwrap();
        r.resolve_fault(id, "agent-1", FaultStatus::Fixed, "换了路径")
            .unwrap();
        let h = r.repair_history(id).unwrap();
        // 最新在前
        assert_eq!(h[0].action, "fixed");
        assert_eq!(h[1].action, "claim");
        assert!(h[0].note.contains("换了路径"));
    }

    #[test]
    fn a_fixed_fault_is_not_erased_by_the_next_detection_but_keeps_its_verdict() {
        // 这条是「当前状态 vs 历史」的核心：已修好的故障不该被下一次
        // 检测静默改回 pending，但它的定位详情要跟着更新。
        let r = reg();
        r.record_faults(&[report("mcp::a", "s", FailureClass::DeadRoot)])
            .unwrap();
        let id = "mcp::a#s";
        r.claim_fault(id, "agent-1").unwrap();
        r.resolve_fault(id, "agent-1", FaultStatus::Fixed, "").unwrap();
        let rep = r
            .record_faults(&[report("mcp::a", "s", FailureClass::LaunchPathMissing)])
            .unwrap();
        assert_eq!(rep.still_resolved, 1);
        let f = r.get_fault(id).unwrap().unwrap();
        assert_eq!(f.status, "fixed", "结论不该被下一次检测抹掉");
        assert_eq!(f.failure_class, "launch_path_missing", "定位详情要更新");
    }

    #[test]
    fn reconcile_clears_only_the_faults_that_are_no_longer_detected() {
        let r = reg();
        r.record_faults(&[
            report("mcp::a", "s1", FailureClass::DeadRoot),
            report("mcp::a", "s2", FailureClass::DeadRoot),
        ])
        .unwrap();
        let cleared = r
            .reconcile_asset_faults("mcp::a", &["mcp::a#s1".to_string()])
            .unwrap();
        assert_eq!(cleared, 1);
        let ids = r.fault_ids_of_asset("mcp::a", false).unwrap();
        assert_eq!(ids, vec!["mcp::a#s1".to_string()]);
        // 清理也要留痕，否则「这条故障怎么没了」永远查不到
        let h = r.repair_history("mcp::a#s2").unwrap();
        assert_eq!(h[0].action, "auto_cleared");
    }

    #[test]
    fn list_faults_can_be_filtered_by_status_class_and_claimant() {
        let r = reg();
        r.record_faults(&[
            report("mcp::a", "s", FailureClass::DeadRoot),
            report("mcp::b", "s", FailureClass::CommandNotInPath),
        ])
        .unwrap();
        r.claim_fault("mcp::a#s", "agent-1").unwrap();
        assert_eq!(
            r.list_faults(&FaultQuery { status: Some(FaultStatus::Repairing), ..Default::default() })
                .unwrap().len(),
            1
        );
        assert_eq!(
            r.list_faults(&FaultQuery {
                failure_class: Some("command_not_in_path".to_string()),
                ..Default::default()
            }).unwrap().len(),
            1
        );
        assert_eq!(
            r.list_faults(&FaultQuery {
                claimed_by: Some("agent-1".to_string()),
                ..Default::default()
            }).unwrap().len(),
            1
        );
    }

    #[test]
    fn asset_entries_roundtrip_and_reject_unknown_kinds() {
        let r = reg();
        let e = AssetEntry {
            asset_id: "mcp::obsidian".to_string(),
            kind: "mcp".to_string(),
            rel_path: "mcp/obsidian".to_string(),
            entry: "run.cmd".to_string(),
            source_asset: "vendor/plugin/obsidian-suite".to_string(),
            managed_at: 1,
            verified_at: None,
            note: String::new(),
        };
        r.upsert_asset_entry(&e).unwrap();
        let got = r.get_asset_entry("mcp::obsidian").unwrap().unwrap();
        assert_eq!(got.rel_path, "mcp/obsidian");
        assert_eq!(r.list_assets(Some("mcp")).unwrap().len(), 1);
        assert_eq!(r.list_assets(Some("skill")).unwrap().len(), 0);
        let mut bad = e.clone();
        bad.asset_id = "mcp::bad".to_string();
        bad.kind = "exe".to_string();
        assert!(r.upsert_asset_entry(&bad).is_err());
    }

    #[test]
    fn mark_managed_closes_the_open_faults_and_keeps_the_history() {
        let r = reg();
        r.record_faults(&[report("mcp::a", "s", FailureClass::LaunchPathMissing)])
            .unwrap();
        let n = r.mark_managed("mcp::a", "agent-1").unwrap();
        assert_eq!(n, 1);
        let f = r.get_fault("mcp::a#s").unwrap().unwrap();
        assert_eq!(f.status, "fixed");
        assert_eq!(r.repair_history("mcp::a#s").unwrap()[0].action, "fixed");
    }

    #[test]
    fn removing_an_assets_entry_does_not_touch_any_fault_history() {
        let r = reg();
        r.record_faults(&[report("mcp::a", "s", FailureClass::DeadRoot)])
            .unwrap();
        r.upsert_asset_entry(&AssetEntry {
            asset_id: "mcp::a".to_string(),
            kind: "mcp".to_string(),
            rel_path: "mcp/a".to_string(),
            entry: String::new(),
            source_asset: String::new(),
            managed_at: 1,
            verified_at: None,
            note: String::new(),
        })
        .unwrap();
        assert!(r.remove_asset_entry("mcp::a").unwrap());
        assert!(r.get_fault("mcp::a#s").unwrap().is_some(), "取消纳管不该动故障记录");
    }
}