//! 多维度身份描述（L4）。
//!
//! 这里的"身份"**不是**用户身份，也不是 OAuth 主体，
//! 而是**智能体在执行任务过程中的行为身份** —— 它能做什么、
//! 有多大权限、会产生什么副作用、面对工具是什么姿态。
//!
//! 设计意图：让"该不该把某个能力给某个智能体"变成一次可计算的比较，
//! 而不是一次人工判断。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 权限级别。沿 ADCS 委派链逐跳收窄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// 只能读，不能写
    Read,
    /// 可执行，但副作用受限
    #[default]
    Observe,
    /// 可写入（默认）
    Write,
    /// 可执行破坏性操作
    Destructive,
}

impl Authority {
    /// 稳定的字符串形式。**与 serde 的 snake_case 必须一致**——
    /// 库里存的是这个字符串，检索层要拿它做 SQL 过滤条件。
    pub const fn as_str(self) -> &'static str {
        match self {
            Authority::Read => "read",
            Authority::Observe => "observe",
            Authority::Write => "write",
            Authority::Destructive => "destructive",
        }
    }

    /// 供 UI / IPC 使用的展示名。
    pub const fn label_zh(self) -> &'static str {
        match self {
            Authority::Read => "只读",
            Authority::Observe => "观察",
            Authority::Write => "可写",
            Authority::Destructive => "破坏性",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "read" => Authority::Read,
            "observe" => Authority::Observe,
            "write" => Authority::Write,
            "destructive" => Authority::Destructive,
            _ => return None,
        })
    }

    /// 按权限升序排列的全部取值，供 UI 做下拉框。
    pub const ALL: [Authority; 4] = [
        Authority::Read,
        Authority::Observe,
        Authority::Write,
        Authority::Destructive,
    ];
}

/// 副作用性质。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    /// 无副作用
    #[default]
    None,
    /// 产生本地副产物（写文件等）
    LocalArtifact,
    /// 改变外部系统状态
    Mutating,
    /// 可能产生不可回滚的变更
    Irreversible,
}

impl SideEffect {
    pub const fn as_str(self) -> &'static str {
        match self {
            SideEffect::None => "none",
            SideEffect::LocalArtifact => "local_artifact",
            SideEffect::Mutating => "mutating",
            SideEffect::Irreversible => "irreversible",
        }
    }

    pub const fn label_zh(self) -> &'static str {
        match self {
            SideEffect::None => "无",
            SideEffect::LocalArtifact => "本地副产物",
            SideEffect::Mutating => "改变外部",
            SideEffect::Irreversible => "不可回滚",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => SideEffect::None,
            "local_artifact" => SideEffect::LocalArtifact,
            "mutating" => SideEffect::Mutating,
            "irreversible" => SideEffect::Irreversible,
            _ => return None,
        })
    }
}

/// 工具姿态 —— 描述智能体调用工具时的行为倾向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolPosture {
    /// 纯计算，不接触外部资源
    #[default]
    Pure,
    /// 只读访问
    Reader,
    /// 需要写入意图确认
    Writer,
    /// 需要人工审批后才能执行
    Gated,
}

impl ToolPosture {
    pub const fn as_str(self) -> &'static str {
        match self {
            ToolPosture::Pure => "pure",
            ToolPosture::Reader => "reader",
            ToolPosture::Writer => "writer",
            ToolPosture::Gated => "gated",
        }
    }

    pub const fn label_zh(self) -> &'static str {
        match self {
            ToolPosture::Pure => "纯计算",
            ToolPosture::Reader => "只读",
            ToolPosture::Writer => "写入",
            ToolPosture::Gated => "需审批",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pure" => ToolPosture::Pure,
            "reader" => ToolPosture::Reader,
            "writer" => ToolPosture::Writer,
            "gated" => ToolPosture::Gated,
            _ => return None,
        })
    }
}

/// 智能体的多维度身份。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CapabilityIdentity {
    /// 角色名，如 `researcher` / `reviewer` / `planner`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    #[serde(default)]
    pub authority: Authority,

    #[serde(default)]
    pub side_effect: SideEffect,

    #[serde(default)]
    pub tool_posture: ToolPosture,

    /// 是否可安全重试
    #[serde(default = "default_true")]
    pub idempotent: bool,

    /// 是否运行在开放世界（需要访问不受控的外部输入）
    #[serde(default)]
    pub open_world: bool,

    /// 扩展维度：`{"cost_tier": "high", "domain": "geo"}`
    ///
    /// 这是**可被检索的** —— 判定层会把它当结构化过滤条件，
    /// 而不是塞进向量里做语义匹配。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dimensions: BTreeMap<String, String>,
}

fn default_true() -> bool {
    true
}

impl CapabilityIdentity {
    /// 判断本身份是否**允许**承载目标身份的能力。
    ///
    /// 规则：权限和副作用必须逐级覆盖，其他维度必须一致或目标未声明。
    /// 用于委派链上的能力收窄校验。
    pub fn admits(&self, required: &CapabilityIdentity) -> bool {
        if self.authority < required.authority {
            return false;
        }
        if self.side_effect < required.side_effect {
            return false;
        }
        // 要求的姿态如果比自身更严格，也不通过
        if required.tool_posture > self.tool_posture {
            return false;
        }
        for (k, v) in &required.dimensions {
            match self.dimensions.get(k) {
                Some(mine) if mine == v => {}
                _ => return false,
            }
        }
        true
    }

    /// 生成用于 BM25 / 精确匹配的扁平关键词串。
    pub fn keyword_blob(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(r) = &self.role {
            parts.push(r.clone());
        }
        parts.push(format!("{:?}", self.authority).to_lowercase());
        parts.push(format!("{:?}", self.side_effect).to_lowercase());
        parts.push(format!("{:?}", self.tool_posture).to_lowercase());
        if self.idempotent {
            parts.push("idempotent".into());
        }
        if self.open_world {
            parts.push("open_world".into());
        }
        for (k, v) in &self.dimensions {
            parts.push(format!("{k}={v}"));
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dims(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
        kv.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn authority_must_cover() {
        let observer = CapabilityIdentity {
            authority: Authority::Observe,
            ..Default::default()
        };
        let destructive = CapabilityIdentity {
            authority: Authority::Destructive,
            ..Default::default()
        };
        assert!(!observer.admits(&destructive));
        assert!(destructive.admits(&observer));
    }

    #[test]
    fn dimension_mismatch_rejected() {
        let cheap = CapabilityIdentity {
            dimensions: dims(&[("cost_tier", "low")]),
            ..Default::default()
        };
        let pricey = CapabilityIdentity {
            dimensions: dims(&[("cost_tier", "high")]),
            ..Default::default()
        };
        assert!(!cheap.admits(&pricey));
        assert!(cheap.admits(&cheap));
    }

    #[test]
    fn keyword_blob_contains_all_dimensions() {
        let id = CapabilityIdentity {
            role: Some("reviewer".into()),
            dimensions: dims(&[("domain", "geo")]),
            ..Default::default()
        };
        let blob = id.keyword_blob();
        assert!(blob.contains("reviewer"));
        assert!(blob.contains("domain=geo"));
    }
}
