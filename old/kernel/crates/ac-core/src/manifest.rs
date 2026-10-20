//! Capability Manifest —— capability-core 的资产注册协议。
//!
//! 六类资产（Skill / MCP / Plugin / Suite / Ghost / Organization）
//! 统一抽象成 `CapabilityManifest`，任何新增资产只要能产出这个结构就能被注册。

use serde::{Deserialize, Serialize};
use std::fmt;

/// CapabilityCore 承载的六类资产。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// SKILL.md 技能包（渐进式披露的知识/流程）
    Skill,
    /// MCP server（可调用的工具集合）
    Mcp,
    /// 插件包（可同时携带 skill / mcp / hook / agent）
    Plugin,
    /// 套件（多个资产的组装形态）
    Suite,
    /// 幽灵智能体（无实体工具、仅存在于委派链中的执行体）
    Ghost,
    /// 组织（智能体层级与编队关系）
    Organization,
}

impl AssetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AssetKind::Skill => "skill",
            AssetKind::Mcp => "mcp",
            AssetKind::Plugin => "plugin",
            AssetKind::Suite => "suite",
            AssetKind::Ghost => "ghost",
            AssetKind::Organization => "organization",
        }
    }
}

impl fmt::Display for AssetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 资产来源溯源信息。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Provenance {
    /// 导入时的绝对路径
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// 导入时间（RFC3339）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_at: Option<String>,
    /// 内容指纹，用于变更检测与增量索引
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// 原始格式（用于回溯与兼容）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    /// M1.5 L2 规范化指纹：去 frontmatter/空白/版本号 + 小写后的哈希。
    ///
    /// 用于识别"重打包但实质相同"的副本。**不参与自动合并**，
    /// 只作为待用户确认的 L2 候选提示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_hash: Option<String>,
    /// 解析阶段收集到的**运行期引用原样值**：MCP 的启动命令与参数、URL、
    /// plugin 声明的组件路径。
    ///
    /// 为什么要有这一列：用户点名的「来源 / 类型」两个维度在库里原本不存在，
    /// 而「这个能力能不能脱离外部目录独立跑」只能从**它引用了什么**判出来。
    /// 早先这些信息只以自然语言写在 `provides[].summary` 里
    /// （"stdio server，启动命令 D:/…"），任何判定都得去解析人话；
    /// 这里把它们原样存下来，判定逻辑（`ac_registry::origin`）只读结构化数据。
    ///
    /// `serde(default)` 是给老库的：旧 manifest JSON 没有这个键，读回来是空表，
    /// 此时 `origin` 模块退化为「从 summary 文本里提取」，两条路径结论一致。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_refs: Vec<String>,
    /// 资产作者**自声明**的名字（v8，2026-10-18）。
    ///
    /// 解析期只做一件事：把作者写的东西**原样取下来**——
    /// plugin.json 的 `author`（字符串或 `{"name": ...}` 两种写法都真实存在）、
    /// SKILL.md frontmatter 的 `author:`。
    ///
    /// 判定「到底算谁」的逻辑不在这里（见 `ac_registry::author`）：
    /// 解析器只负责原样记，谁来判、判成什么，是另一件事。
    /// 两者混在一起就再也无法单独验证判据。
    ///
    /// 缺省 `None` = 作者没写。**绝不写占位串**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// 上面那个 author 是**从哪一类声明里读到的**：
    /// `manifest`（plugin.json）| `frontmatter`（SKILL.md）| `None`（没有声明）。
    ///
    /// 为什么要把「出处」也存进 manifest：同一个自声明在两条解析路径上
    /// 出处不同，界面要按档标注（作者（自声明）/ 作者（frontmatter））。
    /// 只存名字的话，这个区别在入库那一刻就永久丢失了。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_source: Option<String>,
}

/// 一条能力声明。
///
/// 注意：这里描述的是"能力"，不是"工具"。
/// 一个 Skill 提供能力但不提供可调用工具；一个 MCP server 提供一批工具。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvidedCapability {
    /// 全局唯一能力 ID，约定 `{asset_id}::{cap_key}`
    pub capability_id: String,
    /// 短名，用于精确检索
    pub name: String,
    /// 语义描述，**向量检索主要吃这个字段**
    pub summary: String,
    /// 结构化输入 schema（MCP tool 才有）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// 行为标签：`read_only` / `destructive` / `idempotent` / `open_world`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// 统一的资产清单。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapabilityManifest {
    /// 协议版本，见 `ac_core::SCHEMA_VERSION`
    pub schema_version: String,
    /// 全局唯一 ID
    pub id: String,
    /// 资产类型
    pub kind: AssetKind,
    /// 人类可读名
    pub name: String,
    /// 版本号（无版本概念的资产用 "0"）
    #[serde(default)]
    pub version: String,
    /// 一句话描述
    pub description: String,
    /// 多维度身份描述（见 `identity` 模块）
    #[serde(default)]
    pub identity: super::identity::CapabilityIdentity,
    /// 该资产提供的能力
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provides: Vec<ProvidedCapability>,
    /// 与其他资产的关系（见 `relation` 模块）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<super::relation::AssetRelation>,
    #[serde(default)]
    pub provenance: Provenance,
}

#[derive(Debug)]
pub enum ManifestError {
    MissingId,
    MissingName,
    UnknownSchemaVersion(String),
    BadJson(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::MissingId => write!(f, "manifest.id 不能为空"),
            ManifestError::MissingName => write!(f, "manifest.name 不能为空"),
            ManifestError::UnknownSchemaVersion(v) => write!(f, "不认识的 schema_version: {v}"),
            ManifestError::BadJson(e) => write!(f, "JSON 解析失败: {e}"),
        }
    }
}

impl std::error::Error for ManifestError {}

impl CapabilityManifest {
    pub fn new(id: impl Into<String>, kind: AssetKind, name: impl Into<String>) -> Self {
        Self {
            schema_version: super::SCHEMA_VERSION.to_string(),
            id: id.into(),
            kind,
            name: name.into(),
            version: "0".into(),
            description: String::new(),
            identity: Default::default(),
            provides: Vec::new(),
            relations: Vec::new(),
            provenance: Default::default(),
        }
    }

    /// 校验清单合法性。
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.id.trim().is_empty() {
            return Err(ManifestError::MissingId);
        }
        if self.name.trim().is_empty() {
            return Err(ManifestError::MissingName);
        }
        if self.schema_version != super::SCHEMA_VERSION {
            return Err(ManifestError::UnknownSchemaVersion(self.schema_version.clone()));
        }
        Ok(())
    }

    pub fn parse(json: &str) -> Result<Self, ManifestError> {
        let m: Self =
            serde_json::from_str(json).map_err(|e| ManifestError::BadJson(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip() {
        let mut m = CapabilityManifest::new("ac::test", AssetKind::Skill, "测试技能");
        m.description = "验证序列化".into();
        m.provides.push(ProvidedCapability {
            capability_id: "ac::test::run".into(),
            name: "run".into(),
            summary: "执行".into(),
            input_schema: None,
            tags: vec!["read_only".into()],
        });
        let json = serde_json::to_string(&m).unwrap();
        let back = CapabilityManifest::parse(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn rejects_empty_id() {
        let m = CapabilityManifest::new("  ", AssetKind::Ghost, "x");
        assert!(matches!(m.validate(), Err(ManifestError::MissingId)));
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let mut m = CapabilityManifest::new("a", AssetKind::Suite, "n");
        m.schema_version = "9.9".into();
        assert!(matches!(m.validate(), Err(ManifestError::UnknownSchemaVersion(_))));
    }
}
