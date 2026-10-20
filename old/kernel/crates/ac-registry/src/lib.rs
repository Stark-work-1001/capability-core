//! # ac-registry
//!
//! 资产注册：把 `vendor/` 下六类异构资产解析成统一的 `CapabilityManifest`，
//! 推断身份维度，写入 SQLite。
//!
//! ## 模块
//! - [`agents`] —— M3 智能体接入：A/B/C 三通道与三张新表
//! - [`diagnostics`] —— M3 维修站：故障分类 / 能否自修 / 是否需求援 LLM
//! - [`frontmatter`] —— YAML frontmatter 最小解析器（无外部依赖）
//! - [`infer`] —— 身份维度推断
//! - [`parse`] —— SKILL.md / plugin.json / .mcp.json 三种格式解析
//! - [`scan`] —— 目录递归扫描
//! - [`store`] —— SQLite 存储
//! - [`retrieval_impl`] —— `AssetStore` trait 的 registry 侧实现（M2）
//! - [`ingest`] —— 扫描入库编排 + 全量对账

pub mod agents;
pub mod assets;
/// v8：资产作者与「作者出处」判定（manifest / frontmatter / package）。
pub mod author;
pub mod diagnostics;
pub mod faults;
pub mod fingerprint;
pub mod formats;
pub mod frontmatter;
/// v8：智能体协议注册表（宿主清单 + 协议矩阵 + 探测 + 资产↔宿主支持）。
pub mod hosts;
pub mod infer;
pub mod ingest;
pub mod locator;
/// 来源（origin）与注册方式（registration）判定：用户点名的两个维度。
pub mod origin;
pub mod operations;
pub mod presence;
pub mod parse;
pub mod protocol;
pub mod retrieval_impl;
pub mod scan;
pub mod store;
pub mod sweep;
/// 模型分工：任务节点 → 执行者（脚本 / 模型 / 混合）的路由判定表。
///
/// 它是**静态目录**：不读库、不碰磁盘、不调模型。用户要的「分工」先要能
/// 被逐条讲清「谁来干、干到什么程度」，才谈得上接 provider。
pub mod tasks;

#[cfg(test)]
mod db_safety_guard;
#[cfg(test)]
mod testutil;

pub use asset_store::{AssetKind, AssetLayoutReport, AssetRoot, AssetWriteReport};
pub use agents::{
    AgentOverview, AgentRecord, AgentRegistration, EcosystemCount, HeartbeatScanReport,
    StaticRegistrationReport, UsageRow,
};
pub use diagnostics::{Diagnostic, DiagnosticCategory, DiagnosticReport, Repairability};
pub use faults::{
    fault_id_of, AssetEntry, ClaimOutcome, FaultLocationJson, FaultQuery, FaultStatus,
    FaultStoreReport, RepairRecord, StoredFault,
};
pub use locator::{
    AssetVerdict, Evidence, EvidenceKind, FailureClass, FaultReport, LocateReport, Location,
    SuggestedRepairKind,
};
pub use ingest::{ingest, IngestReport};
pub use protocol::{ProtocolCheck, ProtocolMatrix, ProtocolRow};
pub use parse::{parse_asset, IdScope, ParseOutcome};
pub use scan::{scan_dir, ScanReport};
pub use origin::{RegistrationKind, OriginKind, OriginProfile};
pub use store::{AssetQuery, AssetSummary, DedupRecord, KindCount, Registry, TagCount};
pub use sweep::{mcp_targets, open_faults_of, sweep, SweepReport, SweepTarget};
pub use tasks::{
    catalog, route_repair_action, CapabilityTask, ModelContract, RepairRoute, RequirementMapping,
    TaskCatalog, TaskCounts, TaskExecutor, TaskStatus,
};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");
