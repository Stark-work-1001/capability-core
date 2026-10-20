//! M3 产品化：**智能体接入**的三个通道（A / B / C）与它们的落库。
//!
//! # 三个通道分别是什么
//!
//! | 通道 | 名字 | 谁主动 | 数据从哪来 | 本轮做到哪一步 |
//! |---|---|---|---|---|
//! | **C** | 静态登记 | 中枢 | 扫描 vendor/ 时的资产路径推断 | **完整实现**（本轮地基） |
//! | **B** | 共享目录心跳 | 别的宿主 | data/heartbeat/*.json | **只定义格式并读取**，绝不替别的软件写适配器 |
//! | **A** | MCP 端点注册 | 别的宿主 | 宿主主动调用中枢的注册/上报命令 | **只做中枢侧的服务与协议**，不强求现在有宿主接入 |
//!
//! # 为什么这个顺序
//!
//! C 是地基：A 和 B 都可能一个宿主都没有接，但 C 永远能从磁盘得出结果。
//! 先把地基修好，"暂无数据"才是一个**有意义**的信号——
//! 它意味着连静态归属都没建起来，而不是"库是空的"。
//!
//! # 数据真实性
//!
//! 面板上每个数字都来自本模块的 SQL，**没有占位值**。某通道一个宿主都没有时，
//! 上层显示"暂无数据 + 原因"，而不是画一条长度为零的柱子假装有数据。
//!
//! # 语义边界
//!
//! agent_capability_usage.source 是三通道唯一共用的列，也是**语义边界**：
//!
//! - static    —— 磁盘路径**推断**出来的归属。call_count 恒为 0。
//! - heartbeat —— 宿主在心跳文件里**自报**的"我加载了这些"。
//! - mcp       —— 宿主通过注册端点**实报**的调用，call_count 真实累计。
//!
//! 把"推断"和"实测"塞进同一列而不加标记，是这类面板最常见的骗人方式：
//! 界面上写着"某智能体调用了 3 次"，实际只是路径长得像。本模块从表结构上
//! 就不允许这种混淆。

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// 静态登记通道：C。归属来自磁盘路径推断，call_count 恒为 0。
pub const TRANSPORT_STATIC: &str = "static";
/// 共享目录心跳通道：B。
pub const TRANSPORT_HEARTBEAT: &str = "heartbeat";
/// MCP 端点注册通道：A。
pub const TRANSPORT_MCP: &str = "mcp";

/// 心跳目录名，约定在数据目录下：<data_dir>/heartbeat/。
pub const HEARTBEAT_DIR_NAME: &str = "heartbeat";

/// 心跳文件扩展名（不含点）。只认 json，其它扩展名一律忽略
/// （不该替别的软件猜格式）。
///
/// 注意是 `json` 而不是 `.json`：`Path::extension()` 返回的就是不带点的形式。
/// 写成 ".json" 时比较恒为 false，于是整个心跳目录会被当成"一个文件都没有"——
/// 面板上显示"暂无数据"，而目录里其实躺着文件。
pub const HEARTBEAT_FILE_EXT: &str = "json";

/// 距上次心跳不超过这个毫秒数 = online。
pub const ONLINE_WINDOW_MS: i64 = 90_000;
/// 超过 online 但不超过这个毫秒数 = stale（还活着但没按时上报）。
pub const STALE_WINDOW_MS: i64 = 900_000;

/// 无法归属到任何已知宿主的生态键。**这是事实，不是占位符**：
/// vendor 树里确实有大量资产不带任何宿主标记。
pub const ECOSYSTEM_UNASSIGNED: &str = "unassigned";

// ── 数据形状 ──────────────────────────────────────────────────────

/// 一个智能体在 agents 表里的投影，附带其心跳与用量摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub agent_id: String,
    pub display_name: String,
    pub ecosystem: String,
    pub kind: String,
    pub transport: String,
    pub endpoint: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub meta: String,
    /// 心跳通道才有；static / mcp 通道为 None。
    pub heartbeat: Option<HeartbeatState>,
    /// 该智能体名下登记的能力数（agent_capability_usage 行数）。
    pub capability_count: u32,
    /// 实报调用总次数。static 通道恒为 0——推断不是实测。
    pub call_total: i64,
    /// 最近一次实报的失败原因，没有则为空串。
    pub last_error: String,
    /// 该智能体**实际接通过的全部通道**（去重、字母序）。
    ///
    /// 为什么需要它：同一个智能体可能同时有 A 通道（mcp 端点注册）和
    /// B 通道（心跳文件）。`transport` 只能报一个，用户就问「为什么只显示
    /// 一个通道」。这里如实列出全部，界面按它渲染通道徽标。
    #[serde(default)]
    pub channels: Vec<String>,
}

/// B 通道的心跳状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatState {
    pub host: String,
    pub version: String,
    pub pid: Option<i64>,
    /// 心跳自报的时间（unix 秒）。
    pub last_beat: i64,
    /// 读到的时间与自报时间的差（毫秒）。自报缺失时退回文件 mtime。
    pub age_ms: i64,
    pub source_file: String,
    /// online / stale / offline
    pub state: String,
    pub capabilities: Vec<String>,
}

/// 谁拉了哪项能力的一行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRow {
    pub agent_id: String,
    pub agent_name: String,
    pub ecosystem: String,
    pub capability_id: String,
    pub asset_id: String,
    pub asset_name: String,
    /// static / heartbeat / mcp —— 决定这一行到底有多可信。
    pub source: String,
    pub call_count: i64,
    pub first_used_at: i64,
    pub last_used_at: i64,
    pub last_error: String,
}

/// 跨宿主来源分布图的一根柱子。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcosystemCount {
    pub ecosystem: String,
    /// 该生态下登记的智能体数（static 通道恒为 1）。
    pub agents: u32,
    /// 该生态下**声明了能力**的资产数。
    pub assets: u32,
    /// 该生态下的能力条目数。
    pub capabilities: u32,
    /// 其中实报过调用（source=mcp）的条目数。
    pub reported: u32,
}

/// 总览需要的全部数字。字段全部来自 SQL，**没有一项是猜的**。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentOverview {
    pub total_agents: u32,
    pub online: u32,
    pub stale: u32,
    pub offline: u32,
    /// 注册了但从未上报过心跳的智能体数。
    pub unknown: u32,
    pub static_agents: u32,
    pub heartbeat_agents: u32,
    pub mcp_agents: u32,
    pub ecosystems: Vec<EcosystemCount>,
    pub total_assets: u32,
    /// 声明了能力的资产数（分母口径，跨宿主分布图只用这一部分）。
    pub assets_with_capabilities: u32,
    pub total_capabilities: u32,
    /// 全库资产里**没有声明任何能力**的数量。分布图不覆盖它们，这里单独说清。
    pub zero_capability_assets: u32,
}

/// C 通道的一次静态登记结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StaticRegistrationReport {
    pub ecosystems: u32,
    pub capability_rows: u32,
    /// 被 static 通道覆盖的资产数。
    pub assets: u32,
    /// 本次没被任何生态认领的资产数（等于全库减去上面那个数）。
    pub unassigned_assets: u32,
    /// 每个生态各认领了多少能力，给 UI 直接画分布图用。
    pub per_ecosystem: Vec<EcosystemCount>,
}

/// B 通道扫描心跳目录的结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeartbeatScanReport {
    pub dir: String,
    pub dir_exists: bool,
    /// 目录里被读到的 .json 文件数。
    pub files_seen: u32,
    /// 解析成功并登记的宿主数。
    pub hosts_registered: u32,
    /// 解析失败的文件与原因。**不静默吞掉**：宿主写错了格式要看得见。
    pub rejected: Vec<HeartbeatReject>,
}

/// 一个读不懂的心跳文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatReject {
    pub file: String,
    pub reason: String,
}

/// A 通道的注册请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegistration {
    pub agent_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub ecosystem: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub meta: String,
}

fn default_kind() -> String {
    "host".to_string()
}

/// 心跳文件格式（B 通道的对外契约）。
///
/// **中枢只读、不写。** 别的宿主想接入，就按这个形状往
/// <data_dir>/heartbeat/<host>.json 写文件；中枢只负责扫描和展示。
/// 未知字段一律忽略；last_heartbeat 支持秒与毫秒两种量纲。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeartbeatDoc {
    /// 宿主的**人类可读名 / 机器名**。
    ///
    /// ⚠️ 这**不是**智能体身份。身份见 [`Self::id`] / [`Self::agent`]。
    /// 早先把 host 当成 agent_id 用，于是同一个宿主被拆成两条记录：
    ///   A 通道注册：`deepseek-harness`
    ///   B 通道心跳：`heartbeat:WIN-MNI8FILL2II`（host 是主机名）
    /// 界面按 A 通道那条显示时永远看不到心跳 → 「在线」永远是 0。
    /// host 保留它的原义（哪台机器 / 哪个安装），只做展示与排障用。
    pub host: String,
    /// 智能体身份（可选）。**有它就以它为准**。
    /// 与 A 通道 `register_agent` 的 agent_id 同一个值，两条通道才能合并。
    #[serde(default)]
    pub id: String,
    /// 同 [`Self::id`] 的别名（插件两侧字段名不同，都收）。
    #[serde(default)]
    pub agent: String,
    /// 界面显示名（可选）。
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub pid: Option<i64>,
    /// unix 秒或毫秒皆可；缺省时退回文件 mtime。
    #[serde(default)]
    pub last_heartbeat: Option<i64>,
    /// 有的实现写 `last_beat` 而不是 `last_heartbeat`（插件实测两边都有）。
    #[serde(default)]
    pub last_beat: Option<i64>,
    /// 已加载的能力 id 列表。
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl HeartbeatDoc {
    /// 这份心跳代表**哪个智能体**。
    ///
    /// 优先级：`id` > `agent` > `host` > 文件名。
    ///
    /// 为什么 host 只是三级回退而不是首选：host 通常写的是**机器名**
    /// （`WIN-MNI8FILL2II` 这种），拿它当身份会把「一台机器上跑的同一个
    /// 智能体」和「它通过 A 通道注册的身份」撕成两条记录。
    /// id/agent 没有时才退回 host —— 那是老格式，能收就收，但不能让新格式
    /// 继续被它带偏。
    pub fn identity(&self, file_stem: Option<&str>) -> Option<String> {
        for candidate in [&self.id, &self.agent, &self.host] {
            let v = candidate.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
        file_stem
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// 心跳时间戳：`last_heartbeat` 优先，其次 `last_beat`。
    pub fn beat_at(&self) -> Option<i64> {
        self.last_heartbeat.or(self.last_beat)
    }
}

/// C 通道的归属推断结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcosystemGuess {
    pub ecosystem: String,
    /// 命中的规则，写进 UI 让用户知道"凭什么这么归"。
    pub basis: String,
    /// high / medium / none
    pub confidence: &'static str,
}

// ── C 通道：静态归属推断 ──────────────────────────────────────────

/// 包名关键词表。**只认明确的命名约定**，不做模糊匹配——
/// 猜错宿主比"不知道属于谁"有害得多。
const PACKAGE_HINTS: &[(&str, &str)] = &[
    ("dsh-", "dsh"),
    ("zion", "zion"),
    ("obsidian", "obsidian"),
    ("matlab", "matlab"),
    ("codex", "codex"),
    ("claude", "claude"),
];

/// 从资产磁盘路径推断宿主生态。
///
/// 判定顺序是**刻意的**，从强到弱：
///
/// 1. 路径里出现 .<host>-plugin 目录（.qoder-plugin / .claude-plugin …）。
///    这是插件自己声明的宿主，证据最强，排在包名关键词之前。
/// 2. 否则看 vendor 包名（vendor/<kind>/<pkg>/... 的第 4 段）是否命中
///    PACKAGE_HINTS。
/// 3. 都没命中 → unassigned。**这是结论不是占位**：库里确实有大量资产
///    不带任何宿主标记，硬猜一个生态就是在造假数据。
pub fn infer_ecosystem(source_path: &str) -> EcosystemGuess {
    let norm = source_path.replace('\\', "/");
    let segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();

    // ① 声明式目录标记。取**路径上第一个**，与目录深度无关——
    // .qoder-plugin 比包名 zion-nocode 更具体（同一份资产同时带两者时，
    // 目录标记胜出：那是插件作者自己写的）。
    for s in &segs {
        // **必须真的以点开头。** 否则 vendor/skill/dsh-blender-plugin/SKILL.md
        // 里的包名会被当成 ".dsh-blender-plugin"，推出一个不存在的生态
        // "dsh-blender"——这正是把"猜错宿主"写进产品的那种 bug。
        if let Some(inner) = s.strip_prefix('.') {
          if let Some(host) = inner.strip_suffix("-plugin") {
            if !host.is_empty() {
                return EcosystemGuess {
                    ecosystem: host.to_ascii_lowercase(),
                    basis: format!("路径段 {s}（插件自声明宿主）"),
                    confidence: "high",
                };
            }
          }
        }
    }

    // ② 包名关键词。vendor/<kind>/<pkg>/... → 取 <pkg>。
    let pkg = segs.get(3).copied().unwrap_or("");
    let lower = pkg.to_ascii_lowercase();
    for (needle, eco) in PACKAGE_HINTS {
        if lower.contains(needle) {
            return EcosystemGuess {
                ecosystem: (*eco).to_string(),
                basis: format!("包名 {pkg} 命中约定 {needle}"),
                confidence: "medium",
            };
        }
    }

    EcosystemGuess {
        ecosystem: ECOSYSTEM_UNASSIGNED.to_string(),
        basis: format!("路径 {source_path} 无宿主标记"),
        confidence: "none",
    }
}

// ── 工具 ──────────────────────────────────────────────────────────

/// 当前 unix 秒。抽出来是为了让测试能直接断言"刚刚写入 = 新鲜"。
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 心跳时间归一到 unix 秒：>= 1e12 视为毫秒，否则视为秒。
fn normalize_epoch(v: i64) -> i64 {
    if v >= 1_000_000_000_000 {
        v / 1000
    } else {
        v
    }
}

/// 文件 mtime（秒）。读不到返回 None。
fn file_mtime_secs(path: &Path) -> Option<i64> {
    let md = std::fs::metadata(path).ok()?;
    let t = md.modified().ok()?;
    let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(d.as_secs() as i64)
}

/// 按与上次心跳的间隔判定在线状态。
///
/// 三档而不是两档：stale 这一档是运维真正需要的——
/// "还在线但已经 3 分钟没心跳"和"昨天就关了"是完全不同的两种事，
/// 合成一个 offline 会让人以为刚挂。
pub fn observe_state(age_ms: i64) -> &'static str {
    if age_ms <= ONLINE_WINDOW_MS {
        "online"
    } else if age_ms <= STALE_WINDOW_MS {
        "stale"
    } else {
        "offline"
    }
}

// ── Registry 上的接入操作 ─────────────────────────────────────────

impl crate::store::Registry {
    // ── C：静态登记 ───────────────────────────────────────────────

    /// 扫描全库资产，按 infer_ecosystem 推断宿主生态并落库。
    ///
    /// **幂等**：重复调用只会刷新计数，不会产生重复行
    /// （agent_capability_usage 主键是 (agent_id, capability_id)）。
    ///
    /// 覆盖不到的部分（无宿主标记 / 零能力资产）**不写入任何行**，
    /// 由 AgentOverview 如实报出，不靠编数字补齐。
    pub fn register_static_agents(&self) -> Result<StaticRegistrationReport, String> {
        let now = now_unix();

        // 资产 + 其能力，一趟读完，避免每行再查一次。
        let rows: Vec<(String, String, String, String)> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT a.id, a.name, a.source_path,
                            COALESCE(c.capability_id, '')
                     FROM assets a
                     LEFT JOIN capabilities c ON c.asset_id = a.id
                     ORDER BY a.id, c.capability_id",
                )
                .map_err(|e| e.to_string())?;
            let it = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    ))
                })
                .map_err(|e| e.to_string())?;
            let mut acc = Vec::new();
            for row in it {
                acc.push(row.map_err(|e| e.to_string())?);
            }
            acc
        };

        // 按资产聚合推断结果（同一资产的所有能力行共享同一个生态）。
        let mut covered: BTreeMap<String, (String, String)> = BTreeMap::new();
        let mut eco_basis: BTreeMap<String, String> = BTreeMap::new();
        for (id, name, path, _cap) in &rows {
            if covered.contains_key(id) {
                continue;
            }
            let guess = infer_ecosystem(path);
            eco_basis.entry(guess.ecosystem.clone()).or_insert(guess.basis);
            covered.insert(id.clone(), (guess.ecosystem.clone(), name.clone()));
        }

        // 建/更新 agents 行。生态里没有任何资产时**不建行**——
        // 建一个空壳生态只会让"分布图"凭空多一根柱子。
        for (eco, basis) in &eco_basis {
            if eco == ECOSYSTEM_UNASSIGNED {
                continue;
            }
            self.upsert_agent_row(
                &format!("static:{eco}"),
                eco,
                TRANSPORT_STATIC,
                "",
                &format!("{{\"basis\":{}}}", json_str(basis)),
                now,
            )?;
        }

        // 能力归属。
        let mut per: BTreeMap<String, EcosystemCount> = BTreeMap::new();
        let mut written = 0u32;
        for (id, _name, path, cap) in &rows {
            if cap.is_empty() {
                continue;
            }
            let guess = infer_ecosystem(path);
            if guess.ecosystem == ECOSYSTEM_UNASSIGNED {
                continue;
            }
            let agent_id = format!("static:{}", guess.ecosystem);
            self.upsert_usage_row(&agent_id, cap, id, TRANSPORT_STATIC, now)?;
            written += 1;
            per.entry(guess.ecosystem.clone()).or_insert(EcosystemCount {
                ecosystem: guess.ecosystem.clone(),
                agents: 1,
                assets: 0,
                capabilities: 0,
                reported: 0,
            });
        }
        // 资产数按 distinct asset_id 统计（一个资产多条能力只能算一次）。
        let mut seen_in_eco: BTreeMap<(String, String), ()> = BTreeMap::new();
        for (id, _name, path, cap) in &rows {
            if cap.is_empty() {
                continue;
            }
            let eco = infer_ecosystem(path).ecosystem;
            if eco == ECOSYSTEM_UNASSIGNED {
                continue;
            }
            seen_in_eco.entry((eco.clone(), id.clone())).or_insert(());
        }
        for (eco, _id) in seen_in_eco.keys() {
            if let Some(e) = per.get_mut(eco) {
                e.assets += 1;
            }
        }

        let unassigned_assets = covered
            .values()
            .filter(|(eco, _)| eco == ECOSYSTEM_UNASSIGNED)
            .count() as u32;

        Ok(StaticRegistrationReport {
            ecosystems: per.len() as u32,
            capability_rows: written,
            assets: per.values().map(|e| e.assets).sum(),
            unassigned_assets,
            per_ecosystem: per.into_values().collect(),
        })
    }

    // ── B：共享目录心跳 ───────────────────────────────────────────

    /// 扫描 <data_dir>/heartbeat/*.json 并登记。
    ///
    /// **只读别人的文件。** 这里不写、不改、不删心跳目录里的任何东西，
    /// 也不为别的宿主生成适配器——中枢只定义格式并读取。
    ///
    /// 目录不存在不是错误：那意味着"还没有宿主接入"，返回
    /// dir_exists = false 让 UI 显示"暂无数据"而不是报错。
    pub fn scan_heartbeat_dir(&self, dir: &Path) -> Result<HeartbeatScanReport, String> {
        let mut report = HeartbeatScanReport {
            dir: dir.display().to_string(),
            dir_exists: dir.is_dir(),
            ..Default::default()
        };
        if !dir.is_dir() {
            return Ok(report);
        }

        let entries = std::fs::read_dir(dir).map_err(|e| format!("读心跳目录失败: {e}"))?;
        let mut files: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .map(|x| x == HEARTBEAT_FILE_EXT)
                        .unwrap_or(false)
            })
            .collect();
        // 目录遍历顺序不稳定；排序让"同一目录两次扫描产出同一份报告"。
        files.sort();

        let now = now_unix();
        for f in files {
            report.files_seen += 1;
            let text = match std::fs::read_to_string(&f) {
                Ok(t) => t,
                Err(e) => {
                    report.rejected.push(HeartbeatReject {
                        file: f.display().to_string(),
                        reason: format!("读取失败: {e}"),
                    });
                    continue;
                }
            };
            // **必须容忍 UTF-8 BOM**。
            //
            // 心跳文件是**别的程序写的**，不是我们写的。Windows 上
            // PowerShell 5.1 的 `Set-Content -Encoding UTF8` 会带 BOM，
            // 而 serde_json 见到 U+FEFF 会直接报 "expected value at line 1
            // column 1" —— 报错说的是 JSON 语法，用户却会以为文件是坏的。
            // 实测踩到：心跳文件内容完全正确，只因为开头 3 个字节就永远被拒。
            // 这里显式剥掉 BOM 再解析；剥的只是编码前缀，不改内容语义。
            let text = text.trim_start_matches('\u{feff}');
            let doc: HeartbeatDoc = match serde_json::from_str(text) {
                Ok(d) => d,
                Err(e) => {
                    report.rejected.push(HeartbeatReject {
                        file: f.display().to_string(),
                        reason: format!("JSON 解析失败: {e}"),
                    });
                    continue;
                }
            };
            // 身份判定：id > agent > host > 文件名（见 HeartbeatDoc::identity）。
            //
            // **这里曾经直接拿 host 当 agent_id**，于是同一宿主的两条通道
            // 落成了两个身份，界面上的「在线」永远是 0。host 现在只做
            // 展示与排障（哪台机器），不再参与身份。
            let stem = f.file_stem().map(|s| s.to_string_lossy().to_string());
            let Some(agent_identity) = doc.identity(stem.as_deref()) else {
                report.rejected.push(HeartbeatReject {
                    file: f.display().to_string(),
                    reason: "缺少 id / agent / host 且文件名无法作为标识".into(),
                });
                continue;
            };
            // 展示名：心跳自报的 name 优先，否则用身份；host 单独存起来。
            let display = {
                let n = doc.name.trim();
                if !n.is_empty() {
                    n.to_string()
                } else {
                    agent_identity.clone()
                }
            };
            let host = if doc.host.trim().is_empty() {
                stem.clone().unwrap_or_else(|| agent_identity.clone())
            } else {
                doc.host.trim().to_string()
            };

            // 自报时间缺失/离谱时退回文件 mtime，而不是当"从未心跳"。
            // `beat_at()` 同时认 last_heartbeat 与 last_beat（插件两侧都写过）。
            let last_beat = doc
                .beat_at()
                .map(normalize_epoch)
                .filter(|v| *v > 0)
                .or_else(|| file_mtime_secs(&f))
                .unwrap_or(now);
            let age_ms = (now - last_beat).max(0) * 1000;
            if !crate::presence::valid_heartbeat(last_beat, now) {
                report.rejected.push(HeartbeatReject { file: f.display().to_string(), reason: "心跳时间在未来，不能作为在线证据".into() });
                continue;
            }
            let state = observe_state(age_ms);

            // **agent_id 不再带 `heartbeat:` 前缀**。
            //
            // 那个前缀是为了区分「心跳通道」与「A 通道注册」而加的，但它
            // 直接进了主键，于是同一个智能体变成两条不可合并的记录。通道
            // 本来就由 transport 列表达，不需要再编码进 id。
            // 现在 agent_id == 身份，所以心跳与 HTTP 注册会写进同一行。
            let agent_id = agent_identity.clone();
            // ecosystem：心跳协议里没有这个字段，所以传 None 落到 unassigned。
            //
            // 为什么不从 agent_identity 里猜：`deepseek-harness` 属于哪个生态
            // 是**中枢侧的知识**（hosts.rs 的 alias 表），不是心跳文件该背的
            // 责任。让 hosts::sync_hosts 用 alias 去匹配，比在这里做字符串
            // 前缀匹配准确得多。
            self.upsert_agent_row_eco(
                &agent_id,
                &display,
                TRANSPORT_HEARTBEAT,
                &f.display().to_string(),
                &format!("{{\"basis\":{},\"host\":{}}}", json_str("心跳目录"), json_str(&host)),
                last_beat,
                None,
            )?;
            self.upsert_heartbeat_row(
                &agent_id,
                &host,
                &doc.version,
                doc.pid,
                last_beat,
                &f.display().to_string(),
                state,
                &doc.capabilities,
                now,
            )?;
            for cap in &doc.capabilities {
                self.upsert_usage_row(&agent_id, cap, "", TRANSPORT_HEARTBEAT, now)?;
            }
            report.hosts_registered += 1;
        }
        Ok(report)
    }

    // ── A：MCP 端点注册 ──────────────────────────────────────────

    /// 宿主主动注册。返回规范化后的 agent_id。
    ///
    /// 本轮**只交付中枢侧的服务与协议**：命令可调、协议稳定、数据落库，
    /// 但不强求此刻真有宿主接进来。没有宿主时 list_agents 里就没有 mcp 通道的行，
    /// UI 据此显示"暂无数据 + 原因"，而不是造一条假记录。
    pub fn register_agent(&self, reg: &AgentRegistration) -> Result<String, String> {
        let id = reg.agent_id.trim().to_string();
        if id.is_empty() {
            return Err("agent_id 不能为空".into());
        }
        let now = now_unix();
        let eco = if reg.ecosystem.trim().is_empty() {
            ECOSYSTEM_UNASSIGNED.to_string()
        } else {
            reg.ecosystem.trim().to_ascii_lowercase()
        };
        let display = if reg.display_name.trim().is_empty() {
            id.clone()
        } else {
            reg.display_name.trim().to_string()
        };
        let kind = if reg.kind.trim().is_empty() {
            default_kind()
        } else {
            reg.kind.trim().to_string()
        };
        let meta = if reg.meta.trim().is_empty() {
            "{}".to_string()
        } else {
            reg.meta.trim().to_string()
        };
        self.upsert_agent_row(&id, &display, TRANSPORT_MCP, &reg.endpoint, &meta, now)?;
        // 生态键在 upsert 里对 mcp 通道写的是占位，这里显式校正，
        // 免得 meta 与 ecosystem 两处各说各话。
        self.conn
            .execute(
                "UPDATE agents SET ecosystem = ?2, kind = ?3 WHERE agent_id = ?1",
                params![id, eco, kind],
            )
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    /// 清理**测试残留**的智能体注册行。
    ///
    /// 判据三条同时成立才删（保守，宁可留着也不误删真智能体）：
    /// 1. `meta` 是空对象 `{}` 或空串 —— 真实注册会写协议/版本/host 等；
    /// 2. 没有任何 `agent_capability_usage` 引用；
    /// 3. `first_seen == last_seen` —— 一次性写入、此后再没上报过。
    ///
    /// 为什么这件事有必要：插件/契约测试会 POST `/api/v1/agents` 造一条记录
    /// 验证服务端行为，跑完不清理。这类行会出现在界面的「在线智能体」里，
    /// 用户看到的是一个不存在的智能体（实测踩到 `shape-probe`）。
    ///
    /// **只动 agents 表**，不碰资产、不碰 usage（本来就没有）。返回删掉的 id。
    pub fn purge_synthetic_agents(&self) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT a.agent_id FROM agents a
                 WHERE a.transport = ?1
                   AND TRIM(COALESCE(a.meta, '')) IN ('', '{}')
                   AND a.first_seen = a.last_seen
                   AND NOT EXISTS (
                         SELECT 1 FROM agent_capability_usage u WHERE u.agent_id = a.agent_id
                   )
                 ORDER BY a.agent_id",
            )
            .map_err(|e| e.to_string())?;
        let ids: Vec<String> = stmt
            .query_map(params![TRANSPORT_MCP], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        for id in &ids {
            self.conn
                .execute("DELETE FROM agents WHERE agent_id = ?1", params![id])
                .map_err(|e| e.to_string())?;
            // 心跳行也一并清（理论上 synthesis 行没有心跳，防御性处理）
            self.conn
                .execute("DELETE FROM agent_heartbeat WHERE agent_id = ?1", params![id])
                .map_err(|e| e.to_string())?;
        }
        Ok(ids)
    }

    /// 宿主实报一次能力调用，返回累计次数。
    ///
    /// **未注册的智能体一律报错**，绝不顺手建一行 agents：
    /// 否则任何拼错的 id 都会凭空造出一个"智能体"，
    /// 总览上的在线数就会变成不可信的数字。
    pub fn report_capability_usage(
        &self,
        agent_id: &str,
        capability_id: &str,
        ok: bool,
        error: Option<&str>,
    ) -> Result<i64, String> {
        let agent_id = agent_id.trim();
        let capability_id = capability_id.trim();
        if agent_id.is_empty() || capability_id.is_empty() {
            return Err("agent_id 与 capability_id 都不能为空".into());
        }
        let known: Option<String> = self
            .conn
            .query_row(
                "SELECT agent_id FROM agents WHERE agent_id = ?1",
                params![agent_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if known.is_none() {
            return Err(format!("智能体 {agent_id} 未注册，请先调用 register_agent"));
        }
        let now = now_unix();
        let err = if ok {
            String::new()
        } else {
            error.unwrap_or("").to_string()
        };
        self.conn
            .execute(
                "INSERT INTO agent_capability_usage
                     (agent_id, capability_id, asset_id, source, call_count,
                      first_used_at, last_used_at, last_error)
                 VALUES (?1, ?2, '', ?3, 1, ?4, ?4, ?5)
                 ON CONFLICT(agent_id, capability_id) DO UPDATE SET
                     call_count   = call_count + 1,
                     last_used_at = excluded.last_used_at,
                     last_error   = excluded.last_error",
                params![agent_id, capability_id, TRANSPORT_MCP, now, err],
            )
            .map_err(|e| e.to_string())?;
        let n: i64 = self
            .conn
            .query_row(
                "SELECT call_count FROM agent_capability_usage
                 WHERE agent_id = ?1 AND capability_id = ?2",
                params![agent_id, capability_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(n)
    }

    // ── 读 ───────────────────────────────────────────────────────

    /// 全部智能体 + 心跳 + 用量摘要。
    ///
    /// # 身份合并（v9 起）
    ///
    /// 同一个智能体可能有**多条通道记录**：
    /// - A 通道：`register_agent` 写 `transport=mcp`
    /// - B 通道：`scan_heartbeat_dir` 写 `transport=heartbeat`
    ///
    /// 早先这两条是不同的 agent_id（心跳那条带 `heartbeat:` 前缀），于是
    /// 界面把同一个 DSH 显示成两个节点，而「在线」判定落在没被当成主节点的
    /// 那一条上 —— 用户看到的就是「有这台机器，但在线数还是 0」。
    ///
    /// 现在：**主键就是身份**，两条通道写同一行。但历史库里可能已经存在
    /// 带前缀的旧行，所以这里再做一层归并：把 `heartbeat:<id>` 与 `<id>`
    /// 视为同一智能体，合并成一条返回。旧行不删（增量不删），只是不再
    /// 作为独立节点暴露。
    pub fn list_agents(&self) -> Result<Vec<AgentRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT agent_id, display_name, ecosystem, kind, transport, endpoint,
                        first_seen, last_seen, meta
                 FROM agents ORDER BY transport, ecosystem, agent_id",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, String>(8)?,
                ))
            })
            .map_err(|e| e.to_string())?;

        // ── 先按「规范身份」聚合通道 ──────────────────────────────
        //
        // canonical(id)：去掉历史遗留的 `heartbeat:` 前缀。
        // 于是 `deepseek-harness`(mcp) 与 `heartbeat:deepseek-harness`(心跳)
        // 归到同一个键上；归属键仍是 agent_id，但只是内部用于合并。
        struct Acc {
            agent_id: String,
            display_name: String,
            ecosystem: String,
            kind: String,
            /// 各通道的 (transport, 该行 agent_id, last_seen, endpoint)
            rows: Vec<(String, String, i64, String)>,
            first_seen: i64,
            meta: String,
        }
        let mut accs: Vec<Acc> = Vec::new();
        let mut index: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for row in it {
            let (agent_id, display_name, ecosystem, kind, transport, endpoint, first_seen, last_seen, meta) =
                row.map_err(|e| e.to_string())?;
            let canonical = agent_id
                .strip_prefix("heartbeat:")
                .unwrap_or(&agent_id)
                .to_string();
            // 生态键的取舍：**非 unassigned 优先，且 mcp/static 优先于 heartbeat**。
            //
            // 实测踩到的坑：心跳行不知道生态（心跳协议里没这个字段），落
            // unassigned；而它的 agent_id 恰好与宿主 id 同名，于是一眼看上去
            // 那个 ecosystem 像是对的（"deepseek-harness"）—— 其实那是 agent_id
            // 被当成了生态。真实答案（"dsh"）只有 A 通道注册时才知道。
            // 判据：**unassigned 与「等于自己 agent_id」都算无效生态**。
            //
            // 第二条是实测抓到的：旧版 upsert 的兼容分支会把不带冒号的
            // agent_id 整串当 ecosystem，于是 `deepseek-harness` 的 eco 变成
            // 它自己的名字。那看起来像个合法值，实际是身份冒充生态，
            // 会让「按生态分组」永远分错。
            let valid_eco = |eco: &str, id: &str| -> bool {
                !eco.trim().is_empty() && eco != ECOSYSTEM_UNASSIGNED && eco != id
            };
            let better_eco = |cur: &str, next: &str, id: &str| -> bool {
                if !valid_eco(next, id) {
                    return false;
                }
                !valid_eco(cur, id)
            };
            match index.get(&canonical).copied() {
                Some(i) => {
                    let a = &mut accs[i];
                    if a.display_name.trim().is_empty() || a.display_name == a.agent_id {
                        if !display_name.trim().is_empty() {
                            a.display_name = display_name.clone();
                        }
                    }
                    // A/B 两条都可能是 unassigned，只有注册行带真生态。
                    if better_eco(&a.ecosystem, &ecosystem, &canonical) {
                        a.ecosystem = ecosystem.clone();
                    }
                    a.first_seen = a.first_seen.min(first_seen);
                    a.rows.push((transport, agent_id, last_seen, endpoint));
                }
                None => {
                    index.insert(canonical.clone(), accs.len());
                    accs.push(Acc {
                        agent_id: canonical,
                        display_name,
                        ecosystem,
                        kind,
                        rows: vec![(transport, agent_id, last_seen, endpoint)],
                        first_seen,
                        meta,
                    });
                }
            }
        }

        let now = now_unix();
        let mut out = Vec::new();
        for a in accs {
            // 主通道：heartbeat > mcp > static。
            //
            // 为什么心跳优先：它带**实时在线状态**，是用户最关心的那个通道；
            // 而 transport 列只能存一个值，选错就会让界面显示"无心跳通道"。
            let rank = |t: &str| match t {
                TRANSPORT_HEARTBEAT => 0u8,
                TRANSPORT_MCP => 1,
                _ => 2,
            };
            let primary = a
                .rows
                .iter()
                .min_by_key(|(t, _, _, _)| rank(t))
                .cloned()
                .expect("每行至少一条通道");
            let mut channels: Vec<String> = a
                .rows
                .iter()
                .map(|(t, _, _, _)| t.clone())
                .collect();
            channels.sort();
            channels.dedup();
            let last_seen = a.rows.iter().map(|(_, _, s, _)| *s).max().unwrap_or(0);
            // 心跳行优先用它的 agent_id 取心跳状态；没有就退回空。
            let mut heartbeat = None;
            for (_, row_id, _, _) in &a.rows {
                if let Some(h) = self.heartbeat_of(row_id, now)? {
                    heartbeat = Some(h);
                    break;
                }
            }
            // 用量摘要按所有通道行累加（同一能力在两条通道下都算）。
            let mut capability_count: u32 = 0;
            let mut call_total: i64 = 0;
            let mut last_error = String::new();
            for (_, row_id, _, _) in &a.rows {
                let (c, n, e) = self.usage_summary(row_id)?;
                capability_count = capability_count.max(c);
                call_total += n;
                if last_error.is_empty() {
                    last_error = e;
                }
            }
            out.push(AgentRecord {
                agent_id: a.agent_id,
                display_name: if a.display_name.trim().is_empty() {
                    String::new()
                } else {
                    a.display_name
                },
                ecosystem: a.ecosystem,
                kind: a.kind,
                transport: primary.0,
                endpoint: a
                    .rows
                    .iter()
                    .map(|(_, _, _, e)| e.clone())
                    .find(|e| !e.trim().is_empty())
                    .unwrap_or_default(),
                first_seen: a.first_seen,
                last_seen,
                meta: a.meta,
                heartbeat,
                capability_count,
                call_total,
                last_error,
                channels,
            });
        }
        out.sort_by(|x, y| {
            x.transport
                .cmp(&y.transport)
                .then(x.ecosystem.cmp(&y.ecosystem))
                .then(x.agent_id.cmp(&y.agent_id))
        });
        Ok(out)
    }

    /// 总览数字。每一项都是 SQL 聚合，没有默认填充。
    pub fn agent_overview(&self) -> Result<AgentOverview, String> {
        let mut ov = AgentOverview::default();

        for r in self.list_agents()? {
            ov.total_agents += 1;
            match r.transport.as_str() {
                TRANSPORT_STATIC => ov.static_agents += 1,
                TRANSPORT_HEARTBEAT => ov.heartbeat_agents += 1,
                TRANSPORT_MCP => ov.mcp_agents += 1,
                _ => {}
            }
            match r.heartbeat.as_ref().map(|h| h.state.as_str()) {
                Some("online") => ov.online += 1,
                Some("stale") => ov.stale += 1,
                Some("offline") => ov.offline += 1,
                _ => ov.unknown += 1,
            }
        }

        ov.ecosystems = self.ecosystem_distribution()?;
        ov.total_assets = self.count_scalar("SELECT COUNT(*) FROM assets")? as u32;
        ov.total_capabilities = self.count_scalar("SELECT COUNT(*) FROM capabilities")? as u32;
        ov.assets_with_capabilities = self.count_scalar(
            "SELECT COUNT(DISTINCT asset_id) FROM capabilities WHERE asset_id <> ''",
        )? as u32;
        ov.zero_capability_assets = ov.total_assets.saturating_sub(ov.assets_with_capabilities);
        Ok(ov)
    }

    /// 跨宿主来源分布图。
    pub fn ecosystem_distribution(&self) -> Result<Vec<EcosystemCount>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT u.agent_id,
                        COALESCE(a.ecosystem, 'unassigned'),
                        COUNT(*),
                        COUNT(DISTINCT NULLIF(u.asset_id, '')),
                        COALESCE(SUM(CASE WHEN u.source = 'mcp' THEN 1 ELSE 0 END), 0)
                 FROM agent_capability_usage u
                 LEFT JOIN agents a ON a.agent_id = u.agent_id
                 GROUP BY u.agent_id, COALESCE(a.ecosystem, 'unassigned')
                 ORDER BY 3 DESC, 2 ASC",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map([], |r| {
                Ok(EcosystemCount {
                    ecosystem: r.get(1)?,
                    agents: 1,
                    capabilities: r.get::<_, i64>(2)? as u32,
                    assets: r.get::<_, i64>(3)? as u32,
                    reported: r.get::<_, i64>(4)? as u32,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in it {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// "哪些智能体拉了什么能力"。按最近使用倒序。
    pub fn capability_usage(&self, limit: u32) -> Result<Vec<UsageRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT u.agent_id,
                        COALESCE(a.display_name, u.agent_id),
                        COALESCE(a.ecosystem, 'unassigned'),
                        u.capability_id, u.asset_id,
                        COALESCE(s.name, ''),
                        u.source, u.call_count, u.first_used_at,
                        u.last_used_at, u.last_error
                 FROM agent_capability_usage u
                 LEFT JOIN agents a ON a.agent_id = u.agent_id
                 LEFT JOIN assets s ON s.id = u.asset_id AND u.asset_id <> ''
                 ORDER BY u.last_used_at DESC, u.agent_id, u.capability_id
                 LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(params![limit as i64], |r| {
                Ok(UsageRow {
                    agent_id: r.get(0)?,
                    agent_name: r.get(1)?,
                    ecosystem: r.get(2)?,
                    capability_id: r.get(3)?,
                    asset_id: r.get(4)?,
                    asset_name: r.get(5)?,
                    source: r.get(6)?,
                    call_count: r.get(7)?,
                    first_used_at: r.get(8)?,
                    last_used_at: r.get(9)?,
                    last_error: r.get(10)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in it {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    // ── 内部写路径 ────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn upsert_agent_row(
        &self,
        agent_id: &str,
        display: &str,
        transport: &str,
        endpoint: &str,
        meta: &str,
        now: i64,
    ) -> Result<(), String> {
        self.upsert_agent_row_eco(agent_id, display, transport, endpoint, meta, now, None)
    }

    /// 同 [`Self::upsert_agent_row`]，但可以显式指定 ecosystem。
    ///
    /// 为什么要这个重载：早先 ecosystem 是从 `通道:键` 形式的 agent_id 里
    /// 拆出来的（`heartbeat:x` → `x`）。身份改成不再带前缀之后，那条推导
    /// 就成了死代码 —— 更糟的是它会**静默给出错误答案**（整串当 ecosystem）。
    /// 所以把 ecosystem 变成调用方的责任：它知道自己是什么生态，猜不出来
    /// 就传 None（落 `unassigned`），不要让一个字符串拆分解谜。
    #[allow(clippy::too_many_arguments)]
    fn upsert_agent_row_eco(
        &self,
        agent_id: &str,
        display: &str,
        transport: &str,
        endpoint: &str,
        meta: &str,
        now: i64,
        ecosystem: Option<&str>,
    ) -> Result<(), String> {
        let eco = ecosystem
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .or_else(|| {
                // 兼容旧格式 `通道:键`（历史库里已经有这种行）。
                // 新写入一律显式给 ecosystem，不再依赖这条。
                //
                // 注意：只在**确实带冒号**时才拆。没有冒号却把整串当 eco
                // 是个静默错误 —— 它会让 `deepseek-harness` 的 ecosystem
                // 变成它自己的名字，看上去像个合法值，其实是身份冒充生态。
                agent_id
                    .split_once(':')
                    .map(|(_, e)| e.to_string())
                    .filter(|e| !e.trim().is_empty())
            })
            .unwrap_or_else(|| ECOSYSTEM_UNASSIGNED.to_string());
        self.conn
            .execute(
                "INSERT INTO agents
                     (agent_id, display_name, ecosystem, kind, transport, endpoint,
                      first_seen, last_seen, meta)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)
                 ON CONFLICT(agent_id) DO UPDATE SET
                     display_name = excluded.display_name,
                     ecosystem    = excluded.ecosystem,
                     transport    = excluded.transport,
                     endpoint     = excluded.endpoint,
                     last_seen    = excluded.last_seen,
                     meta         = excluded.meta",
                params![
                    agent_id,
                    display,
                    eco,
                    default_kind(),
                    transport,
                    endpoint,
                    now,
                    meta
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_heartbeat_row(
        &self,
        agent_id: &str,
        host: &str,
        version: &str,
        pid: Option<i64>,
        last_beat: i64,
        source_file: &str,
        state: &str,
        capabilities: &[String],
        now: i64,
    ) -> Result<(), String> {
        let caps = serde_json::to_string(capabilities).unwrap_or_else(|_| "[]".into());
        self.conn
            .execute(
                "INSERT INTO agent_heartbeat
                     (agent_id, host, version, pid, last_beat, source_file,
                      observed_state, capabilities)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(agent_id) DO UPDATE SET
                     host = excluded.host, version = excluded.version,
                     pid = excluded.pid, last_beat = excluded.last_beat,
                     source_file = excluded.source_file,
                     observed_state = excluded.observed_state,
                     capabilities = excluded.capabilities",
                params![agent_id, host, version, pid, last_beat, source_file, state, caps],
            )
            .map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "UPDATE agents SET last_seen = ?2 WHERE agent_id = ?1",
                params![agent_id, now],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn upsert_usage_row(
        &self,
        agent_id: &str,
        capability_id: &str,
        asset_id: &str,
        source: &str,
        now: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO agent_capability_usage
                     (agent_id, capability_id, asset_id, source, call_count,
                      first_used_at, last_used_at, last_error)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5, '')
                 ON CONFLICT(agent_id, capability_id) DO UPDATE SET
                     asset_id     = excluded.asset_id,
                     source       = excluded.source,
                     last_used_at = excluded.last_used_at",
                params![agent_id, capability_id, asset_id, source, now],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn heartbeat_of(&self, agent_id: &str, now: i64) -> Result<Option<HeartbeatState>, String> {
        let row: Option<(String, String, Option<i64>, i64, String, String, String)> = self
            .conn
            .query_row(
                "SELECT host, version, pid, last_beat, source_file,
                        observed_state, capabilities
                 FROM agent_heartbeat WHERE agent_id = ?1",
                params![agent_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((host, version, pid, last_beat, source_file, _obs, caps)) = row else {
            return Ok(None);
        };
        let capabilities: Vec<String> = serde_json::from_str(&caps).unwrap_or_default();
        // 库里存的 observed_state 是**写入那一刻**的判定。
        // 读的时候重算一遍，否则放久了的"在线"会一直显示在线。
        let age_ms = (now - last_beat).max(0) * 1000;
        Ok(Some(HeartbeatState {
            host,
            version,
            pid,
            last_beat,
            age_ms,
            source_file,
            state: observe_state(age_ms).to_string(),
            capabilities,
        }))
    }

    fn usage_summary(&self, agent_id: &str) -> Result<(u32, i64, String), String> {
        let row: Option<(i64, i64, String)> = self
            .conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(call_count), 0),
                        COALESCE(MAX(last_error), '')
                 FROM agent_capability_usage WHERE agent_id = ?1",
                params![agent_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(match row {
            Some((n, calls, err)) => (n as u32, calls, err),
            None => (0, 0, String::new()),
        })
    }

    fn count_scalar(&self, sql: &str) -> Result<i64, String> {
        self.conn
            .query_row(sql, [], |r| r.get(0))
            .map_err(|e| e.to_string())
    }
}

/// 把字符串安全地塞进 JSON 字面量（避免手拼 meta 时引号炸掉）。
fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// 列出库里所有表名。迁移测试用它确认三张新表确实建出来了。
pub fn table_names(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .map_err(|e| e.to_string())?;
    let it = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in it {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Registry;
    use crate::testutil::Scratch;

    /// 造一条带指定源路径的资产。
    fn asset(id: &str, rel: &str, caps: &[&str]) -> ac_core::CapabilityManifest {
        let mut m = ac_core::CapabilityManifest::new(id, ac_core::AssetKind::Skill, id);
        m.provenance.source_path = Some(rel.into());
        m.provenance.source_format = Some("skill_md".into());
        for c in caps {
            m.provides.push(ac_core::ProvidedCapability {
                capability_id: format!("{id}::{c}"),
                name: (*c).into(),
                summary: String::new(),
                input_schema: None,
                tags: vec!["read_only".into()],
            });
        }
        m.provenance.content_hash = Some(format!("hash-{id}"));
        m
    }

    // ── 推断规则 ──────────────────────────────────────────────────

    #[test]
    fn dir_marker_beats_package_name() {
        // zion-nocode 包里带 .qoder-plugin 目录：目录标记是插件作者自己写的，
        // 证据强过包名里的 zion。把顺序反过来这条立刻红。
        let g = infer_ecosystem("..\\vendor\\skill\\zion-nocode\\2.7.5\\.qoder-plugin\\plugin.json");
        assert_eq!(g.ecosystem, "qoder");
        assert_eq!(g.confidence, "high");
    }

    #[test]
    fn package_name_used_when_no_dir_marker() {
        let g = infer_ecosystem("..\\vendor\\mcp\\matlab-mcp-server\\.mcp.json");
        assert_eq!(g.ecosystem, "matlab");
        assert_eq!(g.confidence, "medium");
        let g2 = infer_ecosystem("..\\vendor\\plugin\\dsh-blender-plugin\\SKILL.md");
        assert_eq!(g2.ecosystem, "dsh");
        let g3 = infer_ecosystem("..\\vendor\\plugin\\obsidian-suite\\x.json");
        assert_eq!(g3.ecosystem, "obsidian");
    }

    #[test]
    fn unknown_package_stays_unassigned() {
        // 关键：不能瞎猜。库里大量资产确实没有宿主标记，
        // 编一个生态出来会让"跨宿主分布"整个失真。
        let g = infer_ecosystem("..\\vendor\\skill\\product-design\\0.5.10\\SKILL.md");
        assert_eq!(g.ecosystem, ECOSYSTEM_UNASSIGNED);
        assert_eq!(g.confidence, "none");
    }

    #[test]
    fn forward_slash_and_backslash_agree() {
        let a = infer_ecosystem("vendor/skill/x/.claude-plugin/plugin.json");
        let b = infer_ecosystem("vendor\\skill\\x\\.claude-plugin\\plugin.json");
        assert_eq!(a.ecosystem, "claude");
        assert_eq!(a.ecosystem, b.ecosystem);
    }

    // ── 在线状态三档 ──────────────────────────────────────────────

    #[test]
    fn observe_state_has_three_distinct_bands() {
        assert_eq!(observe_state(0), "online");
        assert_eq!(observe_state(ONLINE_WINDOW_MS), "online");
        assert_eq!(observe_state(ONLINE_WINDOW_MS + 1), "stale");
        assert_eq!(observe_state(STALE_WINDOW_MS), "stale");
        assert_eq!(observe_state(STALE_WINDOW_MS + 1), "offline");
    }

    // ── C 通道 ────────────────────────────────────────────────────

    #[test]
    fn static_registration_maps_capabilities_to_ecosystems() {
        let r = Registry::open_in_memory().unwrap();
        r.upsert_all(&[
            asset("skill::a", "..\\vendor\\skill\\x\\.qoder-plugin\\plugin.json", &["one"]),
            asset("skill::b", "..\\vendor\\mcp\\matlab-mcp-server\\.mcp.json", &["two", "three"]),
            asset("skill::c", "..\\vendor\\skill\\product-design\\0.5.10\\SKILL.md", &["four"]),
        ])
        .unwrap();

        let rep = r.register_static_agents().unwrap();
        assert_eq!(rep.ecosystems, 2, "qoder + matlab");
        // 4 项能力里 3 项有宿主归属，第 4 项属于 unassigned，不写行。
        assert_eq!(rep.capability_rows, 3);
        assert_eq!(rep.unassigned_assets, 1);

        let usage = r.capability_usage(100).unwrap();
        assert_eq!(usage.len(), 3);
        for u in &usage {
            assert_eq!(u.source, TRANSPORT_STATIC);
            // 推断出来的归属，调用次数必须是 0 —— 不能假装成实测。
            assert_eq!(u.call_count, 0, "{} 不该有调用次数", u.agent_id);
        }
    }

    #[test]
    fn static_registration_is_idempotent() {
        let r = Registry::open_in_memory().unwrap();
        r.upsert_all(&[asset(
            "skill::a",
            "..\\vendor\\skill\\x\\.qoder-plugin\\plugin.json",
            &["one", "two"],
        )])
        .unwrap();
        let first = r.register_static_agents().unwrap();
        let second = r.register_static_agents().unwrap();
        assert_eq!(first.capability_rows, second.capability_rows);
        let n: i64 = r
            .conn
            .query_row("SELECT COUNT(*) FROM agent_capability_usage", [], |x| x.get(0))
            .unwrap();
        assert_eq!(n, 2, "重复登记不能产生重复行");
    }

    #[test]
    fn overview_reports_zero_capability_assets_separately() {
        let r = Registry::open_in_memory().unwrap();
        r.upsert_all(&[
            asset("skill::a", "..\\vendor\\skill\\x\\.qoder-plugin\\p.json", &["one"]),
            asset("skill::b", "..\\vendor\\skill\\y\\SKILL.md", &[]),
        ])
        .unwrap();
        r.register_static_agents().unwrap();
        let ov = r.agent_overview().unwrap();
        assert_eq!(ov.total_assets, 2);
        assert_eq!(ov.assets_with_capabilities, 1);
        assert_eq!(ov.zero_capability_assets, 1);
        assert_eq!(ov.static_agents, 1);
        assert_eq!(ov.total_capabilities, 1);
    }

    // ── B 通道 ────────────────────────────────────────────────────

    #[test]
    fn missing_heartbeat_dir_reports_no_data_not_error() {
        let r = Registry::open_in_memory().unwrap();
        let s = Scratch::new("hb-missing");
        let rep = r.scan_heartbeat_dir(&s.path().join("nope")).unwrap();
        assert!(!rep.dir_exists);
        assert_eq!(rep.files_seen, 0);
        assert_eq!(rep.hosts_registered, 0);
        assert!(rep.rejected.is_empty());
    }

    #[test]
    fn heartbeat_file_registers_host_as_online() {
        let r = Registry::open_in_memory().unwrap();
        let s = Scratch::new("hb-online");
        let dir = s.mkdir("heartbeat");
        std::fs::write(
            dir.join("host-a.json"),
            format!(
                r#"{{"host":"host-a","version":"1.2.3","pid":4242,"last_heartbeat":{},"capabilities":["cap::x"]}}"#,
                now_unix()
            ),
        )
        .unwrap();
        let rep = r.scan_heartbeat_dir(&dir).unwrap();
        assert!(rep.dir_exists);
        assert_eq!(rep.files_seen, 1);
        assert_eq!(rep.hosts_registered, 1);
        assert!(rep.rejected.is_empty());

        let agents = r.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].transport, TRANSPORT_HEARTBEAT);
        assert_eq!(agents[0].display_name, "host-a");
        let hb = agents[0].heartbeat.clone().expect("心跳通道必须有 heartbeat");
        assert_eq!(hb.state, "online");
        assert_eq!(hb.pid, Some(4242));
        assert_eq!(hb.version, "1.2.3");
        assert_eq!(hb.capabilities, vec!["cap::x".to_string()]);
    }

    #[test]
    fn old_heartbeat_becomes_offline_not_unknown() {
        let r = Registry::open_in_memory().unwrap();
        let s = Scratch::new("hb-old");
        let dir = s.mkdir("heartbeat");
        std::fs::write(
            dir.join("old.json"),
            format!(r#"{{"host":"old","last_heartbeat":{}}}"#, now_unix() - 100_000),
        )
        .unwrap();
        r.scan_heartbeat_dir(&dir).unwrap();
        let a = r.list_agents().unwrap();
        assert_eq!(a[0].heartbeat.as_ref().unwrap().state, "offline");
    }

    #[test]
    fn broken_heartbeat_file_is_reported_not_swallowed() {
        let r = Registry::open_in_memory().unwrap();
        let s = Scratch::new("hb-bad");
        let dir = s.mkdir("heartbeat");
        std::fs::write(dir.join("bad.json"), b"{ not json").unwrap();
        std::fs::write(dir.join("notes.txt"), b"ignored").unwrap();
        let rep = r.scan_heartbeat_dir(&dir).unwrap();
        // .txt 不该被当成心跳文件。
        assert_eq!(rep.files_seen, 1);
        assert_eq!(rep.hosts_registered, 0);
        assert_eq!(rep.rejected.len(), 1);
        assert!(rep.rejected[0].reason.contains("JSON 解析失败"));
        assert!(r.list_agents().unwrap().is_empty(), "坏文件不该造出智能体");
    }

    #[test]
    fn heartbeat_millisecond_epoch_is_normalized() {
        let r = Registry::open_in_memory().unwrap();
        let s = Scratch::new("hb-ms");
        let dir = s.mkdir("heartbeat");
        std::fs::write(
            dir.join("ms.json"),
            format!(r#"{{"host":"ms","last_heartbeat":{}}}"#, now_unix() * 1000),
        )
        .unwrap();
        r.scan_heartbeat_dir(&dir).unwrap();
        let a = r.list_agents().unwrap();
        // 毫秒没归一化的话 age 会是 0 之外的巨大值，状态会误判成 offline。
        assert_eq!(a[0].heartbeat.as_ref().unwrap().state, "online");
    }

    // ── A 通道 ────────────────────────────────────────────────────

    #[test]
    fn report_from_unregistered_agent_is_rejected() {
        // 这条是 A 通道最关键的不变量：没有注册就没有数据。
        // 放松它（顺手 INSERT 一行 agents）会让总览凭空多出智能体。
        let r = Registry::open_in_memory().unwrap();
        let err = r
            .report_capability_usage("ghost-agent", "cap::x", true, None)
            .unwrap_err();
        assert!(err.contains("未注册"), "{err}");
        assert!(r.list_agents().unwrap().is_empty());
    }

    #[test]
    fn mcp_registration_and_usage_accumulate() {
        let r = Registry::open_in_memory().unwrap();
        let id = r
            .register_agent(&AgentRegistration {
                agent_id: "peer-1".into(),
                display_name: "对端".into(),
                ecosystem: "Qoder".into(),
                kind: "host".into(),
                endpoint: "ipc://x".into(),
                meta: "{}".into(),
            })
            .unwrap();
        assert_eq!(id, "peer-1");
        assert_eq!(r.report_capability_usage("peer-1", "cap::x", true, None).unwrap(), 1);
        assert_eq!(r.report_capability_usage("peer-1", "cap::x", true, None).unwrap(), 2);
        assert_eq!(r.report_capability_usage("peer-1", "cap::x", false, Some("超时")).unwrap(), 3);

        let a = r.list_agents().unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].transport, TRANSPORT_MCP);
        assert_eq!(a[0].ecosystem, "qoder", "生态键应归一化为小写");
        assert_eq!(a[0].call_total, 3);
        assert_eq!(a[0].last_error, "超时");

        let ov = r.agent_overview().unwrap();
        assert_eq!(ov.mcp_agents, 1);
        assert_eq!(ov.online, 0);
        // MCP 通道没有心跳，必须落在 unknown 而不是被算成在线。
        assert_eq!(ov.unknown, 1);
    }

    /// 同一个智能体的 A 通道（mcp 注册）与 B 通道（心跳）必须合并成一条。
    ///
    /// 这条测试钉的是用户实际看到的现象：早先心跳行的 agent_id 带
    /// `heartbeat:` 前缀，于是同一个 DSH 在界面里是两个节点，而「在线」
    /// 落在不带前缀那条之外 → 永远显示 0 在线。
    #[test]
    fn heartbeat_and_registration_merge_into_one_identity() {
        let r = Registry::open_in_memory().unwrap();
        // A 通道（mcp 注册）
        r.register_agent(&AgentRegistration {
            agent_id: "deepseek-harness".into(),
            display_name: "DeepSeek Harness".into(),
            ecosystem: "dsh".into(),
            kind: "agent".into(),
            endpoint: "http://127.0.0.1:18198".into(),
            meta: "{}".into(),
        })
        .unwrap();
        // B 通道（历史格式：带前缀的心跳行）
        let now = now_unix();
        r.upsert_agent_row(
            "heartbeat:deepseek-harness",
            "deepseek-harness",
            TRANSPORT_HEARTBEAT,
            "/data/heartbeat/deepseek-harness.json",
            "{}",
            now,
        )
        .unwrap();
        r.upsert_heartbeat_row(
            "heartbeat:deepseek-harness",
            "deepseek-harness",
            "0.1.0",
            Some(1234),
            now,
            "/data/heartbeat/deepseek-harness.json",
            "online",
            &[],
            now,
        )
        .unwrap();

        let agents = r.list_agents().unwrap();
        assert_eq!(
            agents.len(),
            1,
            "同一智能体的两条通道必须合并；实际 {:?}",
            agents.iter().map(|a| a.agent_id.clone()).collect::<Vec<_>>()
        );
        let a = &agents[0];
        assert_eq!(a.agent_id, "deepseek-harness");
        assert_eq!(a.transport, TRANSPORT_HEARTBEAT, "心跳优先作为主通道");
        assert_eq!(a.channels, vec!["heartbeat".to_string(), "mcp".to_string()]);
        assert!(
            a.heartbeat.as_ref().map(|h| h.state.as_str()) == Some("online"),
            "合并后必须带上心跳状态，否则界面在线数还是 0"
        );
        assert_eq!(a.ecosystem, "dsh", "生态要取非 unassigned 的那个");
    }

    /// 心跳文档的身份优先级：id > agent > host > 文件名。
    #[test]
    fn heartbeat_identity_prefers_id_over_hostname() {
        // 插件真实写法：id/agent 是身份，host 是机器名
        let doc = HeartbeatDoc {
            host: "WIN-MNI8FILL2II".into(),
            id: "deepseek-harness".into(),
            ..Default::default()
        };
        assert_eq!(doc.identity(None).as_deref(), Some("deepseek-harness"));

        // 只有 agent 字段
        let doc = HeartbeatDoc {
            host: "WIN-MNI8FILL2II".into(),
            agent: "qoder".into(),
            ..Default::default()
        };
        assert_eq!(doc.identity(None).as_deref(), Some("qoder"));

        // 老格式只有 host
        let doc = HeartbeatDoc {
            host: "legacy-host".into(),
            ..Default::default()
        };
        assert_eq!(doc.identity(None).as_deref(), Some("legacy-host"));

        // 全空时退回文件名；再没有就是 None
        let doc = HeartbeatDoc::default();
        assert_eq!(doc.identity(Some("from-file")).as_deref(), Some("from-file"));
        assert_eq!(doc.identity(None), None);
    }

    /// `last_beat` 与 `last_heartbeat` 两种写法都要认。
    #[test]
    fn heartbeat_beat_at_accepts_both_field_names() {
        let a = HeartbeatDoc { last_heartbeat: Some(111), ..Default::default() };
        assert_eq!(a.beat_at(), Some(111));
        let b = HeartbeatDoc { last_beat: Some(222), ..Default::default() };
        assert_eq!(b.beat_at(), Some(222));
        // 两个都有时 last_heartbeat 优先
        let c = HeartbeatDoc { last_heartbeat: Some(1), last_beat: Some(2), ..Default::default() };
        assert_eq!(c.beat_at(), Some(1));
        assert_eq!(HeartbeatDoc::default().beat_at(), None);
    }

    #[test]
    fn empty_agent_id_is_rejected() {
        let r = Registry::open_in_memory().unwrap();
        let err = r
            .register_agent(&AgentRegistration {
                agent_id: "   ".into(),
                display_name: String::new(),
                ecosystem: String::new(),
                kind: String::new(),
                endpoint: String::new(),
                meta: String::new(),
            })
            .unwrap_err();
        assert!(err.contains("不能为空"), "{err}");
    }
}
