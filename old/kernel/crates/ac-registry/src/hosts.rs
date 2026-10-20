//! 智能体协议注册表：中枢认知里存在的全部宿主，以及它们各自的协议支持矩阵。
//!
//! 用户原话：
//!
//! > 将智能体协议清单罗列所支持的智能体，包括但不限于 claude-code、codex、
//! > qoder、minimax-code、workbuddy、kimi-code、deepseek-harness 等智能体、
//! > 协议及支持状态，并且后端应该支撑并包含这些智能体连接状态和协议状态的
//! > 相应依赖，存储在该能力核心相应目录内部。
//!
//! # 两个存储，各回答一半问题
//!
//! 「数据库 + 分散文件夹」是项目的双存储纪律。这一层把它用满：
//!
//! - \`<data_dir>/hosts/<id>.json\` —— 清单（可读、可改、可手写新增）。
//!   首次运行由 seed_defaults 写入内置清单；**已存在的文件不覆盖**，
//!   用户改过的协议支持、自己加的宿主都保留。
//! - \`agent_hosts\` 表 —— 状态（探测结果：装没装、接没接、上次什么时候）。
//!   每次 Registry::sync_hosts 重新写一遍，它是可从磁盘重算的投影。
//!
//! 为什么状态不能回写进 json：那些文件是用户资产，每次探测都把
//! detected / evidence 刷回去，等于把用户手写的东西冲掉。
//!
//! # detected：有证据才认
//!
//! **判不出来就是 false，绝不猜。** evidence 里逐条写明查了哪里、命中没有：
//!
//! - 命中 → 命中：C:/Builds/PublicUser01/.codex（Codex 用户配置目录）
//! - 未命中 → 未命中：C:/Builds/PublicUser01/.claude（Claude Code 用户配置目录）
//!
//! 界面上一个 detected=false 后面跟着这串证据，用户才能分辨
//! 「真的没装」和「我们没去找」——这是本模块最重要的一条诚实性要求。
//!
//! # connected：只认实报，不认推断
//!
//! 与 agents 模块的通道语义严格对齐：
//!
//! | 通道 | 含义 | 算不算 connected |
//! |---|---|---|
//! | heartbeat | 宿主主动写心跳文件 | 算（online / stale） |
//! | mcp       | 宿主主动调注册端点 | 算（在窗口期内） |
//! | static    | 中枢从磁盘路径推断出来的 | **不算** |
//!
//! static 不算的关键：它是「这堆文件看起来属于 qoder」，不是「qoder 现在连着」。
//! 把推断当成连接，界面上就会出现一个绿点，而那个宿主根本没运行——
//! 这正是 agents 模块从表结构上就禁止的混淆。
//!
//! # 协议维度
//!
//! 每个宿主登记多条协议，每条带 support：
//!
//! - supported —— 中枢已实现该协议的读取路径，且真实语料里有样本；
//! - partial   —— 能读，但只覆盖部分字段 / 样本不足；
//! - planned   —— 中枢尚未实现，仅登记「该宿主使用这一协议」。
//!
//! 这三档是**中枢的实现状态**，不是宿主的官方能力声明。所以 note
//! 必须写清楚依据（哪份样本、缺哪些字段），否则这一列就是装饰。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 清单目录名，约定在数据目录下：<data_dir>/hosts/
pub const HOSTS_DIR_NAME: &str = "hosts";

/// 清单文件扩展名（不含点）。与 agents::HEARTBEAT_FILE_EXT 同款坑：
/// Path::extension() 返回的就是不带点的形式，写成 ".json" 恒不命中。
pub const HOST_FILE_EXT: &str = "json";

// ── 协议传输方式（对外契约，改动等于破坏前端表格）───────────────

pub const TRANSPORT_MCP_STDIO: &str = "mcp-stdio";
pub const TRANSPORT_MCP_HTTP: &str = "mcp-http";
pub const TRANSPORT_CLI: &str = "cli";
pub const TRANSPORT_JSON_CONFIG: &str = "json-config";
pub const TRANSPORT_SKILL_MD: &str = "skill-md";

/// 全部合法的 transport 值（校验与测试共用一份）。
pub const ALL_TRANSPORTS: &[&str] = &[
    TRANSPORT_MCP_STDIO,
    TRANSPORT_MCP_HTTP,
    TRANSPORT_CLI,
    TRANSPORT_JSON_CONFIG,
    TRANSPORT_SKILL_MD,
];

/// 支持状态（对外契约，**小写**，前端按这三个值分档）。
pub const SUPPORT_SUPPORTED: &str = "supported";
pub const SUPPORT_PARTIAL: &str = "partial";
pub const SUPPORT_PLANNED: &str = "planned";

/// 全部合法的 support 值。
pub const ALL_SUPPORTS: &[&str] = &[SUPPORT_SUPPORTED, SUPPORT_PARTIAL, SUPPORT_PLANNED];

/// 宿主类型。plugin / cli / mcp / harness。
pub const KIND_PLUGIN: &str = "plugin";
pub const KIND_CLI: &str = "cli";
pub const KIND_MCP: &str = "mcp";
pub const KIND_HARNESS: &str = "harness";

/// 全部合法的 kind 值。
pub const ALL_KINDS: &[&str] = &[KIND_PLUGIN, KIND_CLI, KIND_MCP, KIND_HARNESS];

fn default_kind() -> String {
    KIND_HARNESS.to_string()
}

// ── 数据形状 ──────────────────────────────────────────────────────

/// 一条协议支持声明。
///
/// transport / support 用**字符串常量**而不是枚举落库：这两个值要过 HTTP、
/// 进前端表格、被用户手写进 json，枚举一旦改名就同时破坏三处。
/// 常量 + 写入前的校验（HostProtocol::validate）更稳。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostProtocol {
    /// 协议显示名，例如 SKILL.md / MCP (stdio) / plugin.json。
    pub name: String,
    /// mcp-stdio | mcp-http | cli | json-config | skill-md
    pub transport: String,
    /// supported | partial | planned
    pub support: String,
    /// 依据。允许空串，但空串意味着「没写依据」。
    #[serde(default)]
    pub note: String,
}

impl HostProtocol {
    pub fn new(name: &str, transport: &str, support: &str, note: &str) -> Self {
        Self {
            name: name.to_string(),
            transport: transport.to_string(),
            support: support.to_string(),
            note: note.to_string(),
        }
    }

    /// 校验两个对外契约值。返回 None 表示合法。
    ///
    /// 为什么要校验：这两个字段是前端表格的分档依据。写错一个（比如
    /// "Supported" 大写、或 "mcp_stdio" 下划线）不会报错，只会让那一格
    /// 颜色变成默认色——**静默的错误比崩溃更难查**。
    pub fn validate(&self) -> Option<String> {
        if !ALL_TRANSPORTS.contains(&self.transport.as_str()) {
            return Some(format!(
                "协议 {} 的 transport={:?} 不在契约值里（只接受 {}）",
                self.name,
                self.transport,
                ALL_TRANSPORTS.join(" / ")
            ));
        }
        if !ALL_SUPPORTS.contains(&self.support.as_str()) {
            return Some(format!(
                "协议 {} 的 support={:?} 不在契约值里（只接受 {}）",
                self.name,
                self.support,
                ALL_SUPPORTS.join(" / ")
            ));
        }
        None
    }
}

/// 一条探测规则。**可序列化**：它是用户能编辑的清单的一部分。
///
/// 每条规则都要带 note：探测不命中时，界面要能说清「查的是哪里、为什么查那里」。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DetectRule {
    /// home 目录下存在某个相对路径（~/.codex）。
    HomeDir {
        rel: String,
        #[serde(default)]
        note: String,
    },
    /// 某个绝对路径存在。
    AbsPath {
        path: String,
        #[serde(default)]
        note: String,
    },
    /// 某个环境变量**指向一个存在的目录**（DSH_HOME）。
    EnvDir {
        var: String,
        #[serde(default)]
        note: String,
    },
    /// PATH 里能找到某个命令。
    Cli {
        names: Vec<String>,
        #[serde(default)]
        note: String,
    },
    /// 某个目录下存在以某前缀开头的子目录（插件根）。
    PluginDir {
        root: String,
        prefix: String,
        #[serde(default)]
        note: String,
    },
}

/// 清单文件（<data_dir>/hosts/<id>.json）的形状：**静态声明**。
///
/// 刻意不带 detected / connected / last_seen：那些是每次探测重算的事实，
/// 写进用户可编辑的文件里只会被覆盖或造成不一致。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostSpec {
    /// kebab-case 稳定 id（对外契约）。
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    /// 中枢推断宿主生态时可能产生的键（把 origin_agent 映射到宿主 id）。
    /// 例如 claude-code 的别名是 claude（.claude-plugin 目录推出来的就是它）。
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub detect: Vec<DetectRule>,
    #[serde(default)]
    pub protocols: Vec<HostProtocol>,
}

impl HostSpec {
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.id.trim().is_empty() {
            errs.push("id 为空".to_string());
        } else if !self
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            errs.push(format!(
                "id {:?} 必须是 kebab-case（小写字母/数字/连字符）",
                self.id
            ));
        }
        if !ALL_KINDS.contains(&self.kind.as_str()) {
            errs.push(format!(
                "kind={:?} 不在契约值里（只接受 {}）",
                self.kind,
                ALL_KINDS.join(" / ")
            ));
        }
        for p in &self.protocols {
            if let Some(e) = p.validate() {
                errs.push(e);
            }
        }
        errs
    }

    /// 该宿主的全部可识别生态键（id + 别名，全小写）。
    pub fn ecosystem_keys(&self) -> Vec<String> {
        let mut v: Vec<String> = vec![self.id.to_ascii_lowercase()];
        for a in &self.aliases {
            let a = a.trim().to_ascii_lowercase();
            if !a.is_empty() && !v.contains(&a) {
                v.push(a);
            }
        }
        v
    }

    /// 是否登记了某种传输方式（三档都算——planned 只是中枢尚未实现）。
    ///
    /// 为什么 planned 也算：asset_host_support 回答的是
    /// 「这个资产能被哪些宿主使用」，不是「中枢现在能把它装进哪些宿主」。
    /// 一个宿主官方支持 MCP，那这份 MCP 资产对它就是可用的，
    /// 哪怕中枢暂时还没实现安装路径。实现状态由 support 列单独表达。
    pub fn supports_transport(&self, transport: &str) -> bool {
        self.protocols.iter().any(|p| p.transport == transport)
    }
}

/// 一次探测的最终结果——即 agent_hosts 表的一行，也是对外返回的形状。
///
/// # 字段名就是契约
///
/// 前端（apps/desktop/src/panel-types.ts）按 **snake_case** 字段名消费：
/// display_name / last_seen / protocols / evidence。
/// Tauri 的 invoke 走纯 serde 序列化，**不做键名转换**，
/// 所以这里绝不能加 rename_all = "camelCase"。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentHost {
    /// kebab-case 稳定 id。
    pub id: String,
    pub display_name: String,
    pub vendor: String,
    /// plugin | cli | mcp | harness
    pub kind: String,
    /// 本机是否装了它。**有证据才 true**。
    pub detected: bool,
    /// 此刻是否接着中枢。只认心跳/端点实报，静态推断不算。
    pub connected: bool,
    /// unix **秒**；0 = 从未见过。
    pub last_seen: i64,
    pub protocols: Vec<HostProtocol>,
    /// 判定依据：每条探测规则一行（命中/未命中 + 查的是哪里）。
    pub evidence: Vec<String>,
}

impl AgentHost {
    pub fn from_spec(spec: &HostSpec) -> Self {
        Self {
            id: spec.id.clone(),
            display_name: if spec.display_name.trim().is_empty() {
                spec.id.clone()
            } else {
                spec.display_name.clone()
            },
            vendor: spec.vendor.clone(),
            kind: if spec.kind.trim().is_empty() {
                default_kind()
            } else {
                spec.kind.clone()
            },
            detected: false,
            connected: false,
            last_seen: 0,
            protocols: spec.protocols.clone(),
            evidence: Vec::new(),
        }
    }

    /// 条目级自检：id 与协议契约值是否合法。
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.id.trim().is_empty() {
            errs.push("id 为空".to_string());
        }
        if !ALL_KINDS.contains(&self.kind.as_str()) {
            errs.push(format!("kind={:?} 不在契约值里", self.kind));
        }
        for p in &self.protocols {
            if let Some(e) = p.validate() {
                errs.push(e);
            }
        }
        errs
    }
}

/// seed_defaults 的结果。首次启动与后续启动必须能从返回值区分开。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSeedReport {
    /// 清单目录。
    pub dir: String,
    /// 本次**新写**的宿主 id。
    pub created: Vec<String>,
    /// 本来就在、本次**没碰**的宿主 id（用户改动因此保留）。
    pub existing: Vec<String>,
}

/// 一个读不懂的清单文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostReject {
    pub file: String,
    pub reason: String,
}

/// sync_hosts 的结果：磁盘清单 → 数据库投影。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSyncReport {
    pub seed: HostSeedReport,
    /// 从清单目录读到的宿主数（含用户自己加的）。
    pub specs_loaded: u32,
    /// 写进 agent_hosts 的行数。
    pub rows_written: u32,
    /// 其中 detected 的行数。
    pub detected: u32,
    /// 其中 connected 的行数。
    pub connected: u32,
    /// 清单文件存在但读不懂的（不静默吞掉：用户手改坏了要看得见）。
    pub rejected: Vec<HostReject>,
    /// 库里有、但当前清单里已经没有的宿主 id。
    ///
    /// **不删除**（增量不删）：行留着，这里如实报出来。删掉它等于
    /// 抹掉「这个宿主曾经存在过」的事实，而那是审计要的东西。
    pub orphans: Vec<String>,
}

/// 注册表总览数字。全部来自 SQL / 本次探测，没有占位值。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostsOverview {
    pub total: u32,
    pub detected: u32,
    pub connected: u32,
    /// 已登记协议条目总数（所有宿主加起来）。
    pub protocol_rows: u32,
    /// 按支持档位分组：supported / partial / planned 各多少条。
    pub by_support: Vec<HostCount>,
    /// 按宿主类型分组：plugin / cli / mcp / harness 各多少。
    pub by_kind: Vec<HostCount>,
    /// 清单目录。界面要能把路径显示给用户（清单在这里，你可以自己加）。
    pub hosts_dir: String,
}

/// 一个「键 → 数量」的计数行（与 KindCount 同形状，但语义不同，不复用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCount {
    pub key: String,
    pub count: u32,
}
// ── 探测环境（可注入，测试不依赖本机装了什么）───────────────────

/// 探测需要的外部环境。抽成 trait 是为了让测试能造一个
/// 「装了 A 没装 B」的假机器——否则测试结果会随开发机安装情况漂移，
/// 那种测试比没有更糟（本地绿、CI 红，或者反过来）。
pub trait ProbeEnv {
    fn home(&self) -> Option<PathBuf>;
    fn var(&self, key: &str) -> Option<String>;
    fn exists(&self, path: &Path) -> bool;
    /// PATH 查找。命中返回绝对路径。
    fn which(&self, cmd: &str) -> Option<PathBuf>;
    /// 列出一个目录下的**子目录名**（不递归）。目录不可读返回空表。
    ///
    /// 为什么它必须在 trait 上：插件根探测（共享插件目录下有没有 dsh-*）
    /// 是最关键的一条 dsh 证据。如果它绕过 trait 直接读真实文件系统，
    /// 测试里的「假机器」就会漏出真实机器上的插件，于是"本机装了什么"
    /// 会渗进单元测试——那种测试在不同机器上给出不同结果，比没有测试更糟。
    fn list_dirs(&self, path: &Path) -> Vec<String>;
}

/// 真实机器。
#[derive(Debug, Clone, Copy, Default)]
pub struct RealEnv;

impl ProbeEnv for RealEnv {
    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
    }

    fn var(&self, key: &str) -> Option<String> {
        std::env::var_os(key).map(|v| v.to_string_lossy().to_string())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn which(&self, cmd: &str) -> Option<PathBuf> {
        which_in_path(cmd)
    }

    fn list_dirs(&self, path: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        out.sort();
        out
    }
}

/// 在 PATH 里查命令。Windows 上还要试 PATHEXT 里的扩展名。
fn which_in_path(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var_os("PATHEXT")
            .map(|v| {
                v.to_string_lossy()
                    .split(';')
                    .map(|s| s.trim().to_ascii_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| vec![".exe".into(), ".cmd".into(), ".bat".into()])
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let base = dir.join(cmd);
        if base.is_file() {
            return Some(base);
        }
        for ext in &exts {
            let p = dir.join(format!("{cmd}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

// ── 探测 ──────────────────────────────────────────────────────────

/// 跑一遍全部探测规则，返回 (是否装了, 证据)。
///
/// # 证据必须逐条产出，命中与未命中都要写
///
/// 只写命中的话，detected=false 的宿主在界面上就是一个空数组——
/// 用户没法区分「真没装」和「这个版本没加规则」。
pub fn detect(spec: &HostSpec, env: &dyn ProbeEnv) -> (bool, Vec<String>) {
    let mut ev: Vec<String> = Vec::new();
    let mut hit = false;
    let mut hits = 0usize;
    for rule in &spec.detect {
        let note = rule_note(rule);
        let (ok, detail) = eval_rule(rule, env);
        if ok {
            hit = true;
            hits += 1;
        }
        let mark = if ok { "命中" } else { "未命中" };
        let tail = if note.trim().is_empty() {
            String::new()
        } else {
            format!("（{note}）")
        };
        ev.push(format!("{mark}：{detail}{tail}"));
    }
    if spec.detect.is_empty() {
        ev.push("未配置任何探测规则：只能判为未检测到（不是「没装」，是「没查」）".to_string());
    } else if hit {
        ev.push(format!(
            "判定：已安装（{} 条规则命中 {} 条）",
            spec.detect.len(),
            hits
        ));
    } else {
        ev.push(format!(
            "判定：未检测到（已按 {} 条规则查过 {} 处；查不到不等于没装，但本表不猜）",
            spec.detect.len(),
            spec.detect.len()
        ));
    }
    (hit, ev)
}

fn rule_note(rule: &DetectRule) -> &str {
    match rule {
        DetectRule::HomeDir { note, .. }
        | DetectRule::AbsPath { note, .. }
        | DetectRule::EnvDir { note, .. }
        | DetectRule::Cli { note, .. }
        | DetectRule::PluginDir { note, .. } => note,
    }
}

fn eval_rule(rule: &DetectRule, env: &dyn ProbeEnv) -> (bool, String) {
    match rule {
        DetectRule::HomeDir { rel, .. } => {
            let Some(home) = env.home() else {
                return (false, format!("~/{rel}（未取到 home 目录）"));
            };
            let p = home.join(rel);
            let ok = env.exists(&p);
            (ok, format!("{}", p.display()))
        }
        DetectRule::AbsPath { path, .. } => {
            let p = Path::new(path);
            (env.exists(p), path.clone())
        }
        DetectRule::EnvDir { var, .. } => match env.var(var) {
            Some(v) if !v.trim().is_empty() => {
                let p = PathBuf::from(v.trim());
                let ok = env.exists(&p);
                (ok, format!("环境变量 {var} = {}", p.display()))
            }
            _ => (false, format!("环境变量 {var} 未设置")),
        },
        DetectRule::Cli { names, .. } => {
            for name in names {
                if let Some(p) = env.which(name) {
                    return (true, format!("PATH 中的 {name} → {}", p.display()));
                }
            }
            (false, format!("PATH 中的 {} 均未找到", names.join(" / ")))
        }
        DetectRule::PluginDir { root, prefix, .. } => {
            // 走 env.list_dirs 而不是 std::fs::read_dir：见 ProbeEnv 的文档，
            // 绕过 trait 会让真实机器的插件渗进单元测试。
            let dirs = env.list_dirs(Path::new(root));
            if dirs.is_empty() {
                return (false, format!("{root}（目录不可读、不存在，或没有子目录）"));
            }
            let mut hits: Vec<String> = dirs
                .into_iter()
                .filter(|n| n.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()))
                .collect();
            hits.sort();
            if hits.is_empty() {
                (false, format!("{root}/{prefix}*（没有匹配的子目录）"))
            } else {
                (
                    true,
                    format!("{root} 下 {} 个 {prefix}* 插件目录", hits.len()),
                )
            }
        }
    }
}

// ── 内置清单 ──────────────────────────────────────────────────────

fn proto(name: &str, transport: &str, support: &str, note: &str) -> HostProtocol {
    HostProtocol::new(name, transport, support, note)
}

/// 插件根：本机真实存在的共享插件目录。
///
/// 硬编码这个路径是**有意的**：它是这台机器上插件部署的既定位置
/// （见该目录下的 _DEPLOYMENT-*.md）。它只作为**一条**探测规则出现，
/// 不是唯一证据，代码里也不拿它当「某宿主一定装了」的判据。
///
/// 用正斜杠写：Windows 的 Path 接受两种分隔符，正斜杠不必转义。
const SHARED_PLUGIN_ROOT: &str = "C:/external-assets/.plugin";
/// 内置宿主清单。
///
/// # 为什么是这 9 个
///
/// 用户点名的 7 个（claude-code / codex / qoder / minimax-code / workbuddy /
/// kimi-code / deepseek-harness）一个不少；另外两个（cursor / zcode）是因为
/// **真实语料里有它们的插件标记目录**（.cursor-plugin 2 个、.zcode-plugin 3 个），
/// 登记进清单才能解释那些资产归谁。
///
/// 没有登记 obsidian / matlab / zion：它们在库里是**包名**推出来的生态，
/// 不是带 .xxx-plugin 目录的宿主。硬把它们列成「智能体」会让协议表变成
/// 装饰——一个 MATLAB MCP server 并不是一个智能体宿主。
///
/// # support 三档的判据
///
/// - supported：中枢的解析器确实验过这种文件（parse.rs 三种格式），
///   或该宿主的协议在真实语料里有样本；
/// - partial：中枢能读这个宿主的主格式，但它的扩展字段/私有配置还没覆盖；
/// - planned：中枢尚未实现，只登记「该宿主使用这一协议」。
///
/// 每条都带 note 写依据。**没有 note 的支持状态等于装饰。**
pub fn builtin_specs() -> Vec<HostSpec> {
    let shared_root = SHARED_PLUGIN_ROOT.to_string();
    vec![
        HostSpec {
            id: "claude-code".into(),
            display_name: "Claude Code".into(),
            vendor: "Anthropic".into(),
            kind: KIND_CLI.into(),
            aliases: vec!["claude".into()],
            detect: vec![
                DetectRule::HomeDir {
                    rel: ".claude".into(),
                    note: "Claude Code 的用户配置目录".into(),
                },
                DetectRule::Cli {
                    names: vec!["claude".into()],
                    note: "PATH 里的 claude 命令".into(),
                },
            ],
            protocols: vec![
                proto(
                    "SKILL.md",
                    TRANSPORT_SKILL_MD,
                    SUPPORT_SUPPORTED,
                    "真实语料 .claude-plugin 目录 4 份，SKILL.md 解析路径已入库",
                ),
                proto(
                    "plugin.json",
                    TRANSPORT_JSON_CONFIG,
                    SUPPORT_SUPPORTED,
                    ".claude-plugin/plugin.json 已解析，含 author 字段（对象与字符串两种写法）",
                ),
                proto(
                    "MCP (stdio)",
                    TRANSPORT_MCP_STDIO,
                    SUPPORT_SUPPORTED,
                    ".mcp.json 的 mcpServers.command 已解析并做入口探测",
                ),
                proto(
                    "MCP (http)",
                    TRANSPORT_MCP_HTTP,
                    SUPPORT_SUPPORTED,
                    ".mcp.json 的 url 字段已解析，判为 remote",
                ),
                proto(
                    "CLI",
                    TRANSPORT_CLI,
                    SUPPORT_SUPPORTED,
                    "PATH 查找已就位；本机未装故 detected=false（证据见 evidence）",
                ),
            ],
        },
        HostSpec {
            id: "codex".into(),
            display_name: "Codex".into(),
            vendor: "OpenAI".into(),
            kind: KIND_CLI.into(),
            aliases: vec![],
            detect: vec![
                DetectRule::HomeDir {
                    rel: ".codex".into(),
                    note: "Codex 用户配置目录".into(),
                },
                DetectRule::Cli {
                    names: vec!["codex".into()],
                    note: "PATH 里的 codex 命令".into(),
                },
            ],
            protocols: vec![
                proto(
                    "SKILL.md",
                    TRANSPORT_SKILL_MD,
                    SUPPORT_SUPPORTED,
                    "真实语料 .codex-plugin 目录 2 份，SKILL.md 解析路径与 claude 同一条",
                ),
                proto(
                    "plugin.json",
                    TRANSPORT_JSON_CONFIG,
                    SUPPORT_SUPPORTED,
                    ".codex-plugin/plugin.json 已解析（含 name 对象形式的 author）",
                ),
                proto(
                    "MCP (stdio)",
                    TRANSPORT_MCP_STDIO,
                    SUPPORT_SUPPORTED,
                    ".mcp.json 解析 + 启动命令探测已实现",
                ),
                proto(
                    "MCP (http)",
                    TRANSPORT_MCP_HTTP,
                    SUPPORT_PARTIAL,
                    "url 字段能读，但 Codex 的 server 表结构与 claude 的差异未逐字段验证",
                ),
                proto(
                    "CLI",
                    TRANSPORT_CLI,
                    SUPPORT_SUPPORTED,
                    "PATH 查找已就位（本机有 ~/.codex 但没有 codex 命令，故只认目录证据）",
                ),
            ],
        },
        HostSpec {
            id: "qoder".into(),
            display_name: "Qoder".into(),
            vendor: "Qoder".into(),
            kind: KIND_PLUGIN.into(),
            aliases: vec!["qoder-cn".into()],
            detect: vec![
                DetectRule::HomeDir {
                    rel: ".qoder".into(),
                    note: "Qoder 用户配置目录（国际版）".into(),
                },
                DetectRule::HomeDir {
                    rel: ".qoder-cn".into(),
                    note: "Qoder 用户配置目录（国内版）".into(),
                },
                DetectRule::Cli {
                    names: vec!["qoder".into(), "qodercli".into()],
                    note: "PATH 里的 qoder 命令".into(),
                },
            ],
            protocols: vec![
                proto(
                    "SKILL.md",
                    TRANSPORT_SKILL_MD,
                    SUPPORT_SUPPORTED,
                    "真实语料 .qoder-plugin 目录 8 份，均已入库",
                ),
                proto(
                    "plugin.json",
                    TRANSPORT_JSON_CONFIG,
                    SUPPORT_SUPPORTED,
                    ".qoder-plugin/plugin.json 已解析，含 displayName / descriptionZh 等扩展字段",
                ),
                proto(
                    "MCP (stdio)",
                    TRANSPORT_MCP_STDIO,
                    SUPPORT_PARTIAL,
                    "同 .mcp.json 路径已实现；Qoder 的 mcpServers 私有字段未逐项验证",
                ),
                proto(
                    "MCP (http)",
                    TRANSPORT_MCP_HTTP,
                    SUPPORT_PARTIAL,
                    "url 能读，未验证 Qoder 的鉴权头写法",
                ),
                proto(
                    "CLI",
                    TRANSPORT_CLI,
                    SUPPORT_PLANNED,
                    "Qoder 的 CLI 入口未在本机出现，中枢尚无其命令适配",
                ),
            ],
        },
        HostSpec {
            id: "minimax-code".into(),
            display_name: "MiniMax Code".into(),
            vendor: "MiniMax".into(),
            kind: KIND_CLI.into(),
            aliases: vec!["minimax".into()],
            detect: vec![
                DetectRule::HomeDir {
                    rel: ".minimax".into(),
                    note: "MiniMax 用户配置目录".into(),
                },
                DetectRule::Cli {
                    names: vec!["minimax".into(), "minimax-code".into()],
                    note: "PATH 里的 minimax 命令".into(),
                },
            ],
            protocols: vec![
                proto(
                    "SKILL.md",
                    TRANSPORT_SKILL_MD,
                    SUPPORT_PARTIAL,
                    "中枢能读 SKILL.md；但本机语料里没有 minimax 专属样本，故只标部分支持",
                ),
                proto(
                    "MCP (stdio)",
                    TRANSPORT_MCP_STDIO,
                    SUPPORT_PARTIAL,
                    "通用 .mcp.json 路径能读；MiniMax 的配置文件名未验证",
                ),
                proto(
                    "plugin.json",
                    TRANSPORT_JSON_CONFIG,
                    SUPPORT_PLANNED,
                    "未见到 MiniMax 的插件清单样本，中枢不猜它的格式",
                ),
                proto(
                    "CLI",
                    TRANSPORT_CLI,
                    SUPPORT_PLANNED,
                    "CLI 适配未实现",
                ),
            ],
        },
        HostSpec {
            id: "workbuddy".into(),
            display_name: "WorkBuddy".into(),
            vendor: "WorkBuddy".into(),
            kind: KIND_CLI.into(),
            aliases: vec!["workbuddy-ai".into()],
            detect: vec![
                DetectRule::HomeDir {
                    rel: ".workbuddy".into(),
                    note: "WorkBuddy 用户配置目录".into(),
                },
                DetectRule::HomeDir {
                    rel: ".workbuddy-ai".into(),
                    note: "WorkBuddy AI 配置目录".into(),
                },
                DetectRule::Cli {
                    names: vec!["workbuddy".into()],
                    note: "PATH 里的 workbuddy 命令".into(),
                },
            ],
            protocols: vec![
                proto(
                    "SKILL.md",
                    TRANSPORT_SKILL_MD,
                    SUPPORT_PARTIAL,
                    "中央解析器能读 SKILL.md；WorkBuddy 是否有私有前置字段未验证",
                ),
                proto(
                    "MCP (stdio)",
                    TRANSPORT_MCP_STDIO,
                    SUPPORT_PARTIAL,
                    "通用 .mcp.json 路径能读，未验证其配置文件位置",
                ),
                proto(
                    "plugin.json",
                    TRANSPORT_JSON_CONFIG,
                    SUPPORT_PLANNED,
                    "未见到插件清单样本",
                ),
                proto(
                    "CLI",
                    TRANSPORT_CLI,
                    SUPPORT_PLANNED,
                    "CLI 适配未实现",
                ),
            ],
        },
        HostSpec {
            id: "kimi-code".into(),
            display_name: "Kimi Code".into(),
            vendor: "Moonshot AI".into(),
            kind: KIND_CLI.into(),
            aliases: vec!["kimi".into()],
            detect: vec![
                DetectRule::HomeDir {
                    rel: ".kimi".into(),
                    note: "Kimi 用户配置目录".into(),
                },
                DetectRule::HomeDir {
                    rel: ".kimi-code".into(),
                    note: "Kimi Code 配置目录".into(),
                },
                DetectRule::Cli {
                    names: vec!["kimi".into(), "kimi-code".into()],
                    note: "PATH 里的 kimi 命令".into(),
                },
            ],
            protocols: vec![
                proto(
                    "SKILL.md",
                    TRANSPORT_SKILL_MD,
                    SUPPORT_SUPPORTED,
                    "真实语料 .kimi-plugin 目录 1 份（zion-nocode），SKILL.md 已入库",
                ),
                proto(
                    "plugin.json",
                    TRANSPORT_JSON_CONFIG,
                    SUPPORT_SUPPORTED,
                    ".kimi-plugin/plugin.json 已解析（author 为字符串写法）",
                ),
                proto(
                    "MCP (stdio)",
                    TRANSPORT_MCP_STDIO,
                    SUPPORT_PARTIAL,
                    "通用 .mcp.json 路径能读，无 kimi 专属样本",
                ),
                proto(
                    "CLI",
                    TRANSPORT_CLI,
                    SUPPORT_PLANNED,
                    "CLI 适配未实现",
                ),
            ],
        },
        HostSpec {
            id: "deepseek-harness".into(),
            display_name: "DeepSeek Harness".into(),
            vendor: "DeepSeek".into(),
            kind: KIND_HARNESS.into(),
            aliases: vec!["dsh".into(), "deepseek".into()],
            detect: vec![
                DetectRule::EnvDir {
                    var: "DSH_HOME".into(),
                    note: "DSH 运行时根目录（本进程就是从它启动的）".into(),
                },
                DetectRule::HomeDir {
                    rel: ".dsh".into(),
                    note: "DSH 用户配置目录".into(),
                },
                DetectRule::PluginDir {
                    root: shared_root,
                    prefix: "dsh-".into(),
                    note: "共享插件根下的 dsh-* 插件".into(),
                },
            ],
            protocols: vec![
                proto(
                    "SKILL.md",
                    TRANSPORT_SKILL_MD,
                    SUPPORT_SUPPORTED,
                    "本机 DSH_HOME/skills 与共享插件根下均有 SKILL.md 样本，已入库",
                ),
                proto(
                    "plugin.json",
                    TRANSPORT_JSON_CONFIG,
                    SUPPORT_SUPPORTED,
                    ".dsh 标记目录 2 份（dsh-cosplay 等），plugin.json 已解析",
                ),
                proto(
                    "MCP (stdio)",
                    TRANSPORT_MCP_STDIO,
                    SUPPORT_SUPPORTED,
                    ".mcp.json 已解析；既有插件用 stdio 本地 server",
                ),
                proto(
                    "MCP (http)",
                    TRANSPORT_MCP_HTTP,
                    SUPPORT_SUPPORTED,
                    "中枢自身就提供 HTTP 端点，宿主可反向调用",
                ),
                proto(
                    "CLI",
                    TRANSPORT_CLI,
                    SUPPORT_PARTIAL,
                    "DSH 的 dsh 命令未出现在 PATH；只用桌面端通道",
                ),
            ],
        },
        HostSpec {
            id: "cursor".into(),
            display_name: "Cursor".into(),
            vendor: "Anysphere".into(),
            kind: KIND_PLUGIN.into(),
            aliases: vec![],
            detect: vec![
                DetectRule::HomeDir {
                    rel: ".cursor".into(),
                    note: "Cursor 用户配置目录".into(),
                },
                DetectRule::Cli {
                    names: vec!["cursor".into(), "cursor-agent".into()],
                    note: "PATH 里的 cursor 命令".into(),
                },
            ],
            protocols: vec![
                proto(
                    "SKILL.md",
                    TRANSPORT_SKILL_MD,
                    SUPPORT_SUPPORTED,
                    "真实语料 .cursor-plugin 目录 2 份，已入库",
                ),
                proto(
                    "plugin.json",
                    TRANSPORT_JSON_CONFIG,
                    SUPPORT_SUPPORTED,
                    ".cursor-plugin/plugin.json 已解析",
                ),
                proto(
                    "MCP (stdio)",
                    TRANSPORT_MCP_STDIO,
                    SUPPORT_PARTIAL,
                    "通用 .mcp.json 路径能读，Cursor 的 .cursor/mcp.json 未单独适配",
                ),
                proto(
                    "CLI",
                    TRANSPORT_CLI,
                    SUPPORT_PLANNED,
                    "CLI 适配未实现",
                ),
            ],
        },
        HostSpec {
            id: "zcode".into(),
            display_name: "ZCode".into(),
            vendor: "ZCode".into(),
            kind: KIND_PLUGIN.into(),
            aliases: vec![],
            detect: vec![
                DetectRule::HomeDir {
                    rel: ".zcode".into(),
                    note: "ZCode 用户配置目录".into(),
                },
                DetectRule::Cli {
                    names: vec!["zcode".into()],
                    note: "PATH 里的 zcode 命令".into(),
                },
            ],
            protocols: vec![
                proto(
                    "SKILL.md",
                    TRANSPORT_SKILL_MD,
                    SUPPORT_SUPPORTED,
                    "真实语料 .zcode-plugin 目录 3 份，已入库",
                ),
                proto(
                    "plugin.json",
                    TRANSPORT_JSON_CONFIG,
                    SUPPORT_SUPPORTED,
                    ".zcode-plugin/plugin.json 已解析（含 descriptionZh / displayName 扩展字段）",
                ),
                proto(
                    "MCP (stdio)",
                    TRANSPORT_MCP_STDIO,
                    SUPPORT_PARTIAL,
                    "通用 .mcp.json 路径能读，无 zcode 专属样本",
                ),
                proto(
                    "CLI",
                    TRANSPORT_CLI,
                    SUPPORT_PLANNED,
                    "CLI 适配未实现",
                ),
            ],
        },
    ]
}

/// 内置清单的只读视图（供 asset_host_support 等需要协议宇宙的场景使用）。
pub fn builtin_hosts() -> Vec<HostSpec> {
    builtin_specs()
}
// ── 双存储：目录清单 ──────────────────────────────────────────────

/// 清单目录：<data_dir>/hosts
pub fn hosts_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(HOSTS_DIR_NAME)
}

/// 首次调用把内置清单写成 <data_dir>/hosts/<id>.json。
///
/// # 不覆盖是硬要求
///
/// 「已存在的文件不覆盖」——用户可能把某个宿主的 support 从 partial
/// 改成一厢情愿的 supported，或者自己加了 vendor 里没有的宿主。
/// 每次启动都重写一遍等于把这些改动静默抹掉，而用户从任何返回值里
/// 都看不出来（除了「我改的怎么又变回去了」）。
///
/// 所以判据精确到**每个文件**：只有 <id>.json 不存在才写。
/// 目录存在但少了某个文件（用户删了它）→ 照样补上。
pub fn seed_defaults(data_dir: &Path) -> Result<HostSeedReport, String> {
    let dir = hosts_dir(data_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("建清单目录 {} 失败: {e}", dir.display()))?;

    let mut rep = HostSeedReport {
        dir: dir.display().to_string(),
        ..Default::default()
    };
    for spec in builtin_specs() {
        let path = dir.join(format!("{}.{}", spec.id, HOST_FILE_EXT));
        if path.is_file() {
            rep.existing.push(spec.id.clone());
            continue;
        }
        let body = serde_json::to_string_pretty(&spec)
            .map_err(|e| format!("序列化宿主清单 {} 失败: {e}", spec.id))?;
        // 原子写：先写 .tmp 再 rename。中途失败不会留下半个 JSON，
        // 否则下次启动读它会报解析失败，而用户根本不知道是自己删的。
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body + "\n")
            .map_err(|e| format!("写清单 {} 失败: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| format!("落盘清单 {} 失败: {e}", path.display()))?;
        rep.created.push(spec.id.clone());
    }
    Ok(rep)
}

/// 读清单目录里的全部宿主（**含用户自己加的**）。
///
/// 目录不存在不是错误：seed_defaults 会先建好它；直接调用本函数
/// （比如只读探针）时返回空表，而不是造一个假宿主。
pub fn load_specs(data_dir: &Path) -> Result<(Vec<HostSpec>, Vec<HostReject>), String> {
    let dir = hosts_dir(data_dir);
    let mut rejects: Vec<HostReject> = Vec::new();
    if !dir.is_dir() {
        return Ok((Vec::new(), rejects));
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("读清单目录 {} 失败: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .map(|x| x == HOST_FILE_EXT)
                    .unwrap_or(false)
        })
        .collect();
    // 目录遍历顺序不稳定；排序让「同一目录两次加载产出同一份结论」。
    files.sort();

    let mut out: Vec<HostSpec> = Vec::new();
    for f in files {
        let text = match std::fs::read_to_string(&f) {
            Ok(t) => t,
            Err(e) => {
                rejects.push(HostReject {
                    file: f.display().to_string(),
                    reason: format!("读取失败: {e}"),
                });
                continue;
            }
        };
        match serde_json::from_str::<HostSpec>(&text) {
            Ok(spec) => {
                let errs = spec.validate();
                if !errs.is_empty() {
                    rejects.push(HostReject {
                        file: f.display().to_string(),
                        reason: format!("清单不合法: {}", errs.join("; ")),
                    });
                    continue;
                }
                // 同名 id 以先读到的为准（文件名排序），并记一条重复提示。
                if out.iter().any(|x| x.id == spec.id) {
                    rejects.push(HostReject {
                        file: f.display().to_string(),
                        reason: format!("重复的宿主 id {:?}（已忽略这一份）", spec.id),
                    });
                    continue;
                }
                out.push(spec);
            }
            Err(e) => rejects.push(HostReject {
                file: f.display().to_string(),
                reason: format!("JSON 解析失败: {e}"),
            }),
        }
    }
    Ok((out, rejects))
}

// ── 资产 ↔ 宿主支持关系 ──────────────────────────────────────────

/// 一个资产的宿主支持判定结果。
///
/// **必须可序列化**：它直接作为 Tauri 命令 `asset_host_support_detail` 的
/// 返回类型跨 IPC 传到前端。少了 Serialize，`generate_handler!` 会在宏展开
/// 期报一个看不出所以然的 E0599（`blocking_kind` / `IpcResponse` 不满足）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSupport {
    /// 宿主 id 列表（字母序）。
    pub hosts: Vec<String>,
    /// 判定依据（每条一行，前端可直接展示）。
    pub basis: Vec<String>,
}

/// 资产格式 → 该格式需要的传输方式。
///
/// plugin_json 归到 json-config：插件清单本质就是一份 JSON 配置。
pub fn transport_of_format(source_format: &str) -> Option<&'static str> {
    match source_format {
        "skill_md" => Some(TRANSPORT_SKILL_MD),
        "plugin_json" => Some(TRANSPORT_JSON_CONFIG),
        "mcp_json" => Some(TRANSPORT_MCP_STDIO),
        _ => None,
    }
}

/// 取资产生效的格式：库里 source_format 为空时按 kind 兜底。
///
/// # 为什么可以这么兜底（而不算"猜"）
///
/// kind 本身是**解析器按文件名/格式判出来的**结论（SKILL.md → skill、
/// plugin.json → plugin、.mcp.json → mcp），三种 kind 与三种格式是
/// 一一对应的。所以从 kind 反推格式不是推断，是同一个事实的另一种写法。
///
/// 反过来，对这种行返回空表会让界面显示"没有宿主支持它"——一个假结论。
pub fn effective_format(kind: &str, source_format: &str) -> String {
    let f = source_format.trim();
    if !f.is_empty() {
        return f.to_string();
    }
    match kind {
        "skill" => "skill_md".to_string(),
        "plugin" => "plugin_json".to_string(),
        "mcp" => "mcp_json".to_string(),
        _ => String::new(),
    }
}

/// 从路径里认出宿主标记目录（.claude-plugin / .qoder-plugin / .codex-plugin）。
///
/// 与 agents::infer_ecosystem 的①同源，但这里取**全部**命中的标记，
/// 不是第一个：一份资产可以同时带 .qoder-plugin 与 .zcode-plugin
/// （真实语料里 obsidian-suite / knowledge-process-suite 就是这样），
/// 那两个宿主都该出现在支持列表里。
pub fn markers_of_path(source_path: &str) -> Vec<String> {
    let norm = source_path.replace('\\', "/");
    let mut out: Vec<String> = Vec::new();
    for seg in norm.split('/').filter(|s| !s.is_empty()) {
        let Some(inner) = seg.strip_prefix('.') else {
            continue;
        };
        // **只认以 -plugin 结尾的标记目录**（.qoder-plugin / .claude-plugin）。
        //
        // 为什么不把 .codex / .git / .venv 这类也算标记：路径上到处
        // 都是点目录，.git 会推出一个叫 "git" 的假宿主。真实语料里插件
        // 自声明宿主的写法**只有** .<host>-plugin 这一种，所以保持同一条口径。
        let Some(token) = inner.strip_suffix("-plugin") else {
            continue;
        };
        if token.is_empty() || token.contains('.') {
            continue;
        }
        let t = token.to_ascii_lowercase();
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// 全部内置宿主的生态键 → 宿主 id 的索引。
fn alias_index() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for spec in builtin_specs() {
        for k in spec.ecosystem_keys() {
            m.entry(k).or_insert_with(|| spec.id.clone());
        }
    }
    m
}

/// 协议匹配：该格式的资产**能**被哪些宿主使用（只看协议档位）。
fn protocol_capable(source_format: &str) -> Vec<String> {
    let Some(want) = transport_of_format(source_format) else {
        return Vec::new();
    };
    let mut out: Vec<String> = builtin_specs()
        .into_iter()
        .filter(|s| match want {
            // MCP 资产：宿主支持 stdio 或 http 任意一种就算能用。
            TRANSPORT_MCP_STDIO => {
                s.supports_transport(TRANSPORT_MCP_STDIO)
                    || s.supports_transport(TRANSPORT_MCP_HTTP)
            }
            other => s.supports_transport(other),
        })
        .map(|s| s.id)
        .collect();
    out.sort();
    out
}

/// 核心判定：给定库里的三列（origin_agent / source_path / source_format），
/// 算出「这个资产被哪些宿主支持」+ 依据。
///
/// # 为什么是纯函数而不查库
///
/// query_assets 每渲染一行都要这个结果。查库就是 N+1 次查询，而且
/// 判据会和 Registry::asset_host_support 分叉（两处各写一遍）。
/// 这里做成纯函数，两处共用同一份实现。
///
/// # 判据（每一步都有证据）
///
/// 1. **协议匹配**：决定候选宇宙（skill_md → 支持 skill-md 的宿主；
///    mcp_json → 支持 MCP 的宿主）。
/// 2. **origin_agent**：已判出的宿主生态 → 映射到宿主 id（claude → claude-code）。
/// 3. **路径标记目录**：.qoder-plugin / .zcode-plugin → 对应宿主。
///
/// 2、3 是**强证据**，有就把结果收窄到它们（再与协议宇宙求交）；
/// 都没有才退到 1 的全集。顺序的理由：路径标记是插件作者自己写的，
/// 比「这份 SKILL.md 长得像所有宿主都能用」具体得多。
pub fn support_from_columns(
    origin_agent: &str,
    source_path: &str,
    source_format: &str,
) -> HostSupport {
    let mut basis: Vec<String> = Vec::new();
    let Some(transport) = transport_of_format(source_format) else {
        basis.push(format!(
            "格式 {source_format:?} 没有对应的宿主协议：无法判定支持关系（不是「谁都不支持」）"
        ));
        return HostSupport {
            hosts: Vec::new(),
            basis,
        };
    };

    let universe = protocol_capable(source_format);
    if universe.is_empty() {
        basis.push(format!("没有任何内置宿主登记了 {transport} 协议"));
        return HostSupport {
            hosts: Vec::new(),
            basis,
        };
    }

    let idx = alias_index();
    let mut strong: Vec<String> = Vec::new();

    let eco = origin_agent.trim().to_ascii_lowercase();
    if !eco.is_empty() {
        match idx.get(&eco) {
            Some(host) => {
                basis.push(format!(
                    "origin_agent={eco:?} 推断出的宿主 → {host}（来自 agents::infer_ecosystem）"
                ));
                if !strong.contains(host) {
                    strong.push(host.clone());
                }
            }
            None => basis.push(format!(
                "origin_agent={eco:?} 不是任何一个内置宿主的生态键：不据此认宿主"
            )),
        }
    }

    for m in markers_of_path(source_path) {
        match idx.get(&m) {
            Some(host) => {
                basis.push(format!(
                    "路径标记目录 .{m}-plugin 命中宿主 {host}（插件自声明的宿主）"
                ));
                if !strong.contains(host) {
                    strong.push(host.clone());
                }
            }
            None => basis.push(format!(
                "路径标记目录 .{m}-plugin 不对应任何内置宿主：不据此认宿主"
            )),
        }
    }

    let hosts = if strong.is_empty() {
        basis.push(format!(
            "没有 origin_agent / 标记目录证据，按协议 {transport} 判：{} 个宿主全部可用",
            universe.len()
        ));
        universe
    } else {
        let mut filtered: Vec<String> = strong
            .into_iter()
            .filter(|h| universe.contains(h))
            .collect();
        filtered.sort();
        filtered.dedup();
        if filtered.is_empty() {
            // 证据指向的宿主与协议档位矛盾（比如 origin=claude 但格式是
            // claude 没登记的协议）。**如实报矛盾**，不要静默退回全集——
            // 退回全集会让界面显示一个看似合理但实际无据的列表。
            basis.push(format!(
                "证据指向的宿主都不支持 {transport} 协议：不返回宿主，等清单补齐后再判"
            ));
            Vec::new()
        } else {
            filtered
        }
    };

    HostSupport { hosts, basis }
}

// ── Registry 上的注册表操作 ──────────────────────────────────────

impl crate::store::Registry {
    /// 把清单目录 → 探测 → 数据库的整条链跑一遍。
    ///
    /// # 顺序是有意义的
    ///
    /// 1. seed_defaults：首次运行写内置清单（不覆盖已有文件）；
    /// 2. load_specs：读**全部**清单（含用户自己加的）；
    /// 3. 逐个探测 detected / connected；
    /// 4. 写 agent_hosts（整行覆盖——它是投影，不是用户状态）。
    ///
    /// # 为什么这里是"整行覆盖"而资产不是
    ///
    /// agent_hosts 的每一列都能从磁盘 + 探测重算出来，覆盖是安全的。
    /// assets.lifecycle 是用户点的，不能覆盖——两条写入路径的纪律不同。
    ///
    /// # connected 的判定口径
    ///
    /// 只认 agents.transport 里的 heartbeat / mcp 通道，**static 不算**。
    pub fn sync_hosts(
        &self,
        data_dir: &Path,
        env: &dyn ProbeEnv,
    ) -> Result<HostSyncReport, String> {
        let seed = seed_defaults(data_dir)?;
        let (specs, rejected) = load_specs(data_dir)?;
        let now = crate::agents::now_unix();

        // 一次读出 agents 表里的实报通道，避免每个宿主查一次。
        //
        // 四列都要：agent_id 与 ecosystem 是**两种不同的身份键**。
        // A 通道（mcp 注册）会写 ecosystem="dsh"；B 通道（心跳）协议里没有
        // ecosystem，只留下 agent_id="deepseek-harness"。只按 ecosystem 匹配
        // 会让「插件明明在发心跳、agent_id 完全对得上」的宿主 connected=false。
        let live: Vec<LiveAgent> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT COALESCE(agent_id, ''), LOWER(COALESCE(ecosystem, '')),
                            COALESCE(transport, ''), last_seen
                     FROM agents",
                )
                .map_err(|e| e.to_string())?;
            let it = stmt
                .query_map([], |r| {
                    Ok(LiveAgent {
                        agent_id: r.get::<_, String>(0)?,
                        ecosystem: r.get::<_, String>(1)?,
                        transport: r.get::<_, String>(2)?,
                        last_seen: r.get::<_, i64>(3)?,
                    })
                })
                .map_err(|e| e.to_string())?;
            it.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };

        let mut rep = HostSyncReport {
            seed,
            specs_loaded: specs.len() as u32,
            rejected,
            ..Default::default()
        };
        let mut seen_ids: Vec<String> = Vec::new();

        for spec in &specs {
            let (detected, evidence) = detect(spec, env);
            let (connected, last_seen, conn_note) = connected_of(spec, &live, now);
            let mut host = AgentHost::from_spec(spec);
            host.detected = detected;
            host.connected = connected;
            host.last_seen = last_seen;
            host.evidence = evidence;
            if let Some(note) = conn_note {
                host.evidence.push(note);
            }
            self.upsert_host_row(&host, now)?;
            seen_ids.push(host.id.clone());
            rep.rows_written += 1;
            if host.detected {
                rep.detected += 1;
            }
            if host.connected {
                rep.connected += 1;
            }
        }

        // 库里多出来的行**不删**（增量不删），只如实报。
        for id in self.list_host_ids()? {
            if !seen_ids.contains(&id) {
                rep.orphans.push(id);
            }
        }
        Ok(rep)
    }

    /// 写一行 agent_hosts。整行覆盖：它是可重算的投影。
    fn upsert_host_row(&self, h: &AgentHost, now: i64) -> Result<(), String> {
        let protocols = serde_json::to_string(&h.protocols).unwrap_or_else(|_| "[]".into());
        let evidence = serde_json::to_string(&h.evidence).unwrap_or_else(|_| "[]".into());
        self.conn
            .execute(
                "INSERT INTO agent_hosts
                     (id, display_name, vendor, kind, detected, connected,
                      last_seen, protocols_json, evidence_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(id) DO UPDATE SET
                     display_name   = excluded.display_name,
                     vendor         = excluded.vendor,
                     kind           = excluded.kind,
                     detected       = excluded.detected,
                     connected      = excluded.connected,
                     last_seen      = excluded.last_seen,
                     protocols_json = excluded.protocols_json,
                     evidence_json  = excluded.evidence_json,
                     updated_at     = excluded.updated_at",
                rusqlite::params![
                    h.id,
                    h.display_name,
                    h.vendor,
                    h.kind,
                    h.detected as i64,
                    h.connected as i64,
                    h.last_seen,
                    protocols,
                    evidence,
                    now
                ],
            )
            .map_err(|e| format!("写 agent_hosts 失败（{}）: {e}", h.id))?;
        Ok(())
    }

    /// 库里的全部宿主，按「接着的 → 检测到的 → id」排序。
    ///
    /// 排序是有意的：界面上用户最先要看的是**正在用中枢**的宿主，
    /// 其次是装了但没接的，最后是登记了但本机没有的。
    pub fn list_agent_hosts(&self) -> Result<Vec<AgentHost>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, display_name, vendor, kind, detected, connected, last_seen,
                        protocols_json, evidence_json
                 FROM agent_hosts
                 ORDER BY connected DESC, detected DESC, id",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map([], |r| {
                let protocols_json: String = r.get(7)?;
                let evidence_json: String = r.get(8)?;
                Ok(AgentHost {
                    id: r.get(0)?,
                    display_name: r.get(1)?,
                    vendor: r.get(2)?,
                    kind: r.get(3)?,
                    detected: r.get::<_, i64>(4)? != 0,
                    connected: r.get::<_, i64>(5)? != 0,
                    last_seen: r.get(6)?,
                    protocols: serde_json::from_str(&protocols_json).unwrap_or_default(),
                    evidence: serde_json::from_str(&evidence_json).unwrap_or_default(),
                })
            })
            .map_err(|e| e.to_string())?;
        it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// 单个宿主。不存在返回 None。
    pub fn get_agent_host(&self, id: &str) -> Result<Option<AgentHost>, String> {
        Ok(self
            .list_agent_hosts()?
            .into_iter()
            .find(|h| h.id.eq_ignore_ascii_case(id)))
    }

    fn list_host_ids(&self) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM agent_hosts ORDER BY id")
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// 注册表总览。所有数字都来自 SQL / 本次探测，没有默认填充。
    pub fn hosts_overview(&self, data_dir: &Path) -> Result<HostsOverview, String> {
        let hosts = self.list_agent_hosts()?;
        let mut ov = HostsOverview {
            total: hosts.len() as u32,
            hosts_dir: hosts_dir(data_dir).display().to_string(),
            ..Default::default()
        };
        let mut sup: BTreeMap<String, u32> = BTreeMap::new();
        let mut kinds: BTreeMap<String, u32> = BTreeMap::new();
        for h in &hosts {
            if h.detected {
                ov.detected += 1;
            }
            if h.connected {
                ov.connected += 1;
            }
            ov.protocol_rows += h.protocols.len() as u32;
            for p in &h.protocols {
                *sup.entry(p.support.clone()).or_insert(0) += 1;
            }
            *kinds.entry(h.kind.clone()).or_insert(0) += 1;
        }
        ov.by_support = sup
            .into_iter()
            .map(|(key, count)| HostCount { key, count })
            .collect();
        ov.by_kind = kinds
            .into_iter()
            .map(|(key, count)| HostCount { key, count })
            .collect();
        Ok(ov)
    }

    /// 哪些宿主智能体**能**用这个资产。
    ///
    /// 返回空表有两种含义，调用方必须分清：资产不存在（None）
    /// 与 判不出来（Some(vec![])）。
    pub fn asset_host_support(&self, asset_id: &str) -> Result<Option<Vec<String>>, String> {
        Ok(self.asset_host_support_detail(asset_id)?.map(|s| s.hosts))
    }

    /// 同上，但把**依据**一起返回（资产详情面板要能回答"凭什么"）。
    pub fn asset_host_support_detail(&self, asset_id: &str) -> Result<Option<HostSupport>, String> {
        let row: Option<(String, Option<String>, Option<String>, String)> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT COALESCE(origin_agent, ''), source_path, source_format, kind
                     FROM assets WHERE id = ?1",
                )
                .map_err(|e| e.to_string())?;
            let it = stmt
                .query_map(rusqlite::params![asset_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            it.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
                .into_iter()
                .next()
        };
        let Some((origin_agent, source_path, source_format, kind)) = row else {
            return Ok(None);
        };
        let format = effective_format(&kind, source_format.as_deref().unwrap_or(""));
        Ok(Some(support_from_columns(
            &origin_agent,
            source_path.as_deref().unwrap_or(""),
            &format,
        )))
    }
}

/// 判定 connected：只认 agents 表里 **heartbeat / mcp** 通道的实报。
///
/// 返回 (是否连着, last_seen, 说明)。说明会拼进 evidence，
/// 让用户在界面上看到"为什么这个宿主是绿的 / 为什么不是"。
/// agents 表里的一行「实报通道」，供 connected 判定使用。
#[derive(Debug, Clone)]
struct LiveAgent {
    agent_id: String,
    ecosystem: String,
    transport: String,
    last_seen: i64,
}

/// 判断这一行 agents 记录是否属于 spec 这个宿主。
///
/// **三个键任一命中就算**：
/// 1. agent_id 等于宿主 id（心跳行的典型形态：`deepseek-harness`）
/// 2. agent_id 等于宿主某个别名（`dsh`）
/// 3. ecosystem 等于宿主某个生态键（A 通道注册声明了 `dsh`）
///
/// 为什么必须三个都认：A 通道与 B 通道**携带的身份信息不一样**。
/// 心跳协议里没有 ecosystem 字段（那是中枢侧知识，不该让心跳文件背），
/// 所以心跳行只能靠 agent_id 认。只按 ecosystem 匹配时，
/// 「插件在发心跳、agent_id 完全对得上」的宿主 connected 永远是 false
/// —— 实测踩过，界面上就是「在线智能体说有 1 个在线，协议矩阵说未连接」。
fn row_belongs_to(spec: &HostSpec, row: &LiveAgent) -> bool {
    let keys = spec.ecosystem_keys();
    let id = row.agent_id.trim().to_ascii_lowercase();
    let eco = row.ecosystem.trim().to_ascii_lowercase();
    if !id.is_empty() && (id == spec.id.to_ascii_lowercase() || keys.contains(&id)) {
        return true;
    }
    !eco.is_empty() && keys.contains(&eco)
}

fn connected_of(
    spec: &HostSpec,
    live: &[LiveAgent],
    now: i64,
) -> (bool, i64, Option<String>) {
    let keys = spec.ecosystem_keys();
    let mut best_seen: i64 = 0;
    let mut channels: Vec<String> = Vec::new();
    let mut matched: Vec<String> = Vec::new();
    for row in live {
        if !row_belongs_to(spec, row) {
            continue;
        }
        // static 通道：磁盘推断，不是实报。**不算 connected**，
        // 但它的 last_seen 也不该被当成"宿主在跑"，所以完全跳过。
        if row.transport != crate::agents::TRANSPORT_HEARTBEAT
            && row.transport != crate::agents::TRANSPORT_MCP
        {
            continue;
        }
        if row.last_seen > best_seen {
            best_seen = row.last_seen;
        }
        let label = format!("{}:{}", row.transport, row.agent_id);
        if !channels.contains(&label) {
            channels.push(label);
        }
        if !matched.contains(&row.agent_id) {
            matched.push(row.agent_id.clone());
        }
    }
    if best_seen == 0 && channels.is_empty() {
        return (
            false,
            0,
            Some(format!(
                "未连接：agents 表里没有 {} 的 heartbeat / mcp 通道记录（static 推断不算连接）",
                keys.join(" / ")
            )),
        );
    }
    // 端点注册的宿主没有心跳窗口，用"上次见到"作为到期判据。
    // 超过 STALE_WINDOW 没有实报，就不再算 connected。
    let age_secs = (now - best_seen).max(0);
    let stale = age_secs * 1000 > crate::agents::STALE_WINDOW_MS;
    if stale {
        return (
            false,
            best_seen,
            Some(format!(
                "未连接：最近一次实报在 {age_secs} 秒前，超过 stale 窗口（{} 秒）",
                crate::agents::STALE_WINDOW_MS / 1000
            )),
        );
    }
    (
        true,
        best_seen,
        Some(format!(
            "已连接：来自 {} 的实报，最近一次 {} 秒前",
            channels.join(" / "),
            age_secs
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;
    use std::collections::BTreeSet;

    // ── 假机器：测试结果不随开发机安装情况漂移 ──────────────────

    /// 一台「造假机器」：只有配置里声明的路径/命令存在。
    struct FakeEnv {
        home: PathBuf,
        exist: BTreeSet<PathBuf>,
        vars: BTreeMap<String, String>,
        clis: BTreeMap<String, PathBuf>,
        dirs: BTreeMap<PathBuf, Vec<String>>,
    }

    impl FakeEnv {
        fn new() -> Self {
            Self {
                home: PathBuf::from("C:/fake-home"),
                exist: BTreeSet::new(),
                vars: BTreeMap::new(),
                clis: BTreeMap::new(),
                dirs: BTreeMap::new(),
            }
        }

        fn with_dir(mut self, rel: &str) -> Self {
            self.exist.insert(self.home.join(rel));
            self
        }

        fn with_abs(mut self, p: &str) -> Self {
            self.exist.insert(PathBuf::from(p));
            self
        }

        fn with_var(mut self, k: &str, v: &str) -> Self {
            self.vars.insert(k.to_string(), v.to_string());
            self
        }

        fn with_cli(mut self, name: &str, p: &str) -> Self {
            self.clis.insert(name.to_string(), PathBuf::from(p));
            self
        }

        /// 造一个「目录下有这么几个子目录」的假插件根。
        fn with_dirs(mut self, root: &str, names: &[&str]) -> Self {
            self.dirs.insert(
                PathBuf::from(root),
                names.iter().map(|s| s.to_string()).collect(),
            );
            self
        }
    }

    impl ProbeEnv for FakeEnv {
        fn home(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
        fn var(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }
        fn exists(&self, path: &Path) -> bool {
            self.exist.contains(path)
        }
        fn which(&self, cmd: &str) -> Option<PathBuf> {
            self.clis.get(cmd).cloned()
        }
        fn list_dirs(&self, path: &Path) -> Vec<String> {
            self.dirs.get(path).cloned().unwrap_or_default()
        }
    }

    // ── 契约值 ──────────────────────────────────────────────────

    #[test]
    fn support_values_are_exactly_three_lowercase_and_transports_are_hyphenated() {
        // 前端按这些字符串分档染色。大写或下划线不会报错，只会让那一格
        // 变成默认色——静默的错误比崩溃更难查，所以钉死。
        assert_eq!(SUPPORT_SUPPORTED, "supported");
        assert_eq!(SUPPORT_PARTIAL, "partial");
        assert_eq!(SUPPORT_PLANNED, "planned");
        assert_eq!(
            ALL_TRANSPORTS,
            &["mcp-stdio", "mcp-http", "cli", "json-config", "skill-md"]
        );
        for t in ALL_TRANSPORTS {
            assert!(!t.is_empty());
            assert!(
                t.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "transport {t:?} 必须是小写连字符形式"
            );
        }
        for k in ALL_KINDS {
            assert!(
                k.chars().all(|c| c.is_ascii_lowercase()),
                "kind {k:?} 必须小写"
            );
        }
    }

    #[test]
    fn host_protocol_validate_rejects_wrong_case_and_underscore() {
        let ok = HostProtocol::new("x", TRANSPORT_MCP_STDIO, SUPPORT_SUPPORTED, "");
        assert!(ok.validate().is_none());
        let bad_support = HostProtocol::new("x", TRANSPORT_MCP_STDIO, "Supported", "");
        assert!(bad_support.validate().is_some(), "大写 support 必须被拒");
        let bad_transport = HostProtocol::new("x", "mcp_stdio", SUPPORT_SUPPORTED, "");
        assert!(bad_transport.validate().is_some(), "下划线 transport 必须被拒");
    }

    #[test]
    fn agent_host_serializes_with_snake_case_field_names() {
        // Tauri invoke 走纯 serde，不做键名转换。前端 panel-types.ts 的
        // AgentHost 按 display_name / last_seen / protocols / evidence 消费。
        let h = AgentHost {
            id: "claude-code".into(),
            display_name: "Claude Code".into(),
            vendor: "Anthropic".into(),
            kind: KIND_CLI.into(),
            detected: true,
            connected: false,
            last_seen: 1700000000,
            protocols: vec![HostProtocol::new(
                "SKILL.md",
                TRANSPORT_SKILL_MD,
                SUPPORT_SUPPORTED,
                "n",
            )],
            evidence: vec!["命中：x".into()],
        };
        let v = serde_json::to_value(&h).unwrap();
        let obj = v.as_object().unwrap();
        for k in [
            "id",
            "display_name",
            "vendor",
            "kind",
            "detected",
            "connected",
            "last_seen",
            "protocols",
            "evidence",
        ] {
            assert!(obj.contains_key(k), "缺字段 {k}（前端会读成 undefined）");
        }
        assert!(!obj.contains_key("displayName"), "不得输出 camelCase");
        assert!(!obj.contains_key("lastSeen"));
        let p = v["protocols"][0].as_object().unwrap();
        for k in ["name", "transport", "support", "note"] {
            assert!(p.contains_key(k), "协议缺字段 {k}");
        }
        assert_eq!(v["protocols"][0]["support"], "supported");
        assert_eq!(v["protocols"][0]["transport"], "skill-md");
    }

    // ── 内置清单 ────────────────────────────────────────────────

    #[test]
    fn builtin_covers_every_host_the_user_named() {
        // 用户点名 7 个，少一个就是需求没做完。
        let ids: Vec<String> = builtin_specs().into_iter().map(|s| s.id).collect();
        for want in [
            "claude-code",
            "codex",
            "qoder",
            "minimax-code",
            "workbuddy",
            "kimi-code",
            "deepseek-harness",
        ] {
            assert!(
                ids.contains(&want.to_string()),
                "内置清单缺宿主 {want}: {ids:?}"
            );
        }
    }

    #[test]
    fn builtin_specs_are_all_valid_and_ids_are_unique() {
        let specs = builtin_specs();
        let mut seen: Vec<String> = Vec::new();
        for s in &specs {
            let errs = s.validate();
            assert!(errs.is_empty(), "宿主 {} 不合法: {errs:?}", s.id);
            assert!(!seen.contains(&s.id), "宿主 id 重复: {}", s.id);
            seen.push(s.id.clone());
            assert!(!s.display_name.trim().is_empty(), "{} 缺 display_name", s.id);
            assert!(!s.vendor.trim().is_empty(), "{} 缺 vendor", s.id);
            // 每条协议都必须有依据。没有依据的支持状态等于装饰。
            for p in &s.protocols {
                assert!(
                    !p.note.trim().is_empty(),
                    "宿主 {} 的协议 {} 没写依据",
                    s.id,
                    p.name
                );
            }
            // 没有探测规则的宿主在界面上永远显示"未检测到"，
            // 而那不是事实（是没查）。所以每个内置宿主都要有规则。
            assert!(!s.detect.is_empty(), "宿主 {} 没有探测规则", s.id);
        }
        assert_eq!(specs.len(), 9, "内置清单规模变了要同步更新本条");
    }

    #[test]
    fn every_builtin_host_has_at_least_one_readable_protocol() {
        // 判据是「至少有一条我们能读（supported 或 partial）」。不要求必须
        // supported：MiniMax / WorkBuddy 这类宿主在本机没有专属样本，
        // 强行标 supported 就是拿"看起来支持"当"实测支持"。
        for s in builtin_specs() {
            assert!(
                s.protocols.iter().any(|p| p.support != SUPPORT_PLANNED),
                "宿主 {} 全是 planned：等于「我们还不认识它」，列进清单没有意义",
                s.id
            );
        }
    }

    #[test]
    fn ecosystem_keys_include_id_and_aliases_lowercased() {
        let c = builtin_specs()
            .into_iter()
            .find(|s| s.id == "claude-code")
            .unwrap();
        let keys = c.ecosystem_keys();
        assert!(keys.contains(&"claude-code".to_string()));
        assert!(
            keys.contains(&"claude".to_string()),
            "claude 是 .claude-plugin 推出的生态键，必须能映射到 claude-code"
        );
    }

    // ── 探测：有证据才认 ────────────────────────────────────────

    fn spec_of(id: &str) -> HostSpec {
        builtin_specs().into_iter().find(|s| s.id == id).unwrap()
    }

    #[test]
    fn detected_is_true_only_when_a_rule_hits() {
        let claude = spec_of("claude-code");
        let env = FakeEnv::new().with_dir(".claude");
        let (hit, ev) = detect(&claude, &env);
        assert!(hit, "命中 ~/.claude 就该判 detected");
        assert!(ev.iter().any(|e| e.starts_with("命中")), "证据里要有命中行");
        assert!(
            ev.iter().any(|e| e.contains("已安装")),
            "末行要给总判定: {ev:?}"
        );
    }

    #[test]
    fn not_detected_still_writes_where_we_looked() {
        // 这是本模块最重要的一条：detected=false 时证据**不能是空表**。
        // 否则用户没法区分「真没装」和「这个版本没加规则」。
        let claude = spec_of("claude-code");
        let env = FakeEnv::new();
        let (hit, ev) = detect(&claude, &env);
        assert!(!hit);
        assert!(
            ev.len() > claude.detect.len(),
            "每条规则 + 末行判定都要出现: {ev:?}"
        );
        assert!(ev.iter().filter(|e| e.starts_with("未命中")).count() == claude.detect.len());
        assert!(
            ev.last().unwrap().contains("未检测到"),
            "末行必须明说未检测到: {ev:?}"
        );
        assert!(
            ev.last().unwrap().contains("不猜"),
            "末行必须说明「查不到不等于没装、本表不猜」"
        );
    }

    #[test]
    fn cli_rule_hits_via_path_lookup() {
        let claude = spec_of("claude-code");
        let env = FakeEnv::new().with_cli("claude", "C:/fake/bin/claude.exe");
        let (hit, ev) = detect(&claude, &env);
        assert!(hit);
        assert!(
            ev.iter().any(|e| e.contains("claude.exe")),
            "证据要写出命中的绝对路径: {ev:?}"
        );
    }

    #[test]
    fn env_dir_rule_uses_environment_variable() {
        let dsh = spec_of("deepseek-harness");
        let env = FakeEnv::new()
            .with_var("DSH_HOME", "D:/dsh-home")
            .with_abs("D:/dsh-home");
        let (hit, ev) = detect(&dsh, &env);
        assert!(hit);
        assert!(
            ev.iter().any(|e| e.contains("DSH_HOME = D:/dsh-home")),
            "{ev:?}"
        );
    }

    #[test]
    fn env_dir_rule_reports_unset_variable_instead_of_guessing() {
        let dsh = spec_of("deepseek-harness");
        let env = FakeEnv::new();
        let (hit, ev) = detect(&dsh, &env);
        assert!(!hit);
        assert!(ev.iter().any(|e| e.contains("DSH_HOME 未设置")), "{ev:?}");
    }

    #[test]
    fn plugin_dir_rule_lists_matching_subdirectories() {
        // 插件根探测是最关键的一条 dsh 证据。它必须走 ProbeEnv.list_dirs，
        // 否则测试里的「假机器」会漏出真实机器上装了什么。
        let spec = HostSpec {
            id: "dsh-test".into(),
            display_name: "X".into(),
            vendor: "Y".into(),
            kind: KIND_HARNESS.into(),
            aliases: vec![],
            detect: vec![DetectRule::PluginDir {
                root: "C:/fake-plugin-root".into(),
                prefix: "dsh-".into(),
                note: "假插件根".into(),
            }],
            protocols: vec![],
        };
        let env = FakeEnv::new().with_dirs(
            "C:/fake-plugin-root",
            &["dsh-blender", "dsh-chatgpt", "other-plugin"],
        );
        let (hit, ev) = detect(&spec, &env);
        assert!(hit);
        assert!(ev.iter().any(|e| e.contains("2 个 dsh-*")), "{ev:?}");

        // 没有匹配项时如实说没有，不凭"目录存在"就判装了
        let env2 = FakeEnv::new().with_dirs("C:/fake-plugin-root", &["other-plugin"]);
        let (hit2, ev2) = detect(&spec, &env2);
        assert!(!hit2);
        assert!(ev2.iter().any(|e| e.contains("没有匹配的子目录")), "{ev2:?}");
    }

    #[test]
    fn spec_without_detect_rules_says_it_did_not_look() {
        // 没有规则 ≠ 没装。这句必须在证据里出现，否则用户会把
        // 「我们没配规则」读成「本机没有这个宿主」。
        let spec = HostSpec {
            id: "custom-host".into(),
            display_name: "X".into(),
            vendor: "Y".into(),
            kind: KIND_HARNESS.into(),
            aliases: vec![],
            detect: vec![],
            protocols: vec![],
        };
        let (hit, ev) = detect(&spec, &FakeEnv::new());
        assert!(!hit);
        assert!(ev[0].contains("未配置任何探测规则"), "{ev:?}");
        assert!(ev[0].contains("不是「没装」"), "{ev:?}");
    }

    // ── 双存储：目录清单 ────────────────────────────────────────

    #[test]
    fn seed_writes_one_json_per_builtin_host() {
        let s = Scratch::new("hosts-seed");
        let rep = seed_defaults(s.path()).unwrap();
        assert_eq!(rep.created.len(), builtin_specs().len());
        assert!(rep.existing.is_empty(), "首次运行不该有既有文件");
        for id in &rep.created {
            let p = hosts_dir(s.path()).join(format!("{id}.json"));
            assert!(p.is_file(), "清单文件没落盘: {}", p.display());
            let text = std::fs::read_to_string(&p).unwrap();
            let back: HostSpec = serde_json::from_str(&text).unwrap();
            assert_eq!(&back.id, id);
        }
        // 目录就是 <data_dir>/hosts
        assert_eq!(rep.dir, hosts_dir(s.path()).display().to_string());
        assert!(rep.dir.ends_with("hosts"));
    }

    #[test]
    fn seeded_manifest_roundtrips_utf8_notes() {
        // 清单里全是中文 note。写出去必须是合法 UTF-8 且原样读回
        // （历史事故：PowerShell 改写源码曾把中文变成乱码）。
        let s = Scratch::new("hosts-utf8");
        seed_defaults(s.path()).unwrap();
        let p = hosts_dir(s.path()).join("claude-code.json");
        let raw = std::fs::read(&p).unwrap();
        let text = String::from_utf8(raw).expect("清单必须是合法 UTF-8");
        let back: HostSpec = serde_json::from_str(&text).unwrap();
        assert!(
            back.protocols[0].note.contains("真实语料"),
            "中文 note 读回来变成了: {:?}",
            back.protocols[0].note
        );
        assert!(!text.contains('\u{fffd}'), "出现替换字符 = 编码损坏");
    }

    #[test]
    fn seed_does_not_overwrite_user_edits() {
        // 用户把某个宿主的 support 改了，第二次启动必须**原样保留**。
        // 覆盖它等于把用户的改动静默抹掉，而用户从返回值里看不出来。
        let s = Scratch::new("hosts-seed-keep");
        seed_defaults(s.path()).unwrap();
        let p = hosts_dir(s.path()).join("qoder.json");
        let mut spec: HostSpec =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        spec.protocols[0].support = SUPPORT_PLANNED.into();
        spec.display_name = "用户改过的名字".into();
        std::fs::write(&p, serde_json::to_string_pretty(&spec).unwrap()).unwrap();

        let rep2 = seed_defaults(s.path()).unwrap();
        assert!(rep2.created.is_empty(), "第二次不该写任何文件");
        assert_eq!(rep2.existing.len(), builtin_specs().len());
        let back: HostSpec =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back.display_name, "用户改过的名字");
        assert_eq!(back.protocols[0].support, SUPPORT_PLANNED);
    }

    #[test]
    fn seed_restores_a_deleted_file() {
        // 用户删了某个文件 → 下次启动补回来。这是显式删除后的重建，
        // 与「覆盖改动」是两件事。
        let s = Scratch::new("hosts-seed-restore");
        seed_defaults(s.path()).unwrap();
        std::fs::remove_file(hosts_dir(s.path()).join("codex.json")).unwrap();
        let rep = seed_defaults(s.path()).unwrap();
        assert_eq!(rep.created, vec!["codex".to_string()]);
        assert!(hosts_dir(s.path()).join("codex.json").is_file());
    }

    #[test]
    fn user_added_host_is_loaded_alongside_builtins() {
        let s = Scratch::new("hosts-user-add");
        seed_defaults(s.path()).unwrap();
        let custom = HostSpec {
            id: "my-host".into(),
            display_name: "我的宿主".into(),
            vendor: "Me".into(),
            kind: KIND_CLI.into(),
            aliases: vec![],
            detect: vec![DetectRule::Cli {
                names: vec!["myhost".into()],
                note: "自定义 CLI".into(),
            }],
            protocols: vec![HostProtocol::new(
                "CLI",
                TRANSPORT_CLI,
                SUPPORT_PARTIAL,
                "用户自己登记",
            )],
        };
        std::fs::write(
            hosts_dir(s.path()).join("my-host.json"),
            serde_json::to_string_pretty(&custom).unwrap(),
        )
        .unwrap();
        let (specs, rejects) = load_specs(s.path()).unwrap();
        assert!(rejects.is_empty(), "{rejects:?}");
        assert_eq!(specs.len(), builtin_specs().len() + 1);
        assert!(specs.iter().any(|x| x.id == "my-host"));
    }

    #[test]
    fn broken_manifest_file_is_reported_not_swallowed() {
        // 用户手改坏了 json，界面必须看得见，而不是"这个宿主凭空消失"。
        let s = Scratch::new("hosts-broken");
        seed_defaults(s.path()).unwrap();
        std::fs::write(hosts_dir(s.path()).join("bad.json"), "{ not json").unwrap();
        let (specs, rejects) = load_specs(s.path()).unwrap();
        assert_eq!(specs.len(), builtin_specs().len());
        assert_eq!(rejects.len(), 1);
        assert!(
            rejects[0].reason.contains("JSON 解析失败"),
            "{:?}",
            rejects[0]
        );
    }

    #[test]
    fn invalid_contract_value_in_manifest_is_rejected() {
        let s = Scratch::new("hosts-bad-support");
        seed_defaults(s.path()).unwrap();
        let p = hosts_dir(s.path()).join("qoder.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        v["protocols"][0]["support"] = serde_json::json!("Supported");
        std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        let (specs, rejects) = load_specs(s.path()).unwrap();
        assert_eq!(specs.len(), builtin_specs().len() - 1, "坏清单不该被载入");
        assert_eq!(rejects.len(), 1);
        assert!(
            rejects[0].reason.contains("不在契约值里"),
            "{:?}",
            rejects[0]
        );
    }

    #[test]
    fn missing_hosts_dir_loads_empty_not_error() {
        let s = Scratch::new("hosts-nodir");
        let (specs, rejects) = load_specs(s.path()).unwrap();
        assert!(specs.is_empty(), "目录不存在时应返回空表，而不是造一个假宿主");
        assert!(rejects.is_empty());
    }

    // ── 资产 ↔ 宿主支持 ────────────────────────────────────────

    #[test]
    fn markers_of_path_finds_every_plugin_marker() {
        let m = markers_of_path("D:/a/vendor/plugin/obsidian-suite/.qoder-plugin/plugin.json");
        assert_eq!(m, vec!["qoder".to_string()]);
        let m2 = markers_of_path("D:/a/vendor/skill/zion-nocode/2.7.5/.claude-plugin/plugin.json");
        assert_eq!(m2, vec!["claude".to_string()]);
        // 普通点文件不是标记目录
        assert!(markers_of_path("D:/a/.mcp.json").is_empty());
        assert!(markers_of_path("D:/a/.git/config").is_empty());
        assert!(markers_of_path("D:/a/.venv/lib/SKILL.md").is_empty());
    }

    #[test]
    fn skill_md_without_evidence_is_supported_by_every_skill_host() {
        // 判据 1 的兜底：没有 origin_agent / 标记目录时，按协议判全集。
        // 这是"能"而不是"正在用"——界面上应据此显示"可被这些宿主加载"。
        let s = support_from_columns("", "D:/vendor/skill/unknown/SKILL.md", "skill_md");
        let all_skill_hosts: Vec<String> = builtin_specs()
            .into_iter()
            .filter(|h| h.supports_transport(TRANSPORT_SKILL_MD))
            .map(|h| h.id)
            .collect();
        assert_eq!(s.hosts.len(), all_skill_hosts.len());
        assert!(s.basis.iter().any(|b| b.contains("全部可用")));
    }

    #[test]
    fn origin_agent_narrows_support_to_that_host() {
        let s = support_from_columns("qoder", "D:/vendor/skill/x/SKILL.md", "skill_md");
        assert_eq!(s.hosts, vec!["qoder".to_string()]);
        assert!(s.basis.iter().any(|b| b.contains("origin_agent=")));
    }

    #[test]
    fn claude_ecosystem_maps_to_claude_code_host_id() {
        // 库里 origin_agent 是 "claude"（.claude-plugin 推出），
        // 而宿主 id 是 "claude-code"。别名必须能接上，否则资产永远
        // 显示"没有宿主支持"。
        let s = support_from_columns(
            "claude",
            "D:/vendor/skill/x/.claude-plugin/plugin.json",
            "plugin_json",
        );
        assert_eq!(s.hosts, vec!["claude-code".to_string()]);
    }

    #[test]
    fn plugin_marker_wins_over_plain_format_default() {
        let s = support_from_columns("", "D:/x/.zcode-plugin/plugin.json", "plugin_json");
        assert_eq!(s.hosts, vec!["zcode".to_string()]);
    }

    #[test]
    fn conflicting_evidence_reports_empty_rather_than_dumping_the_universe() {
        let s = support_from_columns("claude", "D:/x/whatever.bin", "unknown_format");
        assert!(s.hosts.is_empty());
        assert!(s.basis.iter().any(|b| b.contains("无法判定支持关系")));
    }

    #[test]
    fn mcp_asset_requires_a_host_with_mcp_protocol() {
        let s = support_from_columns("", "D:/vendor/mcp/x/.mcp.json", "mcp_json");
        assert!(!s.hosts.is_empty(), "至少 claude-code 支持 mcp");
        assert!(s.hosts.contains(&"claude-code".to_string()));
        assert!(s.hosts.contains(&"qoder".to_string()));
    }

    #[test]
    fn unsupported_format_yields_empty_with_reason() {
        let s = support_from_columns("", "D:/x/a.bin", "some_future_format");
        assert!(s.hosts.is_empty());
        assert!(!s.basis.is_empty(), "判不出来也要给理由，不能空口返回空表");
    }

    #[test]
    fn effective_format_falls_back_to_kind() {
        // 老库的 source_format 可能是空。kind 是解析器按格式判出来的，
        // 反推格式不是猜——但空格式绝不能直接判"谁都不支持"。
        assert_eq!(effective_format("skill", ""), "skill_md");
        assert_eq!(effective_format("plugin", ""), "plugin_json");
        assert_eq!(effective_format("mcp", ""), "mcp_json");
        assert_eq!(effective_format("ghost", ""), "");
        // 有格式时原样返回，不被 kind 覆盖
        assert_eq!(effective_format("skill", "plugin_json"), "plugin_json");
    }

    // ── 数据库链路 ──────────────────────────────────────────────

    #[test]
    fn sync_hosts_writes_a_row_per_manifest_and_is_idempotent() {
        let s = Scratch::new("hosts-sync");
        let reg = crate::store::Registry::open_in_memory().unwrap();
        let env = FakeEnv::new()
            .with_dir(".codex")
            .with_var("DSH_HOME", "D:/dsh")
            .with_abs("D:/dsh");

        let rep = reg.sync_hosts(s.path(), &env).unwrap();
        assert_eq!(rep.specs_loaded as usize, builtin_specs().len());
        assert_eq!(rep.rows_written as usize, builtin_specs().len());
        assert!(rep.rejected.is_empty());
        // 假机器上只装了 codex（~/.codex）与 dsh（DSH_HOME）
        assert_eq!(rep.detected, 2, "只有 ~/.codex 与 DSH_HOME 两条证据");

        let rows = reg.list_agent_hosts().unwrap();
        assert_eq!(rows.len(), builtin_specs().len());
        let codex = rows.iter().find(|h| h.id == "codex").unwrap();
        assert!(codex.detected);
        assert!(!codex.connected, "静态证据不是连接");
        assert!(codex.evidence.iter().any(|e| e.starts_with("命中")));

        // 幂等：再跑一次行数不变、detected 不变
        let rep2 = reg.sync_hosts(s.path(), &env).unwrap();
        assert_eq!(rep2.rows_written, rep.rows_written);
        assert_eq!(rep2.detected, rep.detected);
        assert!(rep2.orphans.is_empty());
        assert_eq!(reg.list_agent_hosts().unwrap().len(), rows.len());
    }

    #[test]
    fn static_agents_channel_does_not_count_as_connected() {
        // 这是本模块最核心的一条纪律：磁盘路径推断 ≠ 宿主在跑。
        // 把 static 算成连接，界面上就会出现一个绿点，而宿主根本没启动。
        let s = Scratch::new("hosts-static");
        let reg = crate::store::Registry::open_in_memory().unwrap();
        let env = FakeEnv::new();
        reg.sync_hosts(s.path(), &env).unwrap();

        reg.conn
            .execute(
                "INSERT INTO agents (agent_id, display_name, ecosystem, kind, transport,
                                     endpoint, first_seen, last_seen, meta)
                 VALUES ('static:qoder','qoder','qoder','host','static','',1,9999999999,'{}')",
                [],
            )
            .unwrap();
        let rep = reg.sync_hosts(s.path(), &env).unwrap();
        assert_eq!(rep.connected, 0, "static 通道不算连接");
        let q = reg
            .list_agent_hosts()
            .unwrap()
            .into_iter()
            .find(|h| h.id == "qoder")
            .unwrap();
        assert!(!q.connected);
        assert!(
            q.evidence.iter().any(|e| e.contains("static 推断不算连接")),
            "证据里要说明为什么不算: {:?}",
            q.evidence
        );
    }

    #[test]
    fn heartbeat_channel_counts_as_connected_and_carries_last_seen() {
        let s = Scratch::new("hosts-heartbeat");
        let reg = crate::store::Registry::open_in_memory().unwrap();
        let env = FakeEnv::new();
        reg.sync_hosts(s.path(), &env).unwrap();

        let now = crate::agents::now_unix();
        reg.conn
            .execute(
                "INSERT INTO agents (agent_id, display_name, ecosystem, kind, transport,
                                     endpoint, first_seen, last_seen, meta)
                 VALUES ('heartbeat:qoder','qoder','qoder','host','heartbeat','',?1,?1,'{}')",
                rusqlite::params![now],
            )
            .unwrap();
        let rep = reg.sync_hosts(s.path(), &env).unwrap();
        assert_eq!(rep.connected, 1);
        let q = reg
            .list_agent_hosts()
            .unwrap()
            .into_iter()
            .find(|h| h.id == "qoder")
            .unwrap();
        assert!(q.connected);
        assert!(q.last_seen > 0, "last_seen 必须是 unix 秒且非 0");
        assert!(
            (q.last_seen - now).abs() < 5,
            "last_seen 应该是最近的秒级时间戳，实际 {}",
            q.last_seen
        );
    }

    #[test]
    fn stale_heartbeat_is_not_connected() {
        let s = Scratch::new("hosts-stale");
        let reg = crate::store::Registry::open_in_memory().unwrap();
        let env = FakeEnv::new();
        reg.sync_hosts(s.path(), &env).unwrap();
        let old = crate::agents::now_unix() - (crate::agents::STALE_WINDOW_MS / 1000) - 60;
        reg.conn
            .execute(
                "INSERT INTO agents (agent_id, display_name, ecosystem, kind, transport,
                                     endpoint, first_seen, last_seen, meta)
                 VALUES ('heartbeat:qoder','qoder','qoder','host','heartbeat','',?1,?1,'{}')",
                rusqlite::params![old],
            )
            .unwrap();
        let rep = reg.sync_hosts(s.path(), &env).unwrap();
        assert_eq!(rep.connected, 0, "超过 stale 窗口就不算连接");
    }

    #[test]
    fn hosts_overview_numbers_come_from_the_table() {
        let s = Scratch::new("hosts-overview");
        let reg = crate::store::Registry::open_in_memory().unwrap();
        reg.sync_hosts(s.path(), &FakeEnv::new()).unwrap();
        let ov = reg.hosts_overview(s.path()).unwrap();
        assert_eq!(ov.total as usize, builtin_specs().len());
        assert_eq!(ov.detected, 0, "假机器上什么都没装");
        assert_eq!(ov.connected, 0);
        let expect_protocols: usize = builtin_specs().iter().map(|s| s.protocols.len()).sum();
        assert_eq!(ov.protocol_rows as usize, expect_protocols);
        assert!(ov.hosts_dir.ends_with("hosts"));
        assert!(!ov.by_support.is_empty());
        assert!(!ov.by_kind.is_empty());
    }

    #[test]
    fn asset_host_support_reads_the_asset_row() {
        let reg = crate::store::Registry::open_in_memory().unwrap();
        let mut m =
            ac_core::CapabilityManifest::new("skill::zh", ac_core::AssetKind::Skill, "中文技能");
        m.provenance.source_path = Some("D:/v/vendor/skill/zh/.qoder-plugin/SKILL.md".into());
        m.provenance.source_format = Some("skill_md".into());
        m.provenance.content_hash = Some("h1".into());
        reg.upsert(&m).unwrap();

        let got = reg.asset_host_support("skill::zh").unwrap().unwrap();
        assert_eq!(got, vec!["qoder".to_string()], "路径标记应判给 qoder");

        // 不存在返回 None，不是空表——两种含义必须分得开
        assert!(reg.asset_host_support("不存在").unwrap().is_none());

        let detail = reg.asset_host_support_detail("skill::zh").unwrap().unwrap();
        assert!(!detail.basis.is_empty(), "详情必须带依据");
    }
}
