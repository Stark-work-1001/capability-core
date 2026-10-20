//! 能力故障定位（Locator）。
//!
//! # 职责边界：只定位，不维修
//!
//! 本模块回答的是「哪里坏了、坏成什么样」，**不回答**「该怎么改」。
//! 维修动作由接入中枢的外部智能体执行——那是它的判断，不是中枢的。
//! 所以 FailureClass 是**检测结论**（实测得到的事实分类），
//! SuggestedRepairKind 是**建议**（一个 suggested_ 前缀的提示，
//! 智能体可以完全不采纳）。
//!
//! # evidence 必须来自实测
//!
//! 每一条 Evidence 都携带 EvidenceKind，取值只有三类：
//! Probed（本次真的碰了文件系统 / PATH / 进程环境）、Recorded（读的是库内已有记录）、
//! Inferred（从上两者推出来的一步逻辑）。
//!
//! **没有 Assumed。** 想写一条「大概是吧」的证据，就得先给 EvidenceKind
//! 加一个变体——那是一次需要评审的改动，而不是顺手补一句字符串。
//!
//! # 绝不修改 vendor/ 与用户原始文件
//!
//! 本模块对外的写入面**只有** assets/ 目录与中枢自己的数据库。
//! 探测是纯读 ProbeFs / ProbePath / ProbeEnv 三个 trait 上的方法，
//! 连一次 OpenOptions::write 都没有。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── 故障分类 ────────────────────────────────────────────────────────

/// 一个能力为什么起不来。**这是检测结论，不是猜测。**
///
/// 变体之间的**声明顺序就是优先序**（derive 出来的 Ord 按它排）。
/// 优先序的判据是「谁更靠近根因」：同一份配置常常同时命中好几条，
/// 例如 obsidian-suite 的命令指向一个整体消失的根目录、参数里还留着
/// 未展开的变量。两条都是真的，但「一整个根目录没了」比
/// 「命令名找不到」更接近根因，所以排在前面。
///
/// 没被选为主分类的那些不会丢，见 FaultReport::also_detected。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// 配置本身读不懂（JSON 解析失败 / 字段类型不对）。
    ConfigUnreadable,
    /// 命令字段为空、也没有 url —— 没说怎么启动。
    LaunchNotSpecified,
    /// 引用的**整个根目录已经不存在**（项目改名 / 搬迁前的旧根）。
    ///
    /// 判据是一次对 <盘符>:<第一段> 的存在性实测。这是本模块里
    /// 解释力最强的一类：它同时解释了 command、args、cwd 为什么全都失效。
    DeadRoot,
    /// 启动命令是一个**绝对路径，而该路径不存在**（根目录还在）。
    LaunchPathMissing,
    /// 命令是一个绝对路径，存在但不是文件（例如指向一个目录）。
    NoExecutableEntry,
    /// command / args 里残留着**未展开的变量**，且该变量在当前环境里未设置。
    UnexpandedVar,
    /// 命令是**裸命令名，且在 PATH 里找不到**。
    CommandNotInPath,
    /// 上面一条都没命中：入口实测存在且可解析。
    Healthy,
}

impl FailureClass {
    /// 稳定字符串。**写进库、走 HTTP、过 MCP 的都是它**，
    /// 因此改动等于破坏对外契约。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConfigUnreadable => "config_unreadable",
            Self::LaunchNotSpecified => "launch_not_specified",
            Self::DeadRoot => "dead_root",
            Self::LaunchPathMissing => "launch_path_missing",
            Self::NoExecutableEntry => "no_executable_entry",
            Self::UnexpandedVar => "unexpanded_var",
            Self::CommandNotInPath => "command_not_in_path",
            Self::Healthy => "healthy",
        }
    }

    /// 是否属于「这个资产确实起不来」。
    pub fn is_failure(self) -> bool {
        self != Self::Healthy
    }

    /// 建议的维修大类。**只是建议**：中枢不执行它，也不保证它对。
    pub fn suggested_repair_kind(self) -> SuggestedRepairKind {
        match self {
            Self::DeadRoot | Self::LaunchPathMissing => SuggestedRepairKind::PathRemap,
            Self::UnexpandedVar => SuggestedRepairKind::EnvExpansion,
            Self::CommandNotInPath | Self::NoExecutableEntry => {
                SuggestedRepairKind::InstallOrProvision
            }
            Self::ConfigUnreadable | Self::LaunchNotSpecified => SuggestedRepairKind::ConfigRewrite,
            Self::Healthy => SuggestedRepairKind::None,
        }
    }
}

/// 建议的维修大类（**建议，不是执行指令**）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedRepairKind {
    /// 把指向旧位置的路径改指到新位置。
    PathRemap,
    /// 把没展开的变量补上真实值。
    EnvExpansion,
    /// 装 / 补上缺失的可执行件。
    InstallOrProvision,
    /// 重写配置。
    ConfigRewrite,
    /// 得人来判断。
    Manual,
    /// 无需维修。
    None,
}

impl SuggestedRepairKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PathRemap => "path_remap",
            Self::EnvExpansion => "env_expansion",
            Self::InstallOrProvision => "install_or_provision",
            Self::ConfigRewrite => "config_rewrite",
            Self::Manual => "manual",
            Self::None => "none",
        }
    }
}

// ── 证据 ────────────────────────────────────────────────────────────

/// 这条证据是怎么来的。**没有 Assumed**，见模块文档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// 本次真的碰了文件系统 / PATH / 进程环境。
    Probed,
    /// 读的是中枢库里已有的记录（资产的 source_path 等）。
    Recorded,
    /// 从上两类推出来的一步逻辑结论。
    Inferred,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probed => "probed",
            Self::Recorded => "recorded",
            Self::Inferred => "inferred",
        }
    }
}

/// 一条实测证据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    /// 人读的一句话。**必须能被另一台机器按同一条判据复现。**
    pub detail: String,
    /// 若这条证据是一个路径判定，这里是那个路径的原文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// 若这条证据是一次存在性判定，这里是从**本次实测**里拿到的布尔值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
}

impl Evidence {
    pub fn probed(detail: impl Into<String>) -> Self {
        Self { kind: EvidenceKind::Probed, detail: detail.into(), subject: None, exists: None }
    }

    pub fn probed_exists(
        detail: impl Into<String>,
        subject: impl Into<String>,
        exists: bool,
    ) -> Self {
        Self {
            kind: EvidenceKind::Probed,
            detail: detail.into(),
            subject: Some(subject.into()),
            exists: Some(exists),
        }
    }

    pub fn recorded(detail: impl Into<String>) -> Self {
        Self { kind: EvidenceKind::Recorded, detail: detail.into(), subject: None, exists: None }
    }

    pub fn inferred(detail: impl Into<String>) -> Self {
        Self { kind: EvidenceKind::Inferred, detail: detail.into(), subject: None, exists: None }
    }
}

// ── 定位 ────────────────────────────────────────────────────────────

/// 故障到底在哪一行哪个字段上。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// 配置里的字段路径，例如 mcpServers.obsidian-suite.command。
    pub field: String,
    /// 该字段的**原文值**。不改写、不归一化：看到的就是配置文件里的东西。
    pub value: String,
    /// 该字段所在的源文件（相对工作区，可读）。
    pub source_file: String,
}

/// 一份定位报告。**中枢的最终产物就是这个结构**，维修方按它接手。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultReport {
    pub asset_id: String,
    /// 资产类型：mcp / plugin / skill。
    pub kind: String,
    pub asset_name: String,
    /// server 名（一份配置里可能有多个 server，一个 server 一条报告）。
    pub server: Option<String>,
    /// 主分类：本次检测里**最靠近根因**的那一条（见 FailureClass 的优先序）。
    pub failure_class: FailureClass,
    /// 同一处还命中的其它分类。**它们没有被丢掉，只是不是主因。**
    /// 一条故障往往同时命中三四条，只报一条会让修的人少看三条事实。
    #[serde(default)]
    pub also_detected: Vec<FailureClass>,
    /// 与主分类对应的定位。
    pub location: Location,
    /// 实测证据链。**至少一条，且至少一条是 Probed。**
    pub evidence: Vec<Evidence>,
    /// 修复这件事之前必须先知道的东西。让接手的智能体不用再猜一遍。
    pub prerequisites: Vec<String>,
    /// **建议**的维修大类，不是执行指令。
    pub suggested_repair_kind: SuggestedRepairKind,
    /// 检测时间（unix 秒）。
    pub detected_at: i64,
}

impl FaultReport {
    pub fn is_failure(&self) -> bool {
        self.failure_class.is_failure()
    }

    /// 对外 / 入库用的稳定 id：asset_id + "#" + server，或无 server 时就是 asset_id。
    pub fn fault_key(&self) -> String {
        match &self.server {
            Some(s) => format!("{}#{}", self.asset_id, s),
            None => self.asset_id.clone(),
        }
    }

    /// 是否满足「evidence 必须来自实测」的硬要求。
    pub fn has_probed_evidence(&self) -> bool {
        self.evidence.iter().any(|e| e.kind == EvidenceKind::Probed)
    }

    /// 本条报告命中的全部分类（主分类 + 其它），已去重并按优先序排序。
    pub fn classes(&self) -> Vec<FailureClass> {
        let mut v = vec![self.failure_class];
        for c in &self.also_detected {
            if !v.contains(c) {
                v.push(*c);
            }
        }
        v.sort();
        v
    }
}

/// 一次扫描的汇总。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocateReport {
    /// 全部被检测的 server 条目数（含健康的）。
    pub total_units: u32,
    /// 健康数。
    pub healthy: u32,
    /// 故障数。
    pub failed: u32,
    /// 按主分类分组计数，键是 FailureClass::as_str。
    pub by_class: std::collections::BTreeMap<String, u32>,
    /// 按**命中过的全部分类**分组计数（含 also_detected）。
    /// 这张表才是「各类症状各有多少处」的答案。
    pub by_any_class: std::collections::BTreeMap<String, u32>,
    /// 全部报告（含 healthy），便于一次性核对「到底检测了什么」。
    pub reports: Vec<FaultReport>,
}

impl LocateReport {
    pub fn from_reports(reports: Vec<FaultReport>) -> Self {
        let mut by_class = std::collections::BTreeMap::new();
        let mut by_any_class: std::collections::BTreeMap<String, u32> =
            std::collections::BTreeMap::new();
        let mut healthy = 0u32;
        let mut failed = 0u32;
        for r in &reports {
            *by_class.entry(r.failure_class.as_str().to_string()).or_insert(0) += 1;
            for c in r.classes() {
                *by_any_class.entry(c.as_str().to_string()).or_insert(0) += 1;
            }
            if r.is_failure() {
                failed += 1;
            } else {
                healthy += 1;
            }
        }
        Self {
            total_units: reports.len() as u32,
            healthy,
            failed,
            by_class,
            by_any_class,
            reports,
        }
    }

    /// 只取故障条目。
    pub fn failures(&self) -> Vec<&FaultReport> {
        self.reports.iter().filter(|r| r.is_failure()).collect()
    }

    /// 按资产聚合：每个资产的主分类 = 它名下**最靠近根因**的那一条。
    ///
    /// 「11 个 MCP 资产里有几个起不来」问的是资产，不是 server 条目，
    /// 所以必须单独给一张资产级的表，不然计数会重复。
    pub fn asset_rollup(&self) -> Vec<AssetVerdict> {
        let mut map: std::collections::BTreeMap<String, AssetVerdict> =
            std::collections::BTreeMap::new();
        for r in &self.reports {
            let e = map.entry(r.asset_id.clone()).or_insert_with(|| AssetVerdict {
                asset_id: r.asset_id.clone(),
                kind: r.kind.clone(),
                asset_name: r.asset_name.clone(),
                source_file: r.location.source_file.clone(),
                units: 0,
                failed_units: 0,
                failure_class: FailureClass::Healthy,
                classes: Vec::new(),
            });
            e.units += 1;
            for c in r.classes() {
                if c.is_failure() && !e.classes.contains(&c) {
                    e.classes.push(c);
                }
            }
            if r.is_failure() {
                e.failed_units += 1;
            }
        }
        let mut out: Vec<AssetVerdict> = map.into_values().collect();
        for e in &mut out {
            e.classes.sort();
            e.failure_class = e.classes.first().copied().unwrap_or(FailureClass::Healthy);
        }
        out
    }
}

/// 资产级裁决（一个资产可能有多条 server 报告）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetVerdict {
    pub asset_id: String,
    pub kind: String,
    pub asset_name: String,
    pub source_file: String,
    /// 该资产下的 server 条目数。
    pub units: u32,
    /// 其中起不来的条目数。
    pub failed_units: u32,
    /// 资产级主分类：名下所有故障里最靠近根因的那一条。
    pub failure_class: FailureClass,
    /// 名下出现过的全部分类（已排序去重）。
    pub classes: Vec<FailureClass>,
}

impl AssetVerdict {
    pub fn is_failure(&self) -> bool {
        self.failure_class.is_failure()
    }
}

// ── 探测抽象（可注入，因此可单测）─────────────────────────────────

/// 文件系统探测。**只读**：本 trait 没有写方法，所以任何实现都不可能
/// 被本模块拿去改文件。
pub trait ProbeFs {
    fn exists(&self, p: &Path) -> bool;
    fn is_file(&self, p: &Path) -> bool;
    fn is_dir(&self, p: &Path) -> bool;
}

/// PATH 探测。
pub trait ProbePath {
    /// 命令是否能在 PATH 里解析到。返回解析到的真实路径。
    fn which(&self, cmd: &str) -> Option<PathBuf>;
    /// 本次查过多少个 PATH 目录。写进证据里，让「找不到」这件事可核对。
    fn dir_count(&self) -> usize;
}

/// 环境变量探测。
pub trait ProbeEnv {
    /// 变量值；未设置为 None。
    fn var(&self, name: &str) -> Option<String>;
}

/// 真实文件系统。
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFs;

impl ProbeFs for RealFs {
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }
    fn is_file(&self, p: &Path) -> bool {
        p.is_file()
    }
    fn is_dir(&self, p: &Path) -> bool {
        p.is_dir()
    }
}

/// 真实 PATH。
///
/// 只认 PATHEXT 里那组后缀，与 Windows 的解析规则一致；
/// 非 Windows 上 PATHEXT 读不到就退回不带后缀直判。
#[derive(Debug, Clone, Default)]
pub struct RealPath {
    /// 预先算好的 PATH 目录列表。空表示现读环境变量。
    pub dirs: Vec<PathBuf>,
    /// 可执行后缀（Windows 上是 PATHEXT）。
    pub exts: Vec<String>,
}

impl RealPath {
    pub fn from_env() -> Self {
        let dirs = std::env::var("PATH")
            .unwrap_or_default()
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        let exts: Vec<String> = std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        Self { dirs, exts }
    }

    fn resolve_dirs(&self) -> Vec<PathBuf> {
        if self.dirs.is_empty() {
            RealPath::from_env().dirs
        } else {
            self.dirs.clone()
        }
    }

    fn resolve_exts(&self) -> Vec<String> {
        if self.exts.is_empty() {
            RealPath::from_env().exts
        } else {
            self.exts.clone()
        }
    }
}

impl ProbePath for RealPath {
    fn which(&self, cmd: &str) -> Option<PathBuf> {
        if cmd.trim().is_empty() {
            return None;
        }
        let as_path = Path::new(cmd);
        // 带目录分隔符的一律按路径处理，不查 PATH——这与 shell 语义一致。
        if as_path.components().count() > 1 {
            return if as_path.is_file() {
                Some(as_path.to_path_buf())
            } else {
                None
            };
        }
        for d in &self.resolve_dirs() {
            // 先试原样（无后缀的可执行，或调用方已经写了后缀）
            let direct = d.join(cmd);
            if direct.is_file() {
                return Some(direct);
            }
            for ext in &self.resolve_exts() {
                let candidate = d.join(format!("{}{}", cmd, ext));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    fn dir_count(&self) -> usize {
        self.resolve_dirs().len()
    }
}

/// 真实环境变量。
#[derive(Debug, Clone, Copy, Default)]
pub struct RealEnv;

impl ProbeEnv for RealEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// 一组默认探针。
pub fn real_probes() -> (RealFs, RealPath, RealEnv) {
    (RealFs, RealPath::from_env(), RealEnv)
}

// ── 变量引用的识别 ──────────────────────────────────────────────────

/// 一个未展开的变量引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarRef {
    /// 原文，例如 ${QODER_PLUGIN_ROOT}。
    pub raw: String,
    /// 变量名，例如 QODER_PLUGIN_ROOT。
    pub name: String,
}

impl VarRef {
    /// 这个变量名是不是「项目根」语义的宿主变量。
    ///
    /// 判据是**后缀**而不是白名单：QODER_PLUGIN_ROOT / CLAUDE_PLUGIN_ROOT /
    /// CODEX_HOME 这类名字在后来的宿主里还会继续出现，
    /// 每来一个新宿主就往白名单里加一行，是把维护成本推给未来。
    pub fn is_root_style(&self) -> bool {
        let n = self.name.to_ascii_uppercase();
        n.ends_with("_ROOT") || n.contains("PLUGIN_ROOT") || n.ends_with("_HOME")
    }
}

/// 找出字符串里全部未展开的变量引用。
///
/// 认三种形态：${VAR}、$VAR、%VAR%（cmd）。
/// **不认**单独的一个美元符。
pub fn find_unexpanded_vars(s: &str) -> Vec<VarRef> {
    let bytes = s.as_bytes();
    let mut out: Vec<VarRef> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = s[i + 2..].find('}') {
                let name = &s[i + 2..i + 2 + end];
                if is_var_name(name) {
                    out.push(VarRef { raw: s[i..i + 2 + end + 1].to_string(), name: name.to_string() });
                }
                i = i + 2 + end + 1;
                continue;
            }
            i += 1;
            continue;
        }
        if b == b'$' && i + 1 < bytes.len() && is_var_head(bytes[i + 1] as char) {
            let mut j = i + 1;
            while j < bytes.len() && is_var_body(bytes[j] as char) {
                j += 1;
            }
            let name = &s[i + 1..j];
            out.push(VarRef { raw: s[i..j].to_string(), name: name.to_string() });
            i = j;
            continue;
        }
        if b == b'%' {
            if let Some(end) = s[i + 1..].find('%') {
                let name = &s[i + 1..i + 1 + end];
                if is_var_name(name) {
                    out.push(VarRef { raw: s[i..i + 1 + end + 1].to_string(), name: name.to_string() });
                }
                i = i + 1 + end + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_var_head(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_var_body(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_var_name(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        Some(c) if is_var_head(c) => it.all(is_var_body),
        _ => false,
    }
}
// ── 配置解析 ────────────────────────────────────────────────────────

/// 一个 MCP server 条目的原始配置（**只读快照**，不改写）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    /// server 名（mcpServers 的键）。
    pub name: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub url: Option<String>,
    /// 该配置里出现的全部环境变量键（值不读取，避免把密钥带进报告）。
    pub env_keys: Vec<String>,
}

/// 从 .mcp.json / mcp.json 的原文解析出 server 表。
///
/// 形态与 ingest 侧一致：兼容 mcpServers 包一层、与顶层直接是 server 表两种。
/// 解析失败返回 Err(原因)，调用方据此报 config_unreadable。
pub fn parse_mcp_servers(text: &str) -> Result<Vec<McpServerConfig>, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("JSON 解析失败: {e}"))?;
    let obj = match v.get("mcpServers").and_then(|x| x.as_object()) {
        Some(m) => m.clone(),
        None => match v.as_object() {
            Some(m) => m.clone(),
            None => return Err("顶层既不是对象也没有 mcpServers".to_string()),
        },
    };
    let mut out = Vec::new();
    for (name, cfg) in obj {
        // 以 "_" 开头的是注释键（真实资产里就长这样），不是 server。
        if name.starts_with('_') {
            continue;
        }
        if !cfg.is_object() {
            return Err(format!("server {name} 的配置不是对象"));
        }
        let args = cfg
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let env_keys = cfg
            .get("env")
            .and_then(|e| e.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        out.push(McpServerConfig {
            name,
            command: cfg.get("command").and_then(|x| x.as_str()).map(String::from),
            args,
            cwd: cfg.get("cwd").and_then(|x| x.as_str()).map(String::from),
            url: cfg.get("url").and_then(|x| x.as_str()).map(String::from),
            env_keys,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

// ── 路径工具 ────────────────────────────────────────────────────────

/// 平台目录分隔符。
///
/// 抽成常量而不是到处写字符字面量：Windows 上是反斜杠，
/// 而反斜杠在字符串字面量里要转义，写多了必然有人少写一个。
pub const SEP: char = std::path::MAIN_SEPARATOR;

/// 把配置里的路径原文归一成当前平台可 Test 的形式。
///
/// 只做一件事：把正斜杠换成平台分隔符。
/// **不做**任何「猜测真实位置」的替换——那会把证据变成推测，
/// 而本模块的全部价值就在于证据是实测的。
pub fn normalize_path(raw: &str) -> String {
    let s = raw.trim();
    if cfg!(windows) {
        s.replace('/', &SEP.to_string())
    } else {
        s.to_string()
    }
}

/// 一个路径是否**看起来是绝对路径**（Windows 盘符或 POSIX 根）。
pub fn is_absolute_like(raw: &str) -> bool {
    let s = raw.trim();
    let chars: Vec<char> = s.chars().take(2).collect();
    if chars.len() == 2 && chars[1] == ':' && chars[0].is_ascii_alphabetic() {
        return true;
    }
    s.starts_with(SEP)
}

/// 取路径的「根段」：盘符 + 第一段目录。
///
/// 这是 dead_root 判据的核心：**整个根段不存在**，说明这一整片
/// 都搬走了，而不是某个具体文件丢了。两者的维修方式完全不同。
pub fn root_segment(raw: &str) -> Option<String> {
    let n = normalize_path(raw);
    let chars: Vec<char> = n.chars().collect();
    if chars.len() >= 3 && chars[0].is_ascii_alphabetic() && chars[1] == ':' {
        let rest: String = chars.iter().skip(3).collect();
        let seg = rest.split(SEP).find(|s| !s.is_empty())?;
        // 必须显式带上分隔符：PathBuf::from("D:") 是**盘符相对路径**，
        // 往上 push 得到的是 "D:foo" 而不是 "D:oo"，前者不对应任何真实目录。
        return Some(format!("{}{}{}", chars[0], ':', SEP.to_string() + seg));
    }
    if n.starts_with(SEP) {
        let seg = n
            .trim_start_matches(SEP)
            .split(SEP)
            .find(|s| !s.is_empty())?;
        let mut p = PathBuf::from(SEP.to_string());
        p.push(seg);
        return Some(p.to_string_lossy().to_string());
    }
    None
}

/// 一个路径的祖先目录链，从近到远。用于给出「到底哪一层开始不见的」。
pub fn ancestors_nearest_first(raw: &str) -> Vec<String> {
    let n = normalize_path(raw);
    let mut out = Vec::new();
    let mut cur = PathBuf::from(&n);
    // 最多爬 8 层：足够定位到死根，又不会在畸长路径上刷屏。
    for _ in 0..8 {
        let Some(p) = cur.parent() else { break };
        if p.as_os_str().is_empty() {
            break;
        }
        let s = p.to_string_lossy().to_string();
        let next = p.to_path_buf();
        out.push(s);
        cur = next;
        // 只剩盘符根（形如 X:）时停下，再往上就是整个卷了。
        if cur.as_os_str().to_string_lossy().ends_with(':') {
            break;
        }
    }
    out
}
// ── 根目录搬迁表（**配置驱动，不是猜**）──────────────────────────

/// 一条「旧根 → 新根」的对应关系。
///
/// # 为什么这张表必须是外部喂进来的
///
/// 中枢可以判定「D:\.my-agent 这个根不存在了」，那是**实测**。
/// 但它不能自己推断「所以它应该是 C:\external-assets」——那是猜测，
/// 而且猜错的代价很高：接手的智能体会照着一条错路径去改配置。
///
/// 所以这张表由调用方（人，或掌握搬迁事实的上层）提供。
/// 中枢拿到表之后做的事**仍然是实测**：旧根不在 + 新根在 +
/// 旧路径的尾段在新根下真的存在 → 这三条同时成立，
/// 「可以改指到这里」才是一条可写进证据的事实，而不是建议。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRemap {
    /// 已经消失的旧根，形如 D:\.my-agent。
    pub old_root: String,
    /// 位于其上的新根，形如 C:\external-assets。
    pub new_root: String,
}

impl RootRemap {
    pub fn new(old_root: impl Into<String>, new_root: impl Into<String>) -> Self {
        Self {
            old_root: normalize_path(&old_root.into()),
            new_root: normalize_path(&new_root.into()),
        }
    }
}

/// 把一条旧路径按搬家表换算成新路径（**纯字符串换算，不判存在**）。
///
/// 大小写不敏感：Windows 路径本身如此，而配置里盘符大小写混用很常见。
pub fn remap_path(raw: &str, remaps: &[RootRemap]) -> Option<(String, String)> {
    let p = normalize_path(raw);
    let lower = p.to_ascii_lowercase();
    for r in remaps {
        let old = r.old_root.to_ascii_lowercase();
        let with_sep = format!("{}{}", old, SEP);
        if lower == old || lower.starts_with(&with_sep) {
            let tail = p[r.old_root.len()..].trim_start_matches(SEP);
            let mut out = r.new_root.clone();
            if !tail.is_empty() {
                out.push(SEP);
                out.push_str(tail);
            }
            return Some((r.old_root.clone(), out));
        }
    }
    None
}

// ── 检测 ────────────────────────────────────────────────────────────

/// 一个待检测的资产（**只读视图**，调用方从库里或磁盘喂进来）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetProbe {
    pub asset_id: String,
    /// mcp / plugin / skill
    pub kind: String,
    pub asset_name: String,
    /// 源文件路径（报告里原样回填，便于人手打开）。
    pub source_file: String,
    /// 该文件的原文。读不到就是 None。
    pub text: Option<String>,
}

/// 一次检测的上下文。三个探针都是 trait 对象式的泛型参数，
/// 因此单测可以塞进完全虚拟的文件系统与 PATH，不必真的碰磁盘。
pub struct Locator<'a, F: ProbeFs, P: ProbePath, E: ProbeEnv> {
    pub fs: &'a F,
    pub path: &'a P,
    pub env: &'a E,
    /// 本次检测的时间戳，写进每一条报告（同一批报告时间一致，便于对齐）。
    pub now: i64,
    /// 「旧根 → 新根」对应表。**由调用方提供**，中枢不自己推断（见 RootRemap）。
    pub remaps: Vec<RootRemap>,
}

impl<'a, F: ProbeFs, P: ProbePath, E: ProbeEnv> Locator<'a, F, P, E> {
    pub fn new(fs: &'a F, path: &'a P, env: &'a E) -> Self {
        Self { fs, path, env, now: now_unix(), remaps: Vec::new() }
    }

    /// 挂上根目录搬家表。表里每一条的**新根存在性仍由本模块实测**。
    pub fn with_remaps(mut self, remaps: Vec<RootRemap>) -> Self {
        self.remaps = remaps;
        self
    }

    /// 检测一个资产，返回它名下**每个 server 一条**报告。
    ///
    /// 为什么按 server 而不是按资产出报告：一份 .mcp.json 里可以塞两个
    /// server（obsidian-suite 就是），两个 server 的坏法可以完全不同。
    /// 按资产出一份合起来的报告，维修方还得自己拆。
    pub fn locate(&self, a: &AssetProbe) -> Vec<FaultReport> {
        let Some(text) = a.text.as_deref() else {
            return vec![self.plain_report(
                a,
                FailureClass::ConfigUnreadable,
                Location {
                    field: "(file)".into(),
                    value: a.source_file.clone(),
                    source_file: a.source_file.clone(),
                },
                vec![Evidence::probed_exists(
                    format!("源文件 {} 实测不存在或不可读", a.source_file),
                    a.source_file.clone(),
                    false,
                )],
                vec!["需要该资产的原始配置文本；当前连文件都读不到".into()],
            )];
        };
        let servers = match parse_mcp_servers(text) {
            Ok(s) if !s.is_empty() => s,
            Ok(_) => {
                return vec![self.plain_report(
                    a,
                    FailureClass::LaunchNotSpecified,
                    Location {
                        field: "mcpServers".into(),
                        value: "(空)".into(),
                        source_file: a.source_file.clone(),
                    },
                    vec![Evidence::probed("配置可解析，但 mcpServers 里一个条目都没有")],
                    vec!["需要确认这份配置是不是漏了 server 定义".into()],
                )];
            }
            Err(e) => {
                return vec![self.plain_report(
                    a,
                    FailureClass::ConfigUnreadable,
                    Location {
                        field: "(json)".into(),
                        value: e.clone(),
                        source_file: a.source_file.clone(),
                    },
                    vec![Evidence::probed(format!("配置解析失败：{e}"))],
                    vec!["需要一份语法正确的 JSON".into()],
                )];
            }
        };
        servers.iter().map(|s| self.locate_server(a, s)).collect()
    }

    /// 检测单个 server 条目。
    pub fn locate_server(&self, a: &AssetProbe, s: &McpServerConfig) -> FaultReport {
        // ① 没有 command：要么是远程 server，要么压根没说怎么启动。
        let cmd = match s.command.as_deref() {
            Some(c) if !c.trim().is_empty() => c,
            _ => {
                if let Some(url) = s.url.as_deref() {
                    // HTTP 型 server 不需要本机可执行文件，算健康。
                    return self.healthy(
                        a,
                        s,
                        Location {
                            field: format!("mcpServers.{}.url", s.name),
                            value: url.to_string(),
                            source_file: a.source_file.clone(),
                        },
                        vec![Evidence::recorded(format!(
                            "该条目是远程 HTTP server（url={url}），不需要本机可执行入口"
                        ))],
                    );
                }
                return self.plain_report(
                    a,
                    FailureClass::LaunchNotSpecified,
                    Location {
                        field: format!("mcpServers.{}.command", s.name),
                        value: "(缺失)".into(),
                        source_file: a.source_file.clone(),
                    },
                    vec![Evidence::probed("配置里既没有 command 也没有 url，无法确定启动方式")],
                    vec!["需要知道该 server 应该怎么启动".into()],
                );
            }
        };

        let mut hits: Vec<Detected> = Vec::new();

        // ② 命令是绝对路径时：死根 / 不存在 / 不是文件
        if is_absolute_like(cmd) {
            let field = format!("mcpServers.{}.command", s.name);
            let dead = self.dead_root_hit(a, &field, cmd);
            let path_hit = if dead.is_some() {
                None
            } else {
                self.command_path_hit(a, &field, cmd, &s.name)
            };
            if let Some(h) = dead {
                hits.push(h);
            }
            if let Some(h) = path_hit {
                hits.push(h);
            }
        }

        // ③ 参数里的死根（只对「像绝对路径」的参数做）
        for (i, arg) in s.args.iter().enumerate() {
            if is_absolute_like(arg) {
                let field = format!("mcpServers.{}.args[{}]", s.name, i);
                if let Some(h) = self.dead_root_hit(a, &field, arg) {
                    hits.push(h);
                }
            }
        }

        // ④ cwd 里的死根
        if let Some(cwd) = s.cwd.as_deref() {
            if is_absolute_like(cwd) {
                let field = format!("mcpServers.{}.cwd", s.name);
                if let Some(h) = self.dead_root_hit(a, &field, cwd) {
                    hits.push(h);
                }
            }
        }

        // ⑤ 未展开变量：命令与参数都要查
        let cmd_field = format!("mcpServers.{}.command", s.name);
        self.unexpanded_into(a, &cmd_field, cmd, &mut hits);
        for (i, arg) in s.args.iter().enumerate() {
            let field = format!("mcpServers.{}.args[{}]", s.name, i);
            self.unexpanded_into(a, &field, arg, &mut hits);
        }

        // ⑥ 裸命令名不在 PATH（只在命令本身没有变量、也不是路径时才查）
        if !is_absolute_like(cmd) && find_unexpanded_vars(cmd).is_empty() {
            if let Some(h) = self.command_not_in_path_hit(a, &cmd_field, cmd) {
                hits.push(h);
            }
        }

        if hits.is_empty() {
            let mut ev = Vec::new();
            if is_absolute_like(cmd) {
                let p = normalize_path(cmd);
                ev.push(Evidence::probed_exists(
                    format!("启动命令指向的文件 {} 实测存在", p),
                    p,
                    true,
                ));
            } else if let Some(found) = self.path.which(cmd) {
                ev.push(Evidence::probed_exists(
                    format!(
                        "裸命令 {} 在 PATH 的 {} 个目录里解析到 {}",
                        cmd,
                        self.path.dir_count(),
                        found.display()
                    ),
                    found.to_string_lossy().to_string(),
                    true,
                ));
            } else {
                ev.push(Evidence::recorded(format!("命令 {cmd} 未命中任何已知故障判据")));
            }
            let location = Location {
                field: cmd_field,
                value: cmd.to_string(),
                source_file: a.source_file.clone(),
            };
            return self.healthy(a, s, location, ev);
        }

        // 按优先序取主分类；其余保留进 also_detected。
        hits.sort_by_key(|h| h.class);
        let primary = hits[0].class;
        let mut also: Vec<FailureClass> = Vec::new();
        for h in hits.iter().skip(1) {
            if h.class != primary && !also.contains(&h.class) {
                also.push(h.class);
            }
        }
        let mut evidence: Vec<Evidence> = Vec::new();
        for h in &hits {
            for e in &h.evidence {
                if !evidence.contains(e) {
                    evidence.push(e.clone());
                }
            }
        }
        let mut prerequisites: Vec<String> = Vec::new();
        for h in &hits {
            for p in &h.prerequisites {
                if !prerequisites.contains(p) {
                    prerequisites.push(p.clone());
                }
            }
        }
        // 无论哪一类，都要给接手的人一条边界说明。
        let boundary = format!(
            "源配置 {} 只读：vendor/ 不在本中枢的写入范围内，任何改动须由外部智能体在其职权内完成",
            a.source_file
        );
        if !prerequisites.contains(&boundary) {
            prerequisites.push(boundary);
        }

        FaultReport {
            asset_id: a.asset_id.clone(),
            kind: a.kind.clone(),
            asset_name: a.asset_name.clone(),
            server: Some(s.name.clone()),
            failure_class: primary,
            also_detected: also,
            location: hits[0].location.clone(),
            evidence,
            prerequisites,
            suggested_repair_kind: primary.suggested_repair_kind(),
            detected_at: self.now,
        }
    }

    /// 死根判定：路径的根段实测不存在 → dead_root。
    ///
    /// 这是本模块解释力最强的一条判据：根段没了，就同时解释了
    /// 为什么 command、args、cwd 全都失效，而不是三个独立故障。
    fn dead_root_hit(&self, a: &AssetProbe, field: &str, raw: &str) -> Option<Detected> {
        let root = root_segment(raw)?;
        let root_path = PathBuf::from(&root);
        if self.fs.exists(&root_path) {
            return None; // 根还在，交给更细的判据
        }
        let mut ev = vec![Evidence::probed_exists(
            format!("根目录 {} 实测不存在（本次 Test-Path 结果为假）", root),
            root.clone(),
            false,
        )];
        for anc in ancestors_nearest_first(raw) {
            if self.fs.exists(Path::new(&anc)) {
                ev.push(Evidence::probed_exists(
                    format!("仍然存在的最近祖先目录是 {}", anc),
                    anc.clone(),
                    true,
                ));
                break;
            }
            ev.push(Evidence::probed_exists(
                format!("祖先目录 {} 也实测不存在", anc),
                anc.clone(),
                false,
            ));
        }
        let mut prerequisites = vec![format!(
            "需要知道 {} 在当前环境里对应的真实位置（项目改名/搬迁后的新根）",
            root
        )];
        prerequisites.push(
            "该根段的消失同时解释了本条目的命令、参数与工作目录为何全部失效；修复前不必分别处理".to_string(),
        );

        // 搬家表命中时，把「换算出来的新路径」**送去实测**。
        //
        // 只有三条同时成立才写进「可以改指」的证据：
        //   1) 旧根实测不存在（否则走不到这里）；
        //   2) 新根实测存在；
        //   3) 换算后的完整路径实测存在。
        // 第 3 条是关键：只看前两条，会把「新根在、但那个文件不在新根下」
        // 也报成可改指，而照着改会得到一个同样起不来的配置。
        if let Some((old_root, candidate)) = remap_path(raw, &self.remaps) {
            let cand_pb = PathBuf::from(&candidate);
            let new_root_exists = self
                .remaps
                .iter()
                .find(|r| r.old_root == old_root)
                .map(|r| self.fs.exists(Path::new(&r.new_root)))
                .unwrap_or(false);
            // 判存在性用 exists 而不是 is_file：同一个判据同时服务
            // command（要文件）、args 与 cwd（可能是目录）。
            // 早先这里写的是 is_file，于是 matlab 的 cwd 明明在新根下存在，
            // 却被报成「换算后的路径实测不存在」——一条自相矛盾的证据。
            let cand_is_file = self.fs.is_file(&cand_pb);
            let cand_exists = self.fs.exists(&cand_pb);
            if new_root_exists && cand_exists {
                ev.push(Evidence::probed_exists(
                    format!(
                        "旧根 {} 有对应新根，且换算后的完整路径实测存在（{}）：{}",
                        old_root,
                        if cand_is_file { "文件" } else { "目录" },
                        candidate
                    ),
                    candidate.clone(),
                    true,
                ));
                prerequisites.push(format!(
                    "搬迁表命中并已实测：本条目的目标文件现在位于 {}（改指到这个路径是被验证过的，不是猜测）",
                    candidate
                ));
            } else if new_root_exists {
                // 新根在、文件不在：这条信息同样重要，必须报出来。
                // 它否掉的是「整个目录原样搬过去了」这个假设。
                ev.push(Evidence::probed_exists(
                    format!(
                        "搬迁表把旧根 {} 指到新根，但换算后的路径实测不存在：{}",
                        old_root, candidate
                    ),
                    candidate.clone(),
                    false,
                ));
                prerequisites.push(format!(
                    "注意：新根下没有与旧路径同形的文件（已实测 {}）。不能整根替换，需逐项确认该能力的真实入口",
                    candidate
                ));
            }
        }

        Some(Detected {
            class: FailureClass::DeadRoot,
            location: Location {
                field: field.to_string(),
                value: raw.to_string(),
                source_file: a.source_file.clone(),
            },
            evidence: ev,
            prerequisites,
        })
    }

    /// 绝对路径命令：不存在 → launch_path_missing；存在但不是文件 → no_executable_entry。
    fn command_path_hit(
        &self,
        a: &AssetProbe,
        field: &str,
        cmd: &str,
        _server: &str,
    ) -> Option<Detected> {
        if !find_unexpanded_vars(cmd).is_empty() {
            return None; // 带变量的路径交给 unexpanded 判据
        }
        let p = normalize_path(cmd);
        let pb = PathBuf::from(&p);
        if self.fs.is_file(&pb) {
            return None;
        }
        if self.fs.exists(&pb) {
            return Some(Detected {
                class: FailureClass::NoExecutableEntry,
                location: Location {
                    field: field.to_string(),
                    value: cmd.to_string(),
                    source_file: a.source_file.clone(),
                },
                evidence: vec![Evidence::probed_exists(
                    format!("{} 实测存在，但它不是一个文件，无法作为启动入口", p),
                    p.clone(),
                    true,
                )],
                prerequisites: vec![format!("需要 {} 这个目录里真正的启动脚本名", p)],
            });
        }
        let mut ev = vec![Evidence::probed_exists(
            format!("启动命令指向的 {} 实测不存在", p),
            p.clone(),
            false,
        )];
        for anc in ancestors_nearest_first(&p) {
            if self.fs.exists(Path::new(&anc)) {
                ev.push(Evidence::probed_exists(
                    format!("仍然存在的最近祖先目录是 {}（故障就在它下面这一层）", anc),
                    anc.clone(),
                    true,
                ));
                break;
            }
        }
        Some(Detected {
            class: FailureClass::LaunchPathMissing,
            location: Location {
                field: field.to_string(),
                value: cmd.to_string(),
                source_file: a.source_file.clone(),
            },
            evidence: ev,
            prerequisites: vec![format!("需要知道 {} 的正确启动脚本位置", p)],
        })
    }

    /// 未展开变量判定。
    ///
    /// **必须同时满足**两条才算故障：字符串里有变量形态，**且**该变量在
    /// 当前进程环境里未设置。只看形态会把已展开的变量当成故障——
    /// 那是误报，而误报会让整份报告失去可信度。
    fn unexpanded_into(
        &self,
        a: &AssetProbe,
        field: &str,
        value: &str,
        hits: &mut Vec<Detected>,
    ) {
        let vars = find_unexpanded_vars(value);
        if vars.is_empty() {
            return;
        }
        let mut unset: Vec<VarRef> = Vec::new();
        let mut ev: Vec<Evidence> = Vec::new();
        for v in vars {
            match self.env.var(&v.name) {
                Some(val) => {
                    let shown = truncate_for_evidence(&val);
                    ev.push(Evidence::probed(format!(
                        "变量 {} 在本进程环境里已设置（值 {}），这一处不构成未展开变量",
                        v.name, shown
                    )));
                }
                None => unset.push(v),
            }
        }
        if unset.is_empty() {
            return;
        }
        for v in &unset {
            ev.push(Evidence::probed(format!(
                "变量 {} 在本进程环境里未设置（env::var 返回 NotPresent）",
                v.name
            )));
        }
        let mut prerequisites: Vec<String> = unset
            .iter()
            .map(|v| format!("需要知道变量 {} 应该被展开成什么值", v.name))
            .collect();
        if unset.iter().any(|v| v.is_root_style()) {
            prerequisites.push(
                "该变量是「宿主插件根」语义：它的值应当指向本机上该插件被真正安装到的目录"
                    .to_string(),
            );
        }
        hits.push(Detected {
            class: FailureClass::UnexpandedVar,
            location: Location {
                field: field.to_string(),
                value: value.to_string(),
                source_file: a.source_file.clone(),
            },
            evidence: ev,
            prerequisites,
        });
    }

    /// 裸命令名判定：在 PATH 里找不到 → command_not_in_path。
    fn command_not_in_path_hit(
        &self,
        a: &AssetProbe,
        field: &str,
        cmd: &str,
    ) -> Option<Detected> {
        if self.path.which(cmd).is_some() {
            return None;
        }
        let n = self.path.dir_count();
        Some(Detected {
            class: FailureClass::CommandNotInPath,
            location: Location {
                field: field.to_string(),
                value: cmd.to_string(),
                source_file: a.source_file.clone(),
            },
            evidence: vec![Evidence::probed(format!(
                "裸命令 {} 在 PATH 的 {} 个目录里逐个试过（含 PATHEXT 后缀），一处都没解析到可执行文件",
                cmd, n
            ))],
            prerequisites: vec![format!(
                "需要知道 {} 的正确提供方式：装进 PATH、换成绝对路径、或用等价的替代命令",
                cmd
            )],
        })
    }

    fn healthy(
        &self,
        a: &AssetProbe,
        s: &McpServerConfig,
        location: Location,
        evidence: Vec<Evidence>,
    ) -> FaultReport {
        FaultReport {
            asset_id: a.asset_id.clone(),
            kind: a.kind.clone(),
            asset_name: a.asset_name.clone(),
            server: Some(s.name.clone()),
            failure_class: FailureClass::Healthy,
            also_detected: Vec::new(),
            location,
            evidence,
            prerequisites: Vec::new(),
            suggested_repair_kind: SuggestedRepairKind::None,
            detected_at: self.now,
        }
    }

    fn plain_report(
        &self,
        a: &AssetProbe,
        class: FailureClass,
        location: Location,
        evidence: Vec<Evidence>,
        prerequisites: Vec<String>,
    ) -> FaultReport {
        FaultReport {
            asset_id: a.asset_id.clone(),
            kind: a.kind.clone(),
            asset_name: a.asset_name.clone(),
            server: None,
            failure_class: class,
            also_detected: Vec::new(),
            location,
            evidence,
            prerequisites,
            suggested_repair_kind: class.suggested_repair_kind(),
            detected_at: self.now,
        }
    }
}

/// 内部用：一次命中。
#[derive(Debug, Clone)]
struct Detected {
    class: FailureClass,
    location: Location,
    evidence: Vec<Evidence>,
    prerequisites: Vec<String>,
}

/// 证据里的值截断。环境变量的值可能是长路径，但更可能是密钥——
/// 报告会被落库、过 HTTP、进 MCP，所以只在证据里留前 80 个字符，
/// 并且**只对已设置的变量**这么做（未设置的根本没值可泄）。
fn truncate_for_evidence(val: &str) -> String {
    const MAX: usize = 80;
    if val.chars().count() <= MAX {
        val.to_string()
    } else {
        format!("{}...", val.chars().take(MAX).collect::<String>())
    }
}

/// 当前 unix 秒。
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    // ── 测试替身：完全虚拟的文件系统 / PATH / 环境 ──────────────
    //
    // 用替身而不是真磁盘，是为了让「证据来自实测」这件事在单测里
    // 也能被验证：测试自己决定了 world 里有什么，报告就必须
    // 如实反映 world，而不是反映这台机器的真实盘符。

    #[derive(Default)]
    struct FakeFs {
        files: BTreeSet<String>,
        dirs: BTreeSet<String>,
    }

    impl FakeFs {
        /// 建一个文件，并顺带把它沿途的父目录都建上。
        ///
        /// 真实的文件系统里「文件存在」必然蕴含「父目录存在」，
        /// 替身如果不建模这一条，就会把正常路径误判成死根。
        fn with_file(mut self, p: &str) -> Self {
            let n = normalize_path(p);
            self.files.insert(n.clone());
            let mut cur = PathBuf::from(&n);
            while let Some(parent) = cur.parent() {
                if parent.as_os_str().is_empty() {
                    break;
                }
                self.dirs.insert(normalize_path(&parent.to_string_lossy()));
                let next = parent.to_path_buf();
                cur = next;
            }
            self
        }
        fn with_dir(mut self, p: &str) -> Self {
            self.dirs.insert(normalize_path(p));
            self
        }
    }

    impl ProbeFs for FakeFs {
        fn exists(&self, p: &Path) -> bool {
            let s = normalize_path(&p.to_string_lossy());
            self.files.contains(&s) || self.dirs.contains(&s)
        }
        fn is_file(&self, p: &Path) -> bool {
            self.files.contains(&normalize_path(&p.to_string_lossy()))
        }
        fn is_dir(&self, p: &Path) -> bool {
            self.dirs.contains(&normalize_path(&p.to_string_lossy()))
        }
    }

    struct FakePath {
        known: BTreeSet<String>,
        dirs: usize,
    }

    impl FakePath {
        fn with(known: &[&str]) -> Self {
            Self { known: known.iter().map(|s| s.to_string()).collect(), dirs: 7 }
        }
    }

    impl ProbePath for FakePath {
        fn which(&self, cmd: &str) -> Option<PathBuf> {
            if self.known.contains(cmd) {
                Some(PathBuf::from(format!("/fake/bin/{}", cmd)))
            } else {
                None
            }
        }
        fn dir_count(&self) -> usize {
            self.dirs
        }
    }

    #[derive(Default)]
    struct FakeEnv {
        vars: Vec<(String, String)>,
    }

    impl FakeEnv {
        fn with(mut self, k: &str, v: &str) -> Self {
            self.vars.push((k.to_string(), v.to_string()));
            self
        }
    }

    impl ProbeEnv for FakeEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        }
    }

    fn asset(id: &str, text: &str) -> AssetProbe {
        AssetProbe {
            asset_id: id.to_string(),
            kind: "mcp".to_string(),
            asset_name: id.trim_start_matches("mcp::").to_string(),
            source_file: format!("vendor/mcp/{}/.mcp.json", id),
            text: Some(text.to_string()),
        }
    }

    fn locate(fs: &FakeFs, path: &FakePath, env: &FakeEnv, a: &AssetProbe) -> Vec<FaultReport> {
        let mut l = Locator::new(fs, path, env);
        l.now = 1_700_000_000; // 固定时间，报告可比对
        l.locate(a)
    }

    // ── 判据本身 ────────────────────────────────────────────────

    #[test]
    fn dead_root_is_detected_from_a_missing_root_segment() {
        // 世界：盘符本身在，但旧根那一整片不在
        let fs = FakeFs::default().with_dir("D:/");
        let path = FakePath::with(&[]);
        let env = FakeEnv::default();
        let json = r#"{"mcpServers":{"s":{"command":"D:/.my-agent/.plugin/x/py.exe","args":[]}}}"#;
        let reps = locate(&fs, &path, &env, &asset("mcp::dead", json));
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].failure_class, FailureClass::DeadRoot);
        assert!(reps[0].has_probed_evidence(), "dead_root 必须带实测证据");
        let probed_false = reps[0]
            .evidence
            .iter()
            .any(|e| e.kind == EvidenceKind::Probed && e.exists == Some(false));
        assert!(probed_false, "必须有一条「实测不存在」的证据");
        assert_eq!(reps[0].location.field, "mcpServers.s.command");
        assert_eq!(reps[0].location.value, "D:/.my-agent/.plugin/x/py.exe");
    }

    #[test]
    fn dead_root_also_catches_the_same_root_in_args() {
        // 命令没问题，坏在参数里的旧根 —— 只查 command 会漏掉这一类。
        let fs = FakeFs::default().with_dir("D:/").with_file("D:/ok/py.exe");
        let path = FakePath::with(&[]);
        let env = FakeEnv::default();
        let json = r#"{"mcpServers":{"s":{"command":"D:/ok/py.exe","args":["D:/.my-agent/mcp/server_entry.py"]}}}"#;
        let reps = locate(&fs, &path, &env, &asset("mcp::args", json));
        assert_eq!(reps[0].failure_class, FailureClass::DeadRoot);
        assert_eq!(reps[0].location.field, "mcpServers.s.args[0]");
    }

    #[test]
    fn launch_path_missing_when_root_alive_but_the_file_is_gone() {
        let fs = FakeFs::default().with_dir("D:/").with_dir("D:/ok");
        let path = FakePath::with(&[]);
        let env = FakeEnv::default();
        let json = r#"{"mcpServers":{"s":{"command":"D:/ok/nope/run.cmd"}}}"#;
        let reps = locate(&fs, &path, &env, &asset("mcp::lp", json));
        assert_eq!(reps[0].failure_class, FailureClass::LaunchPathMissing);
        assert!(reps[0].has_probed_evidence());
        assert!(
            reps[0].evidence.iter().any(
                |e| e.exists == Some(true) && e.detail.contains("仍然存在的最近祖先")
            ),
            "必须指出断点在哪一层"
        );
    }

    #[test]
    fn unexpanded_var_requires_the_variable_to_actually_be_unset() {
        let fs = FakeFs::default();
        let path = FakePath::with(&[]);
        let env = FakeEnv::default(); // 什么变量都没设置
        let json = r#"{"mcpServers":{"s":{"command":"D:/python.exe","args":["${QODER_PLUGIN_ROOT}/mcp/x.py"]}}}"#;
        let reps = locate(&fs, &path, &env, &asset("mcp::uv", json));
        let c = &reps[0];
        assert!(c.classes().contains(&FailureClass::UnexpandedVar));
        assert!(c.evidence.iter().any(|e| e.detail.contains("未设置")));
    }

    #[test]
    fn a_variable_that_is_actually_set_is_not_reported_as_a_failure() {
        // 防误报的关键一条：只看变量形态会把已经能展开的配置判成坏。
        let fs = FakeFs::default().with_dir("D:/").with_file("D:/ok/py.exe");
        let path = FakePath::with(&[]);
        let env = FakeEnv::default().with("QODER_PLUGIN_ROOT", "D:/ok");
        let json = r#"{"mcpServers":{"s":{"command":"D:/ok/py.exe","args":["${QODER_PLUGIN_ROOT}/mcp/x.py"]}}}"#;
        let reps = locate(&fs, &path, &env, &asset("mcp::set", json));
        assert!(
            !reps[0].classes().contains(&FailureClass::UnexpandedVar),
            "变量已设置却报了 unexpanded_var，就是误报"
        );
    }

    #[test]
    fn bare_command_not_on_path_reports_how_many_dirs_were_probed() {
        let fs = FakeFs::default();
        let path = FakePath::with(&["npx"]); // uvx 不在
        let env = FakeEnv::default();
        let json = r#"{"mcpServers":{"s":{"command":"uvx","args":["mcp-for-blender"]}}}"#;
        let reps = locate(&fs, &path, &env, &asset("mcp::uvx", json));
        assert_eq!(reps[0].failure_class, FailureClass::CommandNotInPath);
        let ev = &reps[0].evidence[0];
        assert_eq!(ev.kind, EvidenceKind::Probed);
        assert!(ev.detail.contains("7 个目录"), "必须给出实测的 PATH 目录数：{}", ev.detail);
    }

    #[test]
    fn command_found_on_path_is_healthy() {
        let fs = FakeFs::default();
        let path = FakePath::with(&["npx"]);
        let env = FakeEnv::default();
        let json = r#"{"mcpServers":{"s":{"command":"npx","args":["-y","zion-mcp"]}}}"#;
        let reps = locate(&fs, &path, &env, &asset("mcp::npx", json));
        assert_eq!(reps[0].failure_class, FailureClass::Healthy);
        assert!(!reps[0].is_failure());
        assert!(reps[0].evidence.iter().any(|e| e.exists == Some(true) && e.detail.contains("解析到")));
    }

    #[test]
    fn remote_http_server_needs_no_local_executable() {
        let fs = FakeFs::default();
        let path = FakePath::with(&[]);
        let env = FakeEnv::default();
        let json = r#"{"mcpServers":{"notion":{"type":"http","url":"https://mcp.notion.com/mcp"}}}"#;
        let reps = locate(&fs, &path, &env, &asset("mcp::http", json));
        assert_eq!(reps[0].failure_class, FailureClass::Healthy);
        assert!(reps[0].evidence[0].detail.contains("远程 HTTP"));
    }

    #[test]
    fn multiple_servers_under_one_asset_get_separate_reports() {
        let fs = FakeFs::default().with_dir("D:/");
        let path = FakePath::with(&["npx"]);
        let env = FakeEnv::default();
        let fs = fs.with_dir("D:/nope");
        let json = r#"{"mcpServers":{"bad":{"command":"D:/nope/x.exe"},"good":{"command":"npx","args":[]}}}"#;
        let reps = locate(&fs, &path, &env, &asset("mcp::two", json));
        assert_eq!(reps.len(), 2);
        let good = reps.iter().find(|r| r.server.as_deref() == Some("good")).unwrap();
        let bad = reps.iter().find(|r| r.server.as_deref() == Some("bad")).unwrap();
        assert_eq!(good.failure_class, FailureClass::Healthy);
        assert_eq!(bad.failure_class, FailureClass::LaunchPathMissing);
        assert_ne!(good.fault_key(), bad.fault_key());
    }

    #[test]
    fn broken_json_is_config_unreadable_not_a_silent_healthy() {
        let fs = FakeFs::default();
        let path = FakePath::with(&[]);
        let env = FakeEnv::default();
        let reps = locate(&fs, &path, &env, &asset("mcp::bad", "{not json"));
        assert_eq!(reps[0].failure_class, FailureClass::ConfigUnreadable);
        assert!(reps[0].has_probed_evidence(), "解析失败是本次实测的结论");
    }

    #[test]
    fn a_missing_source_file_is_reported_with_false_probe() {
        let fs = FakeFs::default();
        let path = FakePath::with(&[]);
        let env = FakeEnv::default();
        let a = AssetProbe {
            asset_id: "mcp::gone".into(),
            kind: "mcp".into(),
            asset_name: "gone".into(),
            source_file: "vendor/gone/.mcp.json".into(),
            text: None,
        };
        let reps = locate(&fs, &path, &env, &a);
        assert_eq!(reps[0].failure_class, FailureClass::ConfigUnreadable);
        assert_eq!(reps[0].evidence[0].exists, Some(false));
    }

    #[test]
    fn primary_class_wins_by_root_cause_order_and_the_rest_are_kept() {
        // 同时命中死根与未展开变量：死根更靠近根因，当主分类；
        // 未展开变量必须进 also_detected，不能凭空消失。
        let fs = FakeFs::default().with_dir("D:/");
        let path = FakePath::with(&[]);
        let env = FakeEnv::default();
        let json = r#"{"mcpServers":{"s":{"command":"D:/.my-agent/py.exe","args":["${QODER_PLUGIN_ROOT}/x.py"]}}}"#;
        let reps = locate(&fs, &path, &env, &asset("mcp::both", json));
        let r = &reps[0];
        assert_eq!(r.failure_class, FailureClass::DeadRoot);
        assert!(r.also_detected.contains(&FailureClass::UnexpandedVar));
        assert!(r.classes().contains(&FailureClass::UnexpandedVar));
    }

    // ── 根目录搬迁表：只报实测成立的重映射 ──────────────────────

    #[test]
    fn remap_path_rewrites_only_under_the_old_root() {
        let remaps = vec![RootRemap::new("D:/.my-agent", "C:/external-assets")];
        let (old, got) = remap_path("D:/.my-agent/.plugin/x/y.exe", &remaps).unwrap();
        assert_eq!(old, normalize_path("D:/.my-agent"));
        let mut want = normalize_path("C:/external-assets");
        for seg in [".plugin", "x", "y.exe"] {
            want.push(SEP);
            want.push_str(seg);
        }
        assert_eq!(got, want);
        // 不在旧根下的路径不该被改写
        assert!(remap_path("D:/.other/x", &remaps).is_none());
    }

    #[test]
    fn remap_path_is_case_insensitive_on_the_drive_letter() {
        let remaps = vec![RootRemap::new("D:/.my-agent", "C:/external-assets")];
        assert!(remap_path("d:/.MY-AGENT/x", &remaps).is_some());
    }

    #[test]
    fn remap_evidence_is_claimed_only_when_the_target_really_exists() {
        let fs = FakeFs::default()
            .with_dir("D:/")
            .with_file("C:/external-assets/.plugin/x/py.exe");
        let path = FakePath::with(&[]);
        let env = FakeEnv::default();
        let remaps = vec![RootRemap::new("D:/.my-agent", "C:/external-assets")];
        let json = r#"{"mcpServers":{"s":{"command":"D:/.my-agent/.plugin/x/py.exe"}}}"#;
        let a = asset("mcp::remap", json);
        let mut l = Locator::new(&fs, &path, &env).with_remaps(remaps);
        l.now = 1;
        let reps = l.locate(&a);
        assert!(reps[0]
            .evidence
            .iter()
            .any(|e| e.exists == Some(true) && e.detail.contains("换算后的完整路径实测存在")));
    }

    #[test]
    fn remap_does_not_claim_a_target_that_does_not_exist() {
        // 防最危险的误报：新根在，但同形文件并不在。
        // 若这里也报 exists=true，接手的智能体会照着一个错路径改配置。
        let fs = FakeFs::default().with_dir("D:/").with_dir("C:/external-assets");
        let path = FakePath::with(&[]);
        let env = FakeEnv::default();
        let remaps = vec![RootRemap::new("D:/.my-agent", "C:/external-assets")];
        let json = r#"{"mcpServers":{"s":{"command":"D:/.my-agent/missing/py.exe"}}}"#;
        let a = asset("mcp::noremap", json);
        let mut l = Locator::new(&fs, &path, &env).with_remaps(remaps);
        l.now = 1;
        let reps = l.locate(&a);
        assert!(
            !reps[0]
                .evidence
                .iter()
                .any(|e| e.detail.contains("换算后的完整路径实测存在")),
            "目标不存在却报了可改指，这是会误导维修的假证据"
        );
    }

    // ── 工具函数 ────────────────────────────────────────────────

    #[test]
    fn root_segment_takes_drive_plus_the_first_directory() {
        let got = root_segment("D:/.my-agent/.plugin/x/y.exe").unwrap();
        let mut want = String::from("D:");
        want.push(SEP);
        want.push_str(".my-agent");
        assert_eq!(got, want);
    }

    #[test]
    fn root_segment_is_none_for_a_bare_command() {
        assert!(root_segment("uvx").is_none());
    }

    #[test]
    fn is_absolute_like_distinguishes_drive_paths_from_bare_commands() {
        assert!(is_absolute_like("D:/x/y.exe"));
        assert!(is_absolute_like("C:/x"));
        assert!(!is_absolute_like("uvx"));
        assert!(!is_absolute_like("npx"));
        assert!(!is_absolute_like(""));
    }

    #[test]
    fn parse_skips_underscore_prefixed_comment_keys() {
        let json = r#"{"mcpServers":{"_comment":"not a server","real":{"command":"npx"}}}"#;
        let s = parse_mcp_servers(json).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "real");
    }

    #[test]
    fn parse_accepts_a_bare_server_table_without_the_wrapper() {
        let s = parse_mcp_servers(r#"{"only":{"command":"npx"}}"#).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "only");
    }

    #[test]
    fn env_values_in_evidence_are_truncated_so_long_values_cannot_flood_the_report() {
        let long = "x".repeat(500);
        let out = truncate_for_evidence(&long);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().count(), 83);
    }

    #[test]
    fn locate_report_counts_and_rollup_agree_with_the_reports() {
        let fs = FakeFs::default().with_dir("D:/");
        let path = FakePath::with(&["npx"]);
        let env = FakeEnv::default();
        let mut all = locate(
            &fs,
            &path,
            &env,
            &asset("mcp::a", r#"{"mcpServers":{"s":{"command":"npx"}}}"#),
        );
        all.extend(locate(
            &fs,
            &path,
            &env,
            &asset("mcp::b", r#"{"mcpServers":{"s":{"command":"D:/no/x.exe"}}}"#),
        ));
        let rep = LocateReport::from_reports(all);
        assert_eq!(rep.total_units, 2);
        assert_eq!(rep.healthy, 1);
        assert_eq!(rep.failed, 1);
        let roll = rep.asset_rollup();
        assert_eq!(roll.len(), 2);
        assert_eq!(roll.iter().filter(|v| v.is_failure()).count(), 1);
    }
}