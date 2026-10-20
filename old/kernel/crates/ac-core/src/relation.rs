//! 资产关系（L5）—— 智能体层级与委派链。
//!
//! 人类组织架构只是这里的一个**类比**。真正的模型是：
//! 领导智能体向子智能体派发任务，能力沿链收窄，
//! 任何一跳都不能自行扩大权限。

use serde::{Deserialize, Serialize};

/// 关系类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// 领导 → 直属子智能体（编队关系）
    LeaderToSub,
    /// 委派（任务下发）
    Delegates,
    /// 运行时依赖
    Requires,
    /// 能力扩展（Plugin 内含 Skill 等）
    Extends,
    /// 互斥（同层不可同时启用）
    Conflicts,
    /// 所属（资产归属某个组织）
    BelongsTo,
}

/// 一条有向关系。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssetRelation {
    pub kind: RelationKind,
    /// 目标资产 ID
    pub target_id: String,
    /// 委派时的权限收窄说明（自然语言，记录收窄了什么）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_narrowing: Option<String>,
    /// 环检测用的深度上限，防止 A→B→A
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u8>,
}

impl AssetRelation {
    pub fn new(kind: RelationKind, target_id: impl Into<String>) -> Self {
        Self {
            kind,
            target_id: target_id.into(),
            scope_narrowing: None,
            max_depth: Some(8),
        }
    }

    /// 是否参与"智能体编队"计算（其余关系只用于索引，不进委派图）
    pub fn is_hierarchy(&self) -> bool {
        matches!(self.kind, RelationKind::LeaderToSub | RelationKind::Delegates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hierarchy_relations_enter_delegation_graph() {
        assert!(AssetRelation::new(RelationKind::LeaderToSub, "a").is_hierarchy());
        assert!(AssetRelation::new(RelationKind::Delegates, "a").is_hierarchy());
        assert!(!AssetRelation::new(RelationKind::Requires, "a").is_hierarchy());
        assert!(!AssetRelation::new(RelationKind::BelongsTo, "a").is_hierarchy());
    }

    #[test]
    fn relation_roundtrip() {
        let r = AssetRelation {
            kind: RelationKind::Delegates,
            target_id: "sub-1".into(),
            scope_narrowing: Some("只读子集".into()),
            max_depth: Some(3),
        };
        let j = serde_json::to_string(&r).unwrap();
        let back: AssetRelation = serde_json::from_str(&j).unwrap();
        assert_eq!(r, back);
    }
}
