//! # ac-core
//!
//! CapabilityCore 的核心类型定义与 Capability Manifest 协议。
//!
//! 这一层是**协议层**，不依赖任何 IO、数据库或检索实现，
//! 保证 `ac-registry` / `ac-retrieval` / `ac-llm` / `ac-mcp` 可以完全解耦演进。

pub mod manifest;
pub mod identity;
pub mod relation;
pub mod dedup;

pub use manifest::{
    AssetKind, CapabilityManifest, ProvidedCapability, Provenance, ManifestError,
};
pub use identity::{CapabilityIdentity, Authority, SideEffect, ToolPosture};
pub use relation::{AssetRelation, RelationKind};
pub use dedup::{
    AssetLocation, DedupCandidate, DedupDecision, LocationRole, LogicalAsset, MatchLevel,
};

/// 协议版本。破坏性变更时递增。
pub const SCHEMA_VERSION: &str = "1.0";
