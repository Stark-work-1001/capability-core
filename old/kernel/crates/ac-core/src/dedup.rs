//! M1.5 去重的数据模型：**逻辑资产 + 位置集**。
//!
//! 设计依据见 `docs/planning/02-规划文档.md` 的「### M1.5 去重」。
//!
//! 三个必须守住的纪律：
//!
//! 1. **一个逻辑资产对应 N 个位置**。内容相同、位置不同的副本不是两份资产，
//!    而是同一份资产的两个存在位置。检索按逻辑资产去重，调用按具体位置。
//! 2. **扫描只增不删**。位置从扫描根消失后**降级为 `alias`** 而不是被删除，
//!    这样才能回答"上次还是这版、现在升级了"这类追溯问题。
//! 3. **多版本各自独立**。`0.1.0` 与 `1.0.0` 是两个逻辑资产，
//!    **不做** "latest" 兜底——挑哪个版本是用户决策，不是程序决策。
//!
//! 本模块是协议层：零 IO、零外部 crate 依赖，只描述形状。

use serde::{Deserialize, Serialize};

/// 一个位置在逻辑资产内的角色。
///
/// 默认是 `Alias`：**未经用户确认，一律不假定自己是主副本。**
/// 只有在去重候选队列里被用户显式确认，才会升为 `Canonical`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationRole {
    /// 主副本：逻辑资产的展示名/描述以它为准。
    Canonical,
    /// 别名：同一逻辑资产的另一个磁盘位置。
    ///
    /// 两种来源：
    /// - 同内容不同位置（真重复），尚未由用户裁决；
    /// - 上次扫描还在、这次没了的历史位置（append-only 留下的痕迹）。
    Alias,
}

impl LocationRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            LocationRole::Canonical => "canonical",
            LocationRole::Alias => "alias",
        }
    }

    /// 解析持久化里的字符串。未知值一律当 `alias`：宁可低估也不要误升主副本。
    pub fn parse(s: &str) -> Self {
        match s {
            "canonical" => LocationRole::Canonical,
            _ => LocationRole::Alias,
        }
    }
}

impl std::fmt::Display for LocationRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 去重候选的裁决结果。
///
/// 三个选项都是**显式的用户动作**，没有默认值，也没有超时自动合并。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DedupDecision {
    /// 合并：丢弃别名，只保留规范位置（canonical）。
    Merge,
    /// 共存：全部升为独立逻辑资产，各算各的。
    KeepBoth,
    /// 忽略：维持别名角色，下次扫描再问一次。
    Ignore,
}

impl DedupDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            DedupDecision::Merge => "merge",
            DedupDecision::KeepBoth => "keep_both",
            DedupDecision::Ignore => "ignore",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "merge" => Some(DedupDecision::Merge),
            "keep_both" => Some(DedupDecision::KeepBoth),
            "ignore" => Some(DedupDecision::Ignore),
            _ => None,
        }
    }
}

/// 去重候选：一条待用户确认的分组请求。
///
/// **绝不自动执行。** 静默合并会把"昨天改的本地定制版"和"今天拉的上游版"
/// 混成一份，下一个智能体调到的就不是用户期望的版本了。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DedupCandidate {
    /// 本组共享的指纹（构建组的键，见 `fingerprint::group_key`）
    pub group_key: String,
    /// 哪一级指纹发现的：L1 字节相同 / L2 规范化后相同
    pub match_level: MatchLevel,
    /// 建议的规范位置 id（该组内 source_path 最短的那条）
    pub suggested_canonical: String,
    /// 组内全部位置 id，按路径长度升序（第一条即建议的规范位置）
    pub location_ids: Vec<String>,
}

/// 指纹命中级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchLevel {
    /// L1：全文 SHA-256 相同（忽略 frontmatter 头）——字节级重复。
    L1,
    /// L2：规范化后相同（去 frontmatter / 去空白 / 去版本号 / 小写）——重打包。
    L2,
}

impl MatchLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchLevel::L1 => "l1",
            MatchLevel::L2 => "l2",
        }
    }
}

/// 逻辑资产（一份内容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicalAsset {
    /// 主键。由指纹决定，见 `fingerprint::group_key`。
    pub logical_id: String,
    /// L1 内容指纹。**同内容即同逻辑资产。**
    pub content_hash: String,
    /// L2 规范化指纹。用于识别"重打包但实质相同"的那一批。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_hash: Option<String>,
    /// 资产类型
    pub kind: String,
    /// 展示名——取自规范位置那份
    pub name: String,
    /// 版本——取自规范位置那份
    pub version: String,
    /// 描述——取自规范位置那份
    pub description: String,
    /// 规范位置（`AssetLocation.location_id`）
    pub canonical_location_id: String,
    /// 组内位置数（含 canonical）
    pub location_count: u32,
}

/// 逻辑资产的一个磁盘位置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetLocation {
    pub location_id: String,
    /// 归属的逻辑资产
    pub logical_id: String,
    /// 真实磁盘路径
    pub source_path: String,
    /// 角色
    pub role: LocationRole,
    /// 最近一次扫描看到它的时间（Unix 秒）
    pub last_seen_at: i64,
    /// 关联的旧 `assets.id`（M1 的行），便于回滚
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_role_is_alias_not_canonical() {
        // 未知/损坏的 role 绝不能被当成 canonical：
        // 一旦误升主副本，展示名与描述就会指向用户没选过的位置。
        assert_eq!(LocationRole::parse("garbage"), LocationRole::Alias);
        assert_eq!(LocationRole::parse(""), LocationRole::Alias);
        assert_eq!(LocationRole::parse("ALIAS"), LocationRole::Alias);
        assert_eq!(LocationRole::parse("canonical"), LocationRole::Canonical);
    }

    #[test]
    fn decision_roundtrips() {
        for d in [DedupDecision::Merge, DedupDecision::KeepBoth, DedupDecision::Ignore] {
            assert_eq!(DedupDecision::parse(d.as_str()), Some(d));
        }
        assert_eq!(DedupDecision::parse("nope"), None);
    }
}
