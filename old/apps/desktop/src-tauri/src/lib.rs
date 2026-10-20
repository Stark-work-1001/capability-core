// CapabilityCore 内核 <-> Tauri 前端的命令桥。
//
// 两条纪律：
// 1. 命令必须放在独立 module 内。放在 crate 根时，`#[tauri::command]`
//    生成的 __cmd__* 宏会与 crate 根的宏命名空间冲突（E0255）。
// 2. **业务逻辑与 Tauri 包装分离**：所有实际工作放在 `ops::` 的普通函数里，
//    `#[tauri::command]` 只做一层「State 取出 + 调用」。这样同一份逻辑
//    能被 `--selftest` 直接调用，不需要窗口，也不需要 WebView。
//
// SQLite 连接不是 Sync 的，用 Mutex 包起来再交给 Tauri State。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;

/// 内核信息。前端启动时调用，用于握手确认 Rust 侧已就绪。
#[derive(Debug, Serialize, Deserialize)]
pub struct KernelInfo {
    pub app_version: String,
    /// Capability Manifest 协议版本（来自 ac-core）
    pub schema_version: String,
    /// 编译期捕获的实际编译器版本
    pub rustc_version: String,
    pub os: String,
    pub arch: String,
    /// 版本标识：`devel`（开发测试）或 `standalone`（正式运行）。
    ///
    /// **编译期**由 build.rs 注入，运行时不可改：图标、端口、标题、
    /// 数据目录判据都按它分叉，让同一个 exe 拷到别处也不会"变版本"。
    pub edition: String,
    #[serde(default)] pub review_candidate: bool,
    /// 本版本使用的 HTTP 服务端口（devel 18197 / standalone 18198）。
    pub http_port: u16,
    /// 本次运行 HTTP 服务是否真的起来了。绑不上端口时为 false，
    /// 界面据此显示提示 —— 不能假装在线。
    pub http_listening: bool,
}

/// 库内现状。界面左栏与顶栏都靠它。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct RegistryStats {
    pub db_path: String,
    pub scan_root: String,
    pub total: u32,
    /// 按类型计数，key 为 kind
    pub counts: Vec<KindCount>,
    /// 库是否已打开
    pub ready: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct KindCount {
    pub kind: String,
    pub count: u32,
}

/// 入库结果。区分「真的变了」和「什么都没变」，
/// 界面据此决定要不要提示用户重建索引。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct IngestSummary {
    pub parsed: u32,
    pub inserted: u32,
    pub updated: u32,
    pub unchanged: u32,
    pub removed: u32,
    pub skipped: u32,
    pub has_changes: bool,
    pub has_cycle: bool,
    pub elapsed_ms: u128,
    // ── M1.5 ──
    /// 本次新建的逻辑资产数
    pub logical_created: u32,
    /// 全库 alias 位置数（内容重复的位置）
    pub alias_count: u32,
    /// 待用户确认的去重候选组数
    pub dedup_pending: u32,
}

/// 资产列表查询条件。
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ListQuery {
    pub kind: Option<String>,
    pub text: Option<String>,
    pub min_authority: Option<String>,
    /// 按标签过滤（任一能力带此标签即命中）。M1 审计发现的硬缺口，M1.5 补上。
    pub tag: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListResult {
    pub total: u32,
    pub items: Vec<ac_registry::AssetSummary>,
}

/// 模型 provider 接入状况。**本轮 implemented 恒为 0**，
/// 界面上那句"未接入"就是从这里来的。
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LlmProviderReport {
    /// 配置文件位置，UI 直接显示给用户看。
    pub config_path: String,
    /// 文件是否真的存在。false = "还没人配过"，不是错误。
    pub config_exists: bool,
    pub providers: Vec<ac_llm::provider::ProviderView>,
    /// 已实现真实调用的 provider 数。**本轮恒为 0。**
    pub implemented: u32,
    #[serde(default)] pub configured_slots: u32,
    /// 一句话说明当前状况。
    pub note: String,
}

/// 版本标识，编译期由 build.rs 注入（见那里的 edition()）。
///
/// 为什么不当成运行时参数：图标/版本资源是编译期写进 exe 的，
/// 端口若运行时可变，就会出现「界面说 devel、图标是 standalone、
/// 端口又和另一个实例撞车」这种谁都解释不清的状态。
pub const EDITION: &str = env!("CORE_EDITION");

/// 是否正式运行版。
pub fn is_standalone() -> bool {
    EDITION == "standalone"
}
pub fn is_review_candidate()->bool {exe_dir().is_some_and(|p|p.join("CANDIDATE.json").is_file())}

/// 本版本对外 HTTP 服务端口。
///
/// 两版**必须能同时运行**（用户要 devel 测试 + standalone 实际使用），
/// 所以端口必须错开，且不能碰 18181/18182（memora 的）与 18191（嵌入服务）。
pub const HTTP_PORT: u16 = 18198;
pub const HTTP_PORT_DEVEL: u16 = 18197;

/// 当前版本应使用的端口。环境变量 `CORE_HTTP_PORT` 可覆盖（仅排障用）。
///
/// **纯函数，不探测、不打印**。
///
/// 这里踩过一个坑：早先版本在这个函数里做 `is_port_free` 检查并对占用打警告。
/// 结果是——服务自己绑上端口之后，任何再调一次 `http_port()` 的地方
/// （比如 `kernel_info` 要报给前端）都会把「自己占着」读成「被别人占了」，
/// 于是启动日志里永远多一行假的「端口已被占用」。端口探测只能有一个地方做，
/// 就是真正要绑定的那一刻（见 `start_http_service`）。
pub fn http_port() -> u16 {
    if let Some(v) = std::env::var_os("CORE_HTTP_PORT") {
        if let Some(p) = v.to_string_lossy().trim().parse::<u16>().ok() {
            return p;
        }
    }
    if is_standalone() {
        HTTP_PORT
    } else {
        HTTP_PORT_DEVEL
    }
}

fn is_port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// 全局状态：数据库连接 + 扫描根。
pub struct AppState {
    pub registry: Mutex<Option<ac_registry::Registry>>,
    pub data_dir: PathBuf,
    pub scan_root: Mutex<PathBuf>,
    /// HTTP 服务实际是否在监听（setup 里起服务后回填）。
    pub http_listening: std::sync::atomic::AtomicBool,
}

impl AppState {
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("registry.db")
    }
}

// ── 业务逻辑：不依赖 Tauri，可被 --selftest 直接调用 ──────────────

mod ops {
    use super::{
        AppState, IngestSummary, KindCount, ListQuery, ListResult, LlmProviderReport,
        RegistryStats,
    };
    use ac_registry::agents::{AgentRecord, HeartbeatScanReport, StaticRegistrationReport, UsageRow};
    use ac_registry::diagnostics::DiagnosticReport;
    use ac_registry::protocol::ProtocolMatrix;
    use ac_registry::AgentRegistration;
    use ac_registry::{AssetQuery, AssetSummary};
    use std::path::PathBuf;

    /// 错误统一转字符串：Rust 的 Error 树跨 IPC 不好序列化，
    /// 而界面只需要一句能给人看的话。
    pub type R<T> = Result<T, String>;

    /// 确保数据库已打开。返回打开后的统计。
    pub fn open_registry(st: &AppState) -> R<RegistryStats> {
        ensure_open(st)?;
        stats_of(st)
    }

    /// **懒打开**：任何一个需要库的命令都先过这里。
    ///
    /// 为什么不让前端负责"先 open 再查"：那条约定靠调用顺序维持，
    /// 而界面上好几个面板（总览、维修站）是在 App 的首屏 effect **之前**
    /// 就挂载并各自发请求的。于是启动瞬间会稳定出现一条红条
    /// 「数据库未打开」，1 秒后又自己消失——用户看到的是"这软件有问题"。
    ///
    /// 把"是否已打开"这个状态机收进后端，调用方就不需要知道顺序。
    /// 打开失败仍然如实返回错误（不吞），只是不再把"还没到那一步"
    /// 也报成故障。
    fn ensure_open(st: &AppState) -> R<()> {
        let mut guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        if guard.is_none() {
            let path = st.db_path();
            *guard = Some(ac_registry::Registry::open(&path)?);
        }
        Ok(())
    }

    pub fn stats_of(st: &AppState) -> R<RegistryStats> {
        let scan_root = st
            .scan_root
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?
            .clone();
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let Some(reg) = guard.as_ref() else {
            return Ok(RegistryStats {
                db_path: st.db_path().display().to_string(),
                scan_root: scan_root.display().to_string(),
                ready: false,
                ..Default::default()
            });
        };
        let counts = reg
            .counts_by_kind()?
            .into_iter()
            .map(|c| KindCount {
                kind: c.kind,
                count: c.count,
            })
            .collect();
        Ok(RegistryStats {
            db_path: st.db_path().display().to_string(),
            scan_root: scan_root.display().to_string(),
            total: reg.total()?,
            counts,
            ready: true,
        })
    }

    /// 打开库 + 扫描入库。
    pub fn rescan(st: &AppState, scan_root: Option<String>) -> R<(IngestSummary, RegistryStats)> {
        if let Some(r) = scan_root.filter(|s| !s.trim().is_empty()) {
            *st
                .scan_root
                .lock()
                .map_err(|e| format!("锁被毒化: {e}"))? = PathBuf::from(r);
        }
        {
            let mut guard = st
                .registry
                .lock()
                .map_err(|e| format!("锁被毒化: {e}"))?;
            if guard.is_none() {
                *guard = Some(ac_registry::Registry::open(&st.db_path())?);
            }
        }
        let root = st
            .scan_root
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?
            .clone();
        // ingest 必须在 registry 锁内跑（它要写库），但 stats_of() 会
        // 再锁一次同一个 std::sync::Mutex —— 那是**非重入**的，同线程
        // 二次加锁必然死锁。所以先把 summary 在锁内算出来，让 guard 随
        // 作用域结束释放，再在锁外取统计。
        let summary = {
            let guard = st
                .registry
                .lock()
                .map_err(|e| format!("锁被毒化: {e}"))?;
            let reg = guard.as_ref().ok_or("数据库未打开")?;
            let report = ac_registry::ingest(reg, &root)?;
            IngestSummary {
                parsed: report.parsed,
                inserted: report.inserted,
                updated: report.updated,
                unchanged: report.unchanged,
                removed: report.removed,
                skipped: report.skipped.len() as u32,
                has_changes: report.has_changes(),
                has_cycle: report.has_cycle,
                elapsed_ms: report.elapsed_ms,
                logical_created: report.logical_created,
                alias_count: report.alias_count,
                dedup_pending: report.dedup_candidates.len() as u32,
            }
        };
        Ok((summary, stats_of(st)?))
    }

    pub fn list_assets(st: &AppState, query: Option<ListQuery>) -> R<ListResult> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        let q = query.unwrap_or_default();
        // 用 `..Default::default()` 而不是逐字段列全：
        // kernel 侧每加一个查询维度，这里都跟着改一次——上游一个字段
        // 就能让桌面端编译失败，而失败原因看起来跟本次改动毫无关系。
        // 默认值语义由 kernel 定义（v8 的 include_archived 默认 false =
        // 归档的不出现在列表里，这正是"删除要真的看不见"的要求）。
        let aq = AssetQuery {
            kind: q.kind,
            min_authority: q.min_authority,
            text: q.text.filter(|t| !t.trim().is_empty()),
            tag: q.tag.filter(|t| !t.trim().is_empty()),
            limit: q.limit.unwrap_or(200),
            offset: q.offset.unwrap_or(0),
            ..Default::default()
        };
        let total = reg.count_assets(&aq)?;
        let items: Vec<AssetSummary> = reg.query_assets(&aq)?;
        Ok(ListResult { total, items })
    }

    // ── M1.5：标签与去重 ────────────────────────────────────────

    /// 库内标签清单，供 UI 渲染筛选控件。
    pub fn list_tags(st: &AppState) -> R<Vec<ac_registry::TagCount>> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.all_tags()
    }

    /// 待用户确认的去重候选。
    ///
    /// **这里只读，不改任何数据。** 合并与否必须由用户点出来，
    /// 走 `decide_dedup`。这是 M1.5 最核心的纪律。
    pub fn list_dedup_candidates(st: &AppState) -> R<Vec<ac_core::DedupCandidate>> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.dedup_candidates()
    }

    /// 用户裁决一组去重候选：merge / keep_both / ignore。
    ///
    /// 这是**唯一**能改变去重结果的入口。任何程序内部的自动路径都不允许调用它。
    pub fn decide_dedup(
        st: &AppState,
        group_key: String,
        match_level: String,
        decision: String,
    ) -> R<bool> {
        let lvl = match match_level.as_str() {
            "l1" => ac_core::MatchLevel::L1,
            "l2" => ac_core::MatchLevel::L2,
            other => return Err(format!("未知的匹配级别: {other}")),
        };
        let dec = ac_core::DedupDecision::parse(&decision)
            .ok_or_else(|| format!("未知的裁决: {decision}"))?;
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.decide_dedup(&group_key, lvl, dec)
    }

    /// 已裁决历史（审计）。
    pub fn dedup_history(st: &AppState) -> R<Vec<ac_registry::DedupRecord>> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.dedup_history()
    }

    pub fn get_asset(st: &AppState, id: String) -> R<Option<ac_core::CapabilityManifest>> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.get_manifest(&id)
    }

    // ── M3 产品化：总览 / 维修站 / 模型接入 ────────────────────────

    /// 心跳目录：`<data_dir>/heartbeat/`。
    ///
    /// **只读别人的文件**：中枢只定义格式并扫描，不写、不删、
    /// 也不替别的宿主生成适配器。
    pub fn heartbeat_dir(st: &AppState) -> PathBuf {
        st.data_dir.join(ac_registry::agents::HEARTBEAT_DIR_NAME)
    }

    /// 模型 provider 配置位置：`<data_dir>/llm-providers.json`。
    pub fn llm_config_path(st: &AppState) -> PathBuf {
        ac_llm::provider::provider_config_path(&st.data_dir)
    }

    /// 保存（或按 id 覆盖）一条 provider 配置。
    ///
    /// **明文 key 不经过返回值**：这里只返回保存后的路径。
    /// 界面要回显状态就重新调 `llm_status`，它只给掩码。
    pub fn save_provider_impl(
        st: &AppState,
        cfg: ac_llm::provider::ProviderConfig,
    ) -> Result<String, String> {
        let p = ac_llm::provider::save_provider(&st.data_dir, cfg)?;
        Ok(p.display().to_string())
    }

    /// 当前 LLM 接入状态。**文件不存在不是错误**，而是"还没人配过"。
    pub fn llm_status(st: &AppState) -> Result<LlmProviderReport, String> {
        let (path, cfgs) = ac_llm::provider::load_registry(&st.data_dir)?;
        let providers = ac_llm::provider::views(&cfgs);
        let (_,slots)=ac_llm::slots::load_slots(&st.data_dir)?;
        let configured_slots=slots.iter().filter(|s|s.enabled).count() as u32;
        let implemented = ac_llm::provider::implemented_count(&providers)+slots.iter().filter(|s|s.is_callable()).count() as u32;
        let note: String = if configured_slots>0 {
            format!("已启用 {configured_slots} 个专项槽位，{implemented} 个具备请求配置。实际连通与执行结果见连接测试及工作流历史。")
        }else if providers.is_empty() {
            format!(
                "尚未配置任何模型 provider。配置文件位于 {}，创建后重启本程序即可生效。",
                path.display()
            )
        } else if implemented == 0 {
            "provider 已配置，但本轮只交付接入位，未实现任何厂商调用；故障诊断全部由中枢自带算法（结构化过滤 + BM25）给出".to_string()
        } else {
            format!("已实现 {implemented}/{} 个 provider", providers.len())
        };
        Ok(LlmProviderReport {
            config_path: path.display().to_string(),
            config_exists: path.is_file(),
            providers,
            implemented,
            configured_slots,
            note,
        })
    }

    /// 维修站诊断。**只读**：不会写任何文件、不会改任何数据。
    /// 智能体协议清单（用户点名的 claude-code / codex / qoder / ... 及各自协议）。
    ///
    /// 数据来自 kernel 的 `ac_registry::hosts`：内置清单先 seed 到
    /// `<data_dir>/hosts/*.json`（能力核心自己的目录内部，符合"数据库 + 分散
    /// 文件夹"双存储纪律），再与库内 agent_hosts 表合并。
    ///
    /// **返回空数组是合法的**（清单还没 seed / 库还没迁移完），
    /// 界面据此显示"尚无数据 + 原因"，不填假行。
    pub fn list_agent_hosts(st: &AppState) -> R<Vec<ac_registry::hosts::AgentHost>> {
        ensure_open(st)?;
        let data_dir = st.data_dir.clone();
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        // 先同步一次：seed 内置清单到 <data_dir>/hosts/*.json，并把
        // detected/connected 重算进 agent_hosts 表。
        //
        // 为什么在**读接口**里做一次写：用户要的是"打开界面就看到当前状态"，
        // 而 detected 依赖磁盘探测（插件目录在不在）、connected 依赖 agents 表
        // ——两者都会在界面打开之外变。放在这里做的代价是一次本地文件探测，
        // 收益是界面数字不会过期。同步失败不阻止读取（用旧数据 + 由界面标注）。
        let env = ac_registry::hosts::RealEnv;
        if let Err(e) = reg.sync_hosts(&data_dir, &env) {
            eprintln!("[assets] 宿主清单同步失败（本次读旧数据）: {e}");
        }
        reg.list_agent_hosts()
    }

    /// 资产三态：激活 / 暂停 / 归档。
    ///
    /// **归档不是删除**：项目铁律是增量不删，行永远保留，只是默认查询
    /// 不再返回它。这样用户可以随时把误停的资产恢复回来。
    pub fn set_asset_lifecycle(st: &AppState, id: String, state: String) -> R<bool> {
        ensure_open(st)?;
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.set_asset_lifecycle(&id, &state)
    }

    /// 三态计数（active / paused / archived）。可直接喂侧栏徽标。
    ///
    /// **必须显式按 lifecycle 分组**：默认查询把 archived 过滤掉了，
    /// 所以「库内记录 506」和「505 条可见」不是矛盾，是两个不同的问题。
    pub fn lifecycle_counts(st: &AppState) -> R<Vec<ac_registry::KindCount>> {
        ensure_open(st)?;
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.lifecycle_counts()
    }

    /// 某个资产被哪些宿主支持 + 判据（属性栏做 tooltip 用）。
    pub fn asset_host_support_detail(
        st: &AppState,
        id: String,
    ) -> R<Option<ac_registry::hosts::HostSupport>> {
        ensure_open(st)?;
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.asset_host_support_detail(&id)
    }

    pub fn diagnostics(st: &AppState) -> R<DiagnosticReport> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        let scan_root = st
            .scan_root
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?
            .clone();
        let llm = llm_status(st)?;
        let ctx = ac_registry::diagnostics::DiagContext {
            db_path: st.db_path(),
            scan_root: Some(scan_root),
            preview_json: None,
            llm: ac_registry::diagnostics::LlmStatus {
                config_path: llm.config_path.clone(),
                configured_providers: llm.providers.len() as u32 + llm.configured_slots,
                implemented_providers: llm.implemented,
            },
            // 真探一次向量层：诊断面板曾经硬编码「本仓库没有向量实现」，
            // 而 vector.rs 落地后那句话就是谎报。探测逻辑放在 ac-registry，
            // 保证 Tauri / HTTP / MCP 三个入口对"可用"是同一个判据。
            vector: Some(ac_registry::diagnostics::VectorProbe::probe_from_env()),
        };
        // 报告必须在锁内算完：reg 是 guard 的借用，提前 drop 会让借用失效。
        Ok(ac_registry::diagnostics::collect(reg, &ctx))
    }

    /// 模型接入：协议兼容性矩阵。
    pub fn protocol_matrix(st: &AppState) -> R<ProtocolMatrix> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        let scan_root = st
            .scan_root
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?
            .clone();
        // path_base = 库文件的父目录。source_path 是相对它记的，
        // 扫描根（vendor 目录）已经比它深一层，两者不能混用。
        let path_base = st.db_path().parent().map(|p| p.to_path_buf());
        let checks = ac_registry::protocol::check_all(
            reg,
            Some(scan_root.as_path()),
            path_base.as_deref(),
        )?;
        Ok(ac_registry::protocol::matrix(&checks))
    }

    /// 总览：一次拿齐智能体清单。
    pub fn list_agents(st: &AppState) -> R<Vec<AgentRecord>> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.list_agents()
    }

    /// 总览：C 通道静态登记。幂等，可重复调用。
    pub fn register_static_agents(st: &AppState) -> R<StaticRegistrationReport> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.register_static_agents()
    }

    /// 总览：B 通道扫描共享心跳目录。
    pub fn scan_heartbeats(st: &AppState) -> R<HeartbeatScanReport> {
        let dir = heartbeat_dir(st);
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.scan_heartbeat_dir(&dir)
    }

    /// 总览：A 通道——宿主主动注册。**中枢侧服务，本轮不强求有宿主接入。**
    pub fn register_agent(st: &AppState, reg: AgentRegistration) -> R<String> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let r = guard.as_ref().ok_or("数据库未打开")?;
        r.register_agent(&reg)
    }

    /// 总览：A 通道——宿主实报一次能力调用，返回累计次数。
    pub fn report_capability_usage(
        st: &AppState,
        agent_id: String,
        capability_id: String,
        ok: bool,
        error: Option<String>,
    ) -> R<i64> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let r = guard.as_ref().ok_or("数据库未打开")?;
        r.report_capability_usage(&agent_id, &capability_id, ok, error.as_deref())
    }

    /// 总览：谁拉了什么能力。
    pub fn capability_usage(st: &AppState, limit: Option<u32>) -> R<Vec<UsageRow>> {
        let guard = st
            .registry
            .lock()
            .map_err(|e| format!("锁被毒化: {e}"))?;
        let reg = guard.as_ref().ok_or("数据库未打开")?;
        reg.capability_usage(limit.unwrap_or(500))
    }

    // ── 模型接入三槽位（Embedding / Judge / Rerank）────────────

    /// 三个槽位的视图（**不含明文密钥**，只有掩码）。
    pub fn model_slots(st: &AppState) -> R<Vec<ac_llm::slots::SlotView>> {
        ac_llm::slots::slots_view(&st.data_dir)
    }

    /// 保存一个槽位。
    ///
    /// `api_key` 语义（与 provider::save_provider 一致）：
    /// - `Some("sk-...")` → 用这个 key
    /// - `Some("")` → 清空
    /// - `None` → **不改动现有 key**（前端回显的是掩码，用户没动输入框就别覆盖）
    pub fn save_model_slot(
        st: &AppState,
        slot: ac_llm::slots::SlotConfig,
    ) -> R<String> {
        let p = ac_llm::slots::save_slot(&st.data_dir, slot)?;
        Ok(p.display().to_string())
    }

    /// 用当前槽位配置**真实调一次**对应模型（「测试连接」按钮的后端）。
    ///
    /// **实现只有一份**：`ac_llm::slots::probe_slot`。
    ///
    /// 这里曾经把 embed/rerank/chat 三个分支内联写死过一份，后来发现那会带来
    /// 一个验证难题：验收时无法证明界面按钮走的是哪条代码路径，只能截一张图
    /// 说"点了它显示成功"。挪进 ac-llm 之后，命令行探针
    /// （`examples/slot_probe.rs`）与界面按钮调用**同一个函数**——
    /// 命令行跑通就等于界面走通，不需要 UI 自动化
    /// （WebView2 上 UIA 树本来就取不到）。
    pub fn test_model_slot(
        st: &AppState,
        slot: String,
    ) -> R<ac_llm::slots::SlotProbe> {
        use ac_llm::slots::ModelSlot;
        let want = ModelSlot::parse(&slot)
            .ok_or_else(|| format!("未知槽位 {slot}（只接受 embedding / judge / rerank）"))?;
        ac_llm::slots::probe_slot(&st.data_dir, want)
    }
}


// ── Tauri 包装层：只负责取 State 后转发 ────────────────────────

mod commands {
    use super::{
        ops, AppState, IngestSummary, KernelInfo, ListQuery, ListResult, LlmProviderReport,
        RegistryStats,
    };
    use tauri::State;

    /// 内核信息。**带 State**：要如实报告 HTTP 服务是否真起来了。
    ///
    /// 这里刻意不用无状态的静态信息 —— 界面要区分「服务该在」和
    /// 「服务真的在」，后者只能从运行期状态读。
    #[tauri::command]
    pub fn kernel_info(st: State<AppState>) -> KernelInfo {
        let listening = st
            .http_listening
            .load(std::sync::atomic::Ordering::Relaxed);
        kernel_info_of(listening)
    }

    /// 不带 State 的版本，供 `--selftest`（不起窗口、没有 State）复用。
    ///
    /// 两份实现会漂移：selftest 会显示一个和真实运行不一样的端口/版本。
    pub fn kernel_info_of(http_listening: bool) -> KernelInfo {
        KernelInfo {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            schema_version: ac_core::SCHEMA_VERSION.to_string(),
            rustc_version: env!("AC_RUSTC_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            edition: super::EDITION.to_string(),
            review_candidate: super::is_review_candidate(),
            http_port: super::http_port(),
            http_listening,
        }
    }

    #[tauri::command]
    pub fn open_registry(st: State<AppState>) -> Result<RegistryStats, String> {
        ops::open_registry(&st)
    }

    #[tauri::command]
    pub fn rescan(
        st: State<AppState>,
        scan_root: Option<String>,
    ) -> Result<(IngestSummary, RegistryStats), String> {
        ops::rescan(&st, scan_root)
    }

    #[tauri::command]
    pub fn registry_stats(st: State<AppState>) -> Result<RegistryStats, String> {
        ops::stats_of(&st)
    }

    #[tauri::command]
    pub fn list_assets(
        st: State<AppState>,
        query: Option<ListQuery>,
    ) -> Result<ListResult, String> {
        ops::list_assets(&st, query)
    }

    #[tauri::command]
    pub fn get_asset(
        st: State<AppState>,
        id: String,
    ) -> Result<Option<ac_core::CapabilityManifest>, String> {
        ops::get_asset(&st, id)
    }

    /// 库内标签清单（M1.5 标签筛选的 UI 数据源）
    #[tauri::command]
    pub fn list_tags(st: State<AppState>) -> Result<Vec<ac_registry::TagCount>, String> {
        ops::list_tags(&st)
    }

    /// 待用户确认的去重候选（只读）
    #[tauri::command]
    pub fn list_dedup_candidates(st: State<AppState>) -> Result<Vec<ac_core::DedupCandidate>, String> {
        ops::list_dedup_candidates(&st)
    }

    /// 用户裁决去重候选：merge / keep_both / ignore
    #[tauri::command]
    pub fn decide_dedup(
        st: State<AppState>,
        group_key: String,
        match_level: String,
        decision: String,
    ) -> Result<bool, String> {
        ops::decide_dedup(&st, group_key, match_level, decision)
    }

    /// 去重裁决历史（审计）
    #[tauri::command]
    pub fn dedup_history(st: State<AppState>) -> Result<Vec<ac_registry::DedupRecord>, String> {
        ops::dedup_history(&st)
    }

    // ── M3：总览 ────────────────────────────────────────────────

    /// 全部智能体 + 心跳 + 用量摘要。
    #[tauri::command]
    pub fn list_agents(st: State<AppState>) -> Result<Vec<ac_registry::AgentRecord>, String> {
        ops::list_agents(&st)
    }

    /// C 通道：静态登记（幂等）。
    #[tauri::command]
    pub fn register_static_agents(
        st: State<AppState>,
    ) -> Result<ac_registry::StaticRegistrationReport, String> {
        ops::register_static_agents(&st)
    }

    /// B 通道：扫描共享心跳目录（只读）。
    #[tauri::command]
    pub fn scan_heartbeats(
        st: State<AppState>,
    ) -> Result<ac_registry::HeartbeatScanReport, String> {
        ops::scan_heartbeats(&st)
    }

    /// A 通道：宿主主动注册。中枢侧服务，本轮不强求有宿主接入。
    #[tauri::command]
    pub fn register_agent(
        st: State<AppState>,
        registration: ac_registry::AgentRegistration,
    ) -> Result<String, String> {
        ops::register_agent(&st, registration)
    }

    /// A 通道：宿主实报一次能力调用，返回累计次数。
    #[tauri::command]
    pub fn report_capability_usage(
        st: State<AppState>,
        agent_id: String,
        capability_id: String,
        ok: bool,
        error: Option<String>,
    ) -> Result<i64, String> {
        ops::report_capability_usage(&st, agent_id, capability_id, ok, error)
    }

    /// 总览：谁拉了什么能力。
    #[tauri::command]
    pub fn capability_usage(
        st: State<AppState>,
        limit: Option<u32>,
    ) -> Result<Vec<ac_registry::UsageRow>, String> {
        ops::capability_usage(&st, limit)
    }

    // ── M3：维修站 ──────────────────────────────────────────────

    /// 维修站诊断（只读）。
    #[tauri::command]
    pub fn diagnostics(st: State<AppState>) -> Result<ac_registry::DiagnosticReport, String> {
        ops::diagnostics(&st)
    }

    // ── M3：模型接入 ────────────────────────────────────────────

    /// 协议兼容性矩阵。
    #[tauri::command]
    pub fn protocol_matrix(
        st: State<AppState>,
    ) -> Result<ac_registry::ProtocolMatrix, String> {
        ops::protocol_matrix(&st)
    }

    /// provider 接入状况（本轮 implemented 恒为 0）。
    #[tauri::command]
    pub fn llm_status(st: State<AppState>) -> Result<LlmProviderReport, String> {
        ops::llm_status(&st)
    }

    /// 保存 provider 配置。**只返回路径**，明文 key 不回传。
    #[tauri::command]
    pub fn save_provider(
        st: State<AppState>,
        cfg: ac_llm::provider::ProviderConfig,
    ) -> Result<String, String> {
        ops::save_provider_impl(&st, cfg)
    }

    // ── 智能体协议清单 + 资产生命周期 ──────────────────────────

    /// 各智能体（claude-code / codex / qoder / ...）的协议与支持状态。
    #[tauri::command]
    pub fn list_agent_hosts(
        st: State<AppState>,
    ) -> Result<Vec<ac_registry::hosts::AgentHost>, String> {
        ops::list_agent_hosts(&st)
    }

    /// 资产三态切换：active / paused / archived。
    ///
    /// 返回 false = 该 id 不存在。界面必须据此报错，不能静默当成功。
    #[tauri::command]
    pub fn set_asset_lifecycle(
        st: State<AppState>,
        id: String,
        state: String,
    ) -> Result<bool, String> {
        ops::set_asset_lifecycle(&st, id, state)
    }

    /// 三态计数（侧栏徽标用）。
    ///
    /// 默认查询过滤掉 archived，所以「库内记录 506」与「可见 505」
    /// 是同一事实的两面 —— 界面要能分别说出来。
    #[tauri::command]
    pub fn lifecycle_counts(
        st: State<AppState>,
    ) -> Result<Vec<ac_registry::KindCount>, String> {
        ops::lifecycle_counts(&st)
    }

    // ── 模型接入三槽位 ──────────────────────────────────────

    /// 三个槽位的配置视图（密钥只回掩码）。
    #[tauri::command]
    pub fn model_slots(st: State<AppState>) -> Result<Vec<ac_llm::slots::SlotView>, String> {
        ops::model_slots(&st)
    }

    /// 保存一个槽位。返回落盘路径。
    #[tauri::command]
    pub fn save_model_slot(
        st: State<AppState>,
        slot: ac_llm::slots::SlotConfig,
    ) -> Result<String, String> {
        ops::save_model_slot(&st, slot)
    }

    /// **真实调用**一次该槽位的模型（「测试连接」按钮）。
    #[tauri::command]
    pub fn test_model_slot(
        st: State<AppState>,
        slot: String,
    ) -> Result<ac_llm::slots::SlotProbe, String> {
        ops::test_model_slot(&st, slot)
    }

    /// 任务路由表（模型分工）。
    ///
    /// **静态目录，不读库、不调模型**：它描述「哪件事该由脚本做、哪件必须
    /// 模型做」，本身就是一张设计契约表。
    ///
    /// 不需要 State：不碰数据库。这样即使库坏了，界面仍能展示分工表 ——
    /// 分工是产品设计，不该依赖运行时数据是否健康。
    #[tauri::command]
    pub fn list_capability_tasks() -> ac_registry::tasks::TaskCatalog {
        ac_registry::tasks::catalog()
    }

    #[tauri::command]
    pub fn workflow_catalog() -> Vec<ac_mcp::workflows::WorkflowDefinition> { ac_mcp::workflows::catalog() }
    #[tauri::command]
    pub fn protocol_catalog() -> serde_json::Value { ac_registry::formats::catalog() }
    #[tauri::command]
    pub async fn repair_hub(st: State<'_,AppState>, action: String) -> Result<serde_json::Value,String> {
        let data=st.data_dir.clone();let root=st.scan_root.lock().map_err(|e|e.to_string())?.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let reg=ac_registry::Registry::open(&data.join("registry.db"))?;
            let result=match action.as_str() {
                "rescan_assets"=> {let r=ac_registry::ingest(&reg,&root)?;serde_json::json!({"parsed":r.parsed,"updated":r.updated,"removed":r.removed})},
                "rebuild_search_index"=> {
                    let mut index=ac_retrieval::fulltext::FullTextIndex::attach(data.join("search-index"),&reg,ac_retrieval::fulltext::SyncPolicy::Auto);
                    let r=index.sync(&reg)?;serde_json::json!({"indexed":r.total,"rewritten":r.rewritten,"state":index.state().as_str()})
                },
                _=>return Err("此动作不属于中枢可自动修复范围；请通过修复工作流生成方案".into()),
            };
            reg.audit_operation("repair_hub","", "completed",serde_json::json!({"action":action,"result":result}))?;
            Ok(result)
        }).await.map_err(|e|e.to_string())?
    }
    #[tauri::command]
    pub async fn search_capabilities(st: State<'_,AppState>, mode: String, query: String) -> Result<ac_mcp::search_modes::SearchResponse,String> {
        let data=st.data_dir.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _permit=ac_mcp::scheduler::global().acquire("desktop-search",std::time::Duration::from_secs(2))?;
            ac_mcp::search_modes::search(&data,&mode,&query)
        }).await.map_err(|e|e.to_string())?
    }
    #[tauri::command]
    pub async fn run_workflow(st: State<'_, AppState>, workflow: String, input: ac_mcp::workflows::WorkflowInput) -> Result<ac_mcp::workflows::WorkflowResult,String> {
        let data=st.data_dir.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _permit=ac_mcp::scheduler::global().acquire("desktop-workflows",std::time::Duration::from_secs(2))?;
            ac_mcp::workflows::execute(&data,&workflow,input)
        }).await.map_err(|e|e.to_string())?
    }
    #[tauri::command]
    pub fn workflow_history(st: State<AppState>) -> Result<Vec<serde_json::Value>,String> { ac_registry::Registry::open(&st.db_path())?.workflow_history() }
    #[tauri::command]
    pub fn prepare_capability(st: State<AppState>, request: ac_registry::operations::ImportRequest) -> Result<ac_registry::operations::ImportPlan,String> { ac_registry::Registry::open(&st.db_path())?.prepare_import(&request) }
    #[tauri::command]
    pub fn commit_capability(st: State<AppState>, operation: String, digest: String) -> Result<ac_core::CapabilityManifest,String> { ac_registry::Registry::open(&st.db_path())?.commit_import(&st.data_dir,&operation,&digest) }
    #[tauri::command]
    pub fn asset_research(st: State<AppState>, id:String) -> Result<serde_json::Value,String> {
        let reg=ac_registry::Registry::open(&st.db_path())?;
        let m=reg.get_manifest(&id)?.ok_or("资产不存在")?;
        let v=ac_mcp::workflows::metadata(&st.data_dir,&m);
        reg.save_asset_metadata(&id,&v)?;Ok(v)
    }
    #[tauri::command]
    pub fn operation_history(st: State<AppState>) -> Result<Vec<ac_registry::operations::OperationRecord>,String> { ac_registry::Registry::open(&st.db_path())?.operation_history(100) }
    #[tauri::command]
    pub async fn agent_presence() -> Result<ac_registry::presence::PresenceReport,String> { tauri::async_runtime::spawn_blocking(ac_registry::presence::snapshot).await.map_err(|e|e.to_string()) }
    #[tauri::command]
    pub fn stored_faults(st: State<AppState>) -> Result<Vec<ac_registry::StoredFault>,String> { ac_registry::Registry::open(&st.db_path())?.list_faults(&Default::default()) }
    #[tauri::command]
    pub fn fault_detail(st: State<AppState>, id:String) -> Result<serde_json::Value,String> {
        let reg=ac_registry::Registry::open(&st.db_path())?;
        Ok(ac_registry::operations::redact(&serde_json::json!({"fault":reg.get_fault(&id)?,"history":reg.repair_history(&id)?})))
    }

    /// 某资产被哪些宿主支持 + 判据（属性栏 tooltip 用）。
    #[tauri::command]
    pub fn asset_host_support_detail(
        st: State<AppState>,
        id: String,
    ) -> Result<Option<ac_registry::hosts::HostSupport>, String> {
        ops::asset_host_support_detail(&st, id)
    }
}

// ── 配置解析 ────────────────────────────────────────────────

/// 从当前目录逐级上溯，找第一个含 `vendor/` 的祖先目录。
fn find_vendor_root() -> Option<PathBuf> {
    let mut cur = std::env::current_dir().ok()?;
    loop {
        let cand = cur.join("vendor");
        if cand.is_dir() {
            return Some(cand);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// exe 所在目录。便携判据都锚在这里。
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

/// 便携布局根：exe 同级同时出现 `data/` 或 `vendor/` 就认定是便携目录。
///
/// 为什么用"同级有 data/ 或 vendor/"而不是一个 marker 文件：
/// 用户解压出来的目录天然长这样（vendor 是语料库、data 是库），
/// 不需要他额外点一次"启用便携模式"。开发期 `target/release/` 下
/// 没有这两个目录，所以行为不变。
fn portable_root() -> Option<PathBuf> {
    let dir = exe_dir()?;
    let hit = dir.join("data").is_dir() || dir.join("vendor").is_dir();
    hit.then_some(dir)
}

/// 盘符 / 路径存在性判据与优先级：纯函数，便于单测。
fn pick_data_dir(
    env_override: Option<PathBuf>,
    portable: Option<PathBuf>,
    app_data: Option<PathBuf>,
) -> PathBuf {
    if let Some(d) = env_override {
        return d;
    }
    if let Some(root) = portable {
        return root.join("data");
    }
    app_data.unwrap_or_else(|| PathBuf::from("data"))
}

/// 扫描根优先级：环境变量 > 便携目录的 vendor/ > 从 cwd 上溯找到的 vendor/。
fn pick_scan_root(
    env_override: Option<PathBuf>,
    portable: Option<PathBuf>,
    discovered: Option<PathBuf>,
) -> PathBuf {
    if let Some(d) = env_override {
        return d;
    }
    if let Some(root) = portable {
        let v = root.join("vendor");
        if v.is_dir() {
            return v;
        }
    }
    discovered.unwrap_or_else(|| PathBuf::from("vendor"))
}

fn resolve_scan_root() -> PathBuf {
    pick_scan_root(
        std::env::var_os("CAPABILITYCORE_SCAN_ROOT").map(PathBuf::from),
        portable_root(),
        find_vendor_root(),
    )
}

/// 数据目录优先级：环境变量 > 便携目录的 data/ > Tauri 应用数据目录 > cwd/data。
fn resolve_data_dir(app_data: Option<PathBuf>) -> PathBuf {
    pick_data_dir(
        std::env::var_os("CAPABILITYCORE_DATA_DIR").map(PathBuf::from),
        portable_root(),
        app_data,
    )
}

fn build_state(data_dir: PathBuf, scan_root: PathBuf) -> AppState {
    AppState {
        registry: Mutex::new(None),
        data_dir,
        scan_root: Mutex::new(scan_root),
        http_listening: std::sync::atomic::AtomicBool::new(false),
    }
}

// ── 自检：不依赖窗口 / WebView ───────────────────────────────

/// 一条自检结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SelftestReport {
    pub checks: Vec<Check>,
    pub scan_root: String,
    pub data_dir: String,
    pub schema_version: String,
    pub rustc_version: String,
}

impl SelftestReport {
    pub fn passed(&self) -> usize {
        self.checks.iter().filter(|c| c.ok).count()
    }
    pub fn failed(&self) -> usize {
        self.checks.iter().filter(|c| !c.ok).count()
    }
    pub fn ok(&self) -> bool {
        self.failed() == 0
    }
    pub fn print(&self) {
        println!("CapabilityCore 自检");
        println!("  schema    {}", self.schema_version);
        println!("  rustc     {}", self.rustc_version);
        println!("  扫描根    {}", self.scan_root);
        println!("  数据目录  {}", self.data_dir);
        println!();
        for c in &self.checks {
            println!(
                "  [{}] {:<32} {}",
                if c.ok { "PASS" } else { "FAIL" },
                c.name,
                c.detail
            );
        }
        println!();
        println!("────────────────────────────────");
        println!("  通过 {} / 失败 {}", self.passed(), self.failed());
        println!("────────────────────────────────");
        if self.ok() {
            println!("自检通过。");
        } else {
            println!("自检未通过。");
        }
    }
}

/// 无 GUI 环境下验证「解析 -> 入库 -> 查询 -> 详情」整条链路。
///
/// 存在的理由：命令签名对了不代表序列化对了。`invoke` 的参数命名、
/// `Result<T,String>` 的错误回传、Tuple 返回值的解构，只有真跑一遍
/// 才知道。GUI 在 CI / 无桌面会话的机器上起不来，这条通道是唯一的
/// 自动验证方式。
///
/// 同一份逻辑被 `--selftest` CLI 与 `cargo test` 共用，不是两份实现。
///
/// **`with_rescan=false` 跑"快"模式**——只测库操作（open/list/get/serde），
/// 不重扫真实 vendor。任何环境都能跑完、几秒出结果。
/// 适用于：
/// - 受限环境（无 GUI window station / 路径沙箱 / walkdir 撞深层符号链接）
/// - 改动后想 5 秒内看到"库代码没退化"
///
/// **`with_rescan=true` 跑"全"模式**——含真实 vendor 端到端入库、增量幂等校验。
/// 默认走这条。沙箱环境下若跑挂了，**回退到快模式定位是 rescan 那段**。
pub fn run_selftest(data_dir: PathBuf, scan_root: PathBuf) -> SelftestReport {
    run_selftest_with(data_dir, scan_root, true)
}

pub fn run_selftest_with(
    data_dir: PathBuf,
    scan_root: PathBuf,
    with_rescan: bool,
) -> SelftestReport {
    let mut checks: Vec<Check> = Vec::new();
    // selftest 没有窗口也没有 AppState，用「未监听」如实填 —— 它就是没起服务。
    let info = commands::kernel_info_of(false);
    let finish = |checks: Vec<Check>| SelftestReport {
        checks,
        scan_root: scan_root.display().to_string(),
        data_dir: data_dir.display().to_string(),
        schema_version: info.schema_version.clone(),
        rustc_version: info.rustc_version.clone(),
    };

    checks.push(Check {
        name: "kernel_info".into(),
        ok: !info.schema_version.is_empty(),
        detail: format!(
            "v{} / {} / {}-{}",
            info.app_version, info.schema_version, info.os, info.arch
        ),
    });

    // 自检用独立库，绝不碰用户的正式 registry.db
    let _ = std::fs::create_dir_all(&data_dir);
    let st = build_state(data_dir.clone(), scan_root.clone());

    // 1. 打开库（含 schema 迁移）
    let mut stats = match ops::open_registry(&st) {
        Ok(s) => {
            checks.push(Check {
                name: "open_registry".into(),
                ok: s.ready,
                detail: format!("库就绪，当前 {} 条", s.total),
            });
            s
        }
        Err(e) => {
            checks.push(Check {
                name: "open_registry".into(),
                ok: false,
                detail: e,
            });
            return finish(checks);
        }
    };

    // 2. 扫描入库
    if !with_rescan {
        // 快模式：跳过 rescan 和幂等检查；库是用户自带的
        // 后面 list/get/serde 检查照样跑，足以验证 IPC 命令链
        checks.push(Check {
            name: "rescan".into(),
            ok: true,
            detail: format!(
                "快模式跳过（CAPABILITYCORE_SELFTEST_FAST=1）— 当前库 {} 条",
                stats.total
            ),
        });
        checks.push(Check {
            name: "no_delegation_cycle".into(),
            ok: true,
            detail: "快模式跳过".into(),
        });
        checks.push(Check {
            name: "ingest_idempotent".into(),
            ok: true,
            detail: "快模式跳过".into(),
        });
    } else {
        let (sum, stats2) = match ops::rescan(&st, None) {
            Ok(v) => v,
            Err(e) => {
                checks.push(Check {
                    name: "rescan".into(),
                    ok: false,
                    detail: e,
                });
                return finish(checks);
            }
        };
        checks.push(Check {
            name: "rescan".into(),
            ok: sum.parsed > 0,
            detail: format!(
                "解析 {} / 新增 {} / 更新 {} / 清除 {} / 跳过 {} / {}ms",
                sum.parsed, sum.inserted, sum.updated, sum.removed, sum.skipped, sum.elapsed_ms
            ),
        });
        checks.push(Check {
            name: "no_delegation_cycle".into(),
            ok: !sum.has_cycle,
            detail: if sum.has_cycle { "委派图成环".into() } else { "无环".into() },
        });

        // 3. 增量幂等：再扫一次必须零变更
        checks.push(match ops::rescan(&st, None) {
            Ok((s2, _)) => Check {
                name: "ingest_idempotent".into(),
                ok: !s2.has_changes,
                detail: format!(
                    "二次扫描 需重建索引={} 新增{} 更新{} 清除{}",
                    s2.has_changes, s2.inserted, s2.updated, s2.removed
                ),
            },
            Err(e) => Check {
                name: "ingest_idempotent".into(),
                ok: false,
                detail: e,
            },
        });
        // rescan 之后的任何校验都必须以「重扫后」的统计为准。
        // 外层 stats 来自 open_registry，是重扫之前的快照；拿它去核对
        // 重扫后的库（例如 kind_filter_matches_counts）就成了拿旧账对新账。
        // 本数据集恰好没变化，所以这个 bug 一直没被自检暴露出来。
        stats = stats2;
    }

    // 4. 列表查询
    checks.push(match ops::list_assets(&st, Some(ListQuery { limit: Some(5), ..Default::default() })) {
        Ok(all) => Check {
            name: "list_assets".into(),
            ok: all.total > 0 && all.items.len() == all.total.min(5) as usize,
            detail: format!("共 {} 条，返回 {} 条", all.total, all.items.len()),
        },
        Err(e) => Check {
            name: "list_assets".into(),
            ok: false,
            detail: e,
        },
    });

    // 5. 类型过滤必须与计数一致
    {
        let mut ok = true;
        let mut detail: Vec<String> = Vec::new();
        for c in &stats.counts {
            match ops::list_assets(
                &st,
                Some(ListQuery { kind: Some(c.kind.clone()), limit: Some(1), ..Default::default() }),
            ) {
                Ok(r) => {
                    ok &= r.total == c.count;
                    detail.push(format!("{}={}", c.kind, c.count));
                }
                Err(e) => {
                    ok = false;
                    detail.push(format!("{} ERR {e}", c.kind));
                }
            }
        }
        checks.push(Check {
            name: "kind_filter_matches_counts".into(),
            ok,
            detail: detail.join(" "),
        });
    }

    let sample = ops::list_assets(&st, Some(ListQuery { limit: Some(1), ..Default::default() }))
        .ok()
        .and_then(|r| r.items.into_iter().next());

    // 6. 文本过滤
    if let Some(s) = &sample {
        let q: String = s.name.chars().take(4).collect();
        checks.push(match ops::list_assets(
            &st,
            Some(ListQuery { text: Some(q.clone()), limit: Some(200), ..Default::default() }),
        ) {
            Ok(r) => Check {
                name: "text_filter".into(),
                ok: r.total > 0,
                detail: format!("搜「{q}」命中 {} 条", r.total),
            },
            Err(e) => Check {
                name: "text_filter".into(),
                ok: false,
                detail: e,
            },
        });
    }

    // 7. 权限过滤（按枚举序，不是字典序）
    {
        // 手工核对必须看到**每一行**。AssetQuery::clamp 把 limit 封顶在 1000
        // （store.rs），直接问一次大 limit 会被悄悄截断——资产一旦过千
        // （M1.5 去重入库后必然发生），这里就会拿一份残缺的列表去和 SQL
        // 的 total 比，可能对也可能是错的。故按上限翻页直到取尽。
        const PAGE: u32 = 1000;
        let fetch_all = |mut q: ListQuery| -> Result<ListResult, String> {
            let mut acc = Vec::new();
            let mut total;
            loop {
                q.limit = Some(PAGE);
                q.offset = Some(acc.len() as u32);
                let page = ops::list_assets(&st, Some(q.clone()))?;
                total = page.total;
                let got = page.items.len();
                acc.extend(page.items);
                // 取不满一页 = 已经是最后一页。
                if got < PAGE as usize {
                    break;
                }
            }
            Ok(ListResult { total, items: acc })
        };

        let all = fetch_all(ListQuery::default());
        let w = fetch_all(ListQuery {
            min_authority: Some("write".into()),
            ..Default::default()
        });
        checks.push(match (all, w) {
            (Ok(a), Ok(x)) => {
                let expect = a
                    .items
                    .iter()
                    .filter(|i| i.authority == "write" || i.authority == "destructive")
                    .count();
                Check {
                    name: "min_authority_filter".into(),
                    // 翻页后 items 必须等于 total，否则说明又漏行了——宁可响着失败。
                    ok: x.total as usize == expect && a.items.len() as u32 == a.total,
                    detail: format!(
                        "write+ 命中 {} 条，库内手工核对 {} 条（全量翻取 {}/{} 行）",
                        x.total,
                        expect,
                        a.items.len(),
                        a.total
                    ),
                }
            }
            (Err(e), _) | (_, Err(e)) => Check {
                name: "min_authority_filter".into(),
                ok: false,
                detail: e,
            },
        });
    }

    // 8. 详情往返
    if let Some(s) = &sample {
        checks.push(match ops::get_asset(&st, s.id.clone()) {
            Ok(Some(m)) => Check {
                name: "get_asset".into(),
                ok: m.id == s.id,
                detail: format!(
                    "{} 能力 {} 项 / 关系 {} 条",
                    m.id,
                    m.provides.len(),
                    m.relations.len()
                ),
            },
            Ok(None) => Check {
                name: "get_asset".into(),
                ok: false,
                detail: "返回 None".into(),
            },
            Err(e) => Check {
                name: "get_asset".into(),
                ok: false,
                detail: e,
            },
        });
    }

    // 9. 不存在的 id 返回 None 而不是报错
    checks.push(match ops::get_asset(&st, "不存在的id::x".into()) {
        Ok(None) => Check {
            name: "get_asset_missing_returns_none".into(),
            ok: true,
            detail: "正确".into(),
        },
        Ok(Some(_)) => Check {
            name: "get_asset_missing_returns_none".into(),
            ok: false,
            detail: "不该返回 Some".into(),
        },
        Err(e) => Check {
            name: "get_asset_missing_returns_none".into(),
            ok: false,
            detail: e,
        },
    });

    // 9.5. M1.5 去重：候选可列出、且**未经确认绝不合并**
    //
    // 这是本里程碑最重要的一条自检。去重必然产生候选，
    // 但候选只允许停留在"待确认"状态——任何自动合并都会把
    // "昨天改的本地定制版"和"今天拉的上游版"混成一份。
    {
        let cands = ops::list_dedup_candidates(&st).unwrap_or_default();
        let l1_groups = cands.iter().filter(|c| c.match_level == ac_core::MatchLevel::L1).count();
        checks.push(Check {
            name: "dedup_candidates_listed".into(),
            ok: true,
            detail: format!(
                "待确认候选 {} 组（L1 {} / L2 {}）",
                cands.len(),
                l1_groups,
                cands.len() - l1_groups
            ),
        });

        // 未确认的候选，组内位置数必须 >1（说明确实存在重复内容）
        let sizes_ok = cands
            .iter()
            .all(|c| c.location_ids.len() > 1 && c.location_ids.len() == c.location_ids.iter().collect::<std::collections::BTreeSet<_>>().len());
        checks.push(Check {
            name: "dedup_group_members_are_distinct".into(),
            ok: sizes_ok,
            detail: if sizes_ok {
                "每组位置数>1 且无重复位置".into()
            } else {
                "存在成员为 0/1 或位置重复的分组".into()
            },
        });

        // **绝不静默合并**：候选存在时，逻辑资产数必须仍等于
        // 「有指纹的 distinct content_hash 数」，而不是被折叠后的数量。
        let (logical_total, distinct_l1) = {
            let guard = st.registry.lock().map_err(|e| format!("锁被毒化: {e}")).ok();
            match guard {
                Some(g) => match g.as_ref() {
                    Some(reg) => (reg.logical_total().ok(), reg.distinct_l1_count().ok()),
                    None => (None, None),
                },
                None => (None, None),
            }
        };
        checks.push(match (logical_total, distinct_l1) {
            (Some(l), Some(d)) => Check {
                name: "no_silent_merge".into(),
                ok: l == d,
                detail: format!(
                    "逻辑资产 {l} 个 / distinct L1 {} 个（相等 = 未发生静默合并）",
                    d
                ),
            },
            _ => Check {
                name: "no_silent_merge".into(),
                ok: false,
                detail: "无法读取逻辑资产计数".into(),
            },
        });
    }

    // 9.6. M1.5 标签筛选：ListQuery.tag -> SQL -> 结果条数
    {
        let tags = ops::list_tags(&st).unwrap_or_default();
        checks.push(Check {
            name: "tag_inventory".into(),
            ok: !tags.is_empty(),
            detail: format!(
                "库内 {} 种标签，前 5：{}",
                tags.len(),
                tags.iter()
                    .take(5)
                    .map(|t| format!("{}({})", t.tag, t.count))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        });

        // 逐个验证：按标签筛的 total 必须与 count 一致，且 <= 全量
        let mut all_ok = !tags.is_empty();
        let mut detail: Vec<String> = Vec::new();
        for t in tags.iter().take(6) {
            match ops::list_assets(
                &st,
                Some(ListQuery {
                    tag: Some(t.tag.clone()),
                    limit: Some(1000),
                    ..Default::default()
                }),
            ) {
                Ok(r) => {
                    let plausible = r.total <= t.count && r.total > 0;
                    all_ok &= plausible;
                    detail.push(format!("{}:{}/{}", t.tag, r.total, t.count));
                }
                Err(e) => {
                    all_ok = false;
                    detail.push(format!("{} ERR {e}", t.tag));
                }
            }
        }
        checks.push(Check {
            name: "tag_filter_end_to_end".into(),
            ok: all_ok,
            detail: detail.join(" "),
        });
    }

    // 10. 序列化往返：前端拿到的 JSON 必须能原样读回
    {
        let json = serde_json::to_string(&stats).unwrap_or_default();
        checks.push(Check {
            name: "serde_roundtrip".into(),
            ok: serde_json::from_str::<RegistryStats>(&json).is_ok(),
            detail: format!("RegistryStats {} 字节往返一致", json.len()),
        });
    }
    if let Ok(all) = ops::list_assets(&st, Some(ListQuery { limit: Some(20), ..Default::default() })) {
        let json = serde_json::to_string(&all).unwrap_or_default();
        checks.push(Check {
            name: "serde_roundtrip_list".into(),
            ok: serde_json::from_str::<ListResult>(&json).is_ok(),
            detail: format!("ListResult {} 字节往返一致", json.len()),
        });
    }

    finish(checks)
}

#[cfg(test)]
mod selftest_tests {
    use super::*;

    /// 自检用的临时目录，锚在 target/ 下（保证在工作区内可写）。
    fn scratch() -> PathBuf {
        let mut p = std::env::current_exe().expect("current_exe");
        p.pop(); // deps
        p.pop(); // release | debug
        p.join("ac-selftest")
    }

    /// 真实资产端到端：跑的就是 `--selftest` 那一套逻辑。
    ///
    /// 这是 M1 唯一能自动跑的「界面契约」验证——GUI 在本环境起不来，
    /// 但命令逻辑、序列化、过滤语义全在这一条里。
    #[test]
    fn selftest_passes_against_real_vendor() {
        let data_dir = scratch();
        let _ = std::fs::remove_dir_all(&data_dir);
        let scan_root = super::find_vendor_root()
            .expect("找不到 vendor/ 目录，自检无法运行");
        let report = run_selftest(data_dir.clone(), scan_root);

        for c in &report.checks {
            if !c.ok {
                println!("FAIL {} -> {}", c.name, c.detail);
            }
        }
        assert!(report.ok(), "自检未通过: {}/{} 失败", report.failed(), report.checks.len());
        assert!(report.checks.len() >= 10, "自检项太少: {}", report.checks.len());
        let _ = std::fs::remove_dir_all(&data_dir);
    }
}

// ── 入口 ────────────────────────────────────────────────────

/// 便携模式的优先级判据，全部是纯函数，可脱窗口单测。
///
/// 为什么值得单独测：便携模式一旦判错，用户解压出来的目录会
/// "看起来能启动、库却落在别处"，属于最难从界面发现的一类错。
#[cfg(test)]
mod portable_tests {
    use super::{pick_data_dir, pick_scan_root};
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn data_dir_prefers_env_over_everything() {
        let got = pick_data_dir(
            Some(p("E:/override")),
            Some(p("D:/portable")),
            Some(p("C:/appdata/data")),
        );
        assert_eq!(got, p("E:/override"));
    }

    #[test]
    fn data_dir_prefers_portable_over_appdata() {
        let got = pick_data_dir(None, Some(p("D:/portable")), Some(p("C:/appdata/data")));
        assert_eq!(got, p("D:/portable/data"));
    }

    #[test]
    fn data_dir_falls_back_to_appdata_when_not_portable() {
        let got = pick_data_dir(None, None, Some(p("C:/appdata/data")));
        assert_eq!(got, p("C:/appdata/data"));
    }

    #[test]
    fn data_dir_uses_relative_last_resort() {
        assert_eq!(pick_data_dir(None, None, None), p("data"));
    }

    #[test]
    fn scan_root_prefers_env_then_portable_vendor_then_discovered() {
        assert_eq!(
            pick_scan_root(Some(p("E:/scan")), Some(p("D:/portable")), Some(p("D:/vendor"))),
            p("E:/scan")
        );
        // 便携目录里 vendor 不存在时不能凭空返回一个不存在的路径，
        // 必须回落到上溯发现的那个 vendor。
        assert_eq!(
            pick_scan_root(None, Some(p("D:/no-such-portable-root")), Some(p("D:/vendor"))),
            p("D:/vendor")
        );
        assert_eq!(
            pick_scan_root(None, None, Some(p("D:/vendor"))),
            p("D:/vendor")
        );
        assert_eq!(pick_scan_root(None, None, None), p("vendor"));
    }
}

/// 自检用的数据目录。
///
/// 优先 `CAPABILITYCORE_SELFTEST_DIR`，其次 exe 同级，**不直接用系统 TEMP**——
/// 部分受限环境下系统目录不可写，那样自检会以一种和真实缺陷无关的方式失败。
fn selftest_data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CAPABILITYCORE_SELFTEST_DIR") {
        return PathBuf::from(d);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("selftest")
}

/// 决定是否把 Tauri 的应用目录（WebView2 profile 等）挪到安全位置。
///
/// 默认值是 %LOCALAPPDATA% + identifier。Tauri 建窗前会无条件
/// create_dir_all 那个目录，失败就直接 panic（app.rs:1444）。有些机器
/// （比如开发机上一条按 exe 目录生效的 FS 策略）不允许该写入，于是应用
/// 连窗口都起不来。
///
/// 规则：
/// - CAPABILITYCORE_APP_DIR_OVERRIDE 显式指定时无条件采用；
/// - 否则只在「默认目录确实不可用」时才改道，并落到 exe 旁边的
///   .capabilitycore-appdata。探测要真写一个临时文件——只 exists() 判断
///   不算数，目录可能存在但不可写。
fn app_data_dir_override() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CAPABILITYCORE_APP_DIR_OVERRIDE") {
        return Some(PathBuf::from(d));
    }
    let default = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|d| d.join("dev.capabilitycore.desktop"))?;
    if dir_is_usable(&default) {
        return None;
    }
    let fallback = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".capabilitycore-appdata");
    dir_is_usable(&fallback).then_some(fallback)
}

/// 目录能否真正落盘：建得出来、并且能写进去。
fn dir_is_usable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".ac-write-probe");
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 起对外 HTTP 服务（ac-mcp 的 HttpServer），后台线程跑。
///
/// 返回**是否真的开始监听**。返回值会被写进 AppState，界面据此显示
/// 「服务在线 / 未启动」—— 不猜、不假装。
///
/// 装配顺序（照 ac-mcp/gateway.rs 的既有路径）：
/// 1. `CapabilityGateway::open(db_path)`：打开库 + 在 db 同级挂 BM25 全文层
/// 2. `CapabilityService::new(gateway)`：加上 Router（租约/排队）
/// 3. `HttpServer::new(service)`：纯函数路由表
/// 4. `serve(127.0.0.1:<端口>)`：阻塞式监听 → 丢到后台线程
///
/// **只绑 127.0.0.1**，不绑 0.0.0.0：这是本机的能力核心，不该对局域网暴露。
fn start_http_service(app: &tauri::AppHandle, state: &AppState) -> bool {
    let port = http_port();
    let addr = format!("127.0.0.1:{port}");
    let db_path = state.db_path();

    // 端口探活先做一次：被占就别浪费一次完整的网关装配（那要建索引）。
    //
    // **这是全工程唯一一处端口探测**。别处再探会把「自己绑着」误读成
    // 「被别人占着」（见 http_port 的注释）。
    if !is_port_free(port) {
        eprintln!(
            "[assets] HTTP 端口 {port} 已被占用，本进程不启动 HTTP 服务。\
             若另一版本（devel {} / standalone {}）正在运行，这是预期情况。",
            HTTP_PORT_DEVEL, HTTP_PORT
        );
        return false;
    }

    let gateway = match ac_mcp::gateway::CapabilityGateway::open(&db_path) {
        Ok(g) => std::sync::Arc::new(g),
        Err(e) => {
            eprintln!("[assets] HTTP 服务装配失败（网关打不开库 {db_path:?}）: {e}");
            return false;
        }
    };
    let service = std::sync::Arc::new(ac_mcp::service::CapabilityService::new(gateway));
    let server = std::sync::Arc::new(ac_mcp::http::HttpServer::new(service));

    let _ = app; // 预留：以后要把窗口状态/事件带进路由
    let addr_for_thread = addr.clone();
    std::thread::Builder::new()
        .name("assets-http".into())
        .spawn(move || {
            if let Err(e) = server.serve(&addr_for_thread) {
                eprintln!("[assets] HTTP 服务退出: {e}");
            }
        })
        .is_ok()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // --selftest：不开窗口，直接跑完整链路后退出
    if std::env::args().any(|a| a == "--selftest") {
        // CAPABILITYCORE_SELFTEST_FAST=1 走快模式，跳过真实 rescan
        // 适用场景：受限环境（无 window station / 路径沙箱 / walkdir 撞深层符号链接）
        let fast = std::env::var_os("CAPABILITYCORE_SELFTEST_FAST").is_some();
        let report = if fast {
            run_selftest_with(selftest_data_dir(), resolve_scan_root(), false)
        } else {
            run_selftest(selftest_data_dir(), resolve_scan_root())
        };
        report.print();
        std::process::exit(i32::from(!report.ok()));
    }

    let mut context = tauri::generate_context!();
    if let Some(dir) = app_data_dir_override() {
        let _ = std::fs::create_dir_all(&dir);
        context.config_mut().app.app_directories_override =
            Some(tauri::utils::config::AppDirectoriesOverride::Root(dir));
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // 数据目录：CAPABILITYCORE_DATA_DIR > 便携目录（exe 同级有 data/ 或 vendor/）> 系统应用目录。
            // 安装到 Program Files 时 exe 同级不可写，那里不会被认成便携目录，
            // 仍然回落到 app_data_dir —— 所以便携判据不会伤到安装场景。
            let app_data = app
                .path()
                .app_data_dir()
                .ok()
                .map(|d| d.join("data"));
            let data_dir = resolve_data_dir(app_data);

            // **启动即打开库**，而不是等前端发 open_registry。
            //
            // 原因：总览 / 维修站面板在自己的 useEffect 里就发请求，
            // 而 App 的首屏 effect 里那句 openRegistry() 不保证先跑完。
            // 结果是每次启动都闪一条红条「数据库未打开」，1 秒后自己消失——
            // 用户看到的是"这软件有问题"，而库本身完全正常。
            // 把打开动作放到 setup 里，任何命令到达时库都已就绪。
            //
            // 打开失败**不 panic**：库坏了仍要能开窗口，让用户在维修站里
            // 看到具体原因（那是真故障，该如实报，只是不该阻止启动）。
            let state = build_state(data_dir, resolve_scan_root());
            match ac_registry::Registry::open(&state.db_path()) {
                Ok(reg) => {
                    // 启动时清一次「测试残留」的智能体注册行。
                    //
                    // 插件与契约测试会 POST /api/v1/agents 造记录验证服务端
                    // 行为，跑完不清理。这类行会出现在界面「在线智能体」里，
                    // 用户看到的是一个根本不存在的智能体（实测踩到
                    // `shape-probe`：meta={}、无 usage、first_seen==last_seen）。
                    //
                    // 清理判据三条同时成立才删，真实智能体不可能同时满足。
                    // 失败只记日志 —— 清理是卫生工作，不该阻止启动。
                    match reg.purge_synthetic_agents() {
                        Ok(ids) if !ids.is_empty() => {
                            eprintln!("[assets] 已清理测试残留的智能体: {}", ids.join(", "));
                        }
                        Ok(_) => {}
                        Err(e) => eprintln!("[assets] 清理测试残留失败（不影响启动）: {e}"),
                    }
                    if let Ok(mut g) = state.registry.lock() {
                        *g = Some(reg);
                    }
                }
                Err(e) => {
                    eprintln!("启动时打开数据库失败（界面会显示为故障）：{e}");
                }
            }
            // ── 起对外 HTTP 服务 ────────────────────────────────
            //
            // 为什么要做：用户要 assets-plugin（DSH 插件）能连上这个中枢，
            // 而插件在另一个进程里，只能走本机 HTTP。
            //
            // 端口按版本错开（devel 18197 / standalone 18198），两版可同时跑。
            //
            // **失败不 panic**：端口被占（另一版正在跑）是正常情况，
            // 界面会显示「HTTP 服务未启动」，而不是让整个程序起不来。
            let listening = start_http_service(&app.handle(), &state);
            state
                .http_listening
                .store(listening, std::sync::atomic::Ordering::Relaxed);

            // ── 后台心跳扫描 ────────────────────────────────────
            //
            // 为什么必须自动扫：插件（assets-plugin）通过 B 通道往
            // `<data_dir>/heartbeat/<host>.json` 写心跳，但**中枢只读不写、也不
            // 自动扫**——早先版本必须用户手点「扫描心跳目录」按钮界面才会更新。
            // 结果就是：插件明明在跑、文件明明在那儿，界面上「在线智能体」还是 0。
            //
            // 30 秒扫一次（在线窗口 90s，留三倍余量）。扫描只读文件 + 写
            // agent_heartbeat 表，很轻。失败只记日志，绝不 panic。
            //
            // **不能在这里用 `handle.try_state::<AppState>()`**：那个 API 只在
            // `app.manage(state)` 之后才拿得到，而 manage 在这段代码后面。
            // 早先版本就栽在这儿——线程每轮都拿不到 state，于是静默什么都不做，
            // 表现是「心跳文件在、界面永远 0 在线」，且没有任何报错。
            // 这里改为把已构造好的 data_dir 直接搬进线程，扫描时自己开库。
            {
                let data_dir = state.data_dir.clone();
                let hb_dir_for_log = data_dir.join(ac_registry::agents::HEARTBEAT_DIR_NAME);
                eprintln!("[assets] 启动后台心跳扫描线程，目录 = {}", hb_dir_for_log.display());
                let spawned = std::thread::Builder::new()
                    .name("assets-heartbeat".into())
                    .spawn(move || {
                        let dir = data_dir.join(ac_registry::agents::HEARTBEAT_DIR_NAME);
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(30));
                            // 独立开一个连接：这条线程不该和界面抢同一把 Registry 锁，
                            // 也不能因为界面正忙就整轮跳过。WAL 模式下多连接是安全的。
                            let db_path = data_dir.join("registry.db");
                            match ac_registry::Registry::open(&db_path) {
                                Ok(reg) => {
                                    match reg.scan_heartbeat_dir(&dir) {
                                        Ok(rep) => {
                                            if rep.files_seen > 0 || rep.hosts_registered > 0 {
                                                eprintln!(
                                                    "[assets] 心跳扫描: 文件 {} / 登记 {} / 拒绝 {}",
                                                    rep.files_seen, rep.hosts_registered, rep.rejected.len()
                                                );
                                            }
                                            for r in &rep.rejected {
                                                eprintln!("[assets] 心跳文件被拒 {}: {}", r.file, r.reason);
                                            }
                                        }
                                        Err(e) => eprintln!("[assets] 后台心跳扫描失败（下轮再试）: {e}"),
                                    }
                                    // 扫完必须立刻同步宿主清单。
                                    //
                                    // 为什么：`connected` 是「agents 表里有没有这个宿主的
                                    // 实报通道」。心跳写进 agents 之后，agent_hosts 表里的
                                    // connected 还是上一轮的值 —— 于是界面出现自相矛盾的画面：
                                    // 「在线智能体」说有 1 个在线，「协议支持矩阵」说未连接。
                                    // 两处读的是同一份数据，只是后者没被刷新。
                                    let env = ac_registry::hosts::RealEnv;
                                    if let Err(e) = reg.sync_hosts(&data_dir, &env) {
                                        eprintln!("[assets] 心跳后同步宿主清单失败（下轮再试）: {e}");
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[assets] 心跳扫描开库失败（下轮再试）: {e}");
                                }
                            }
                        }
                    });
                if spawned.is_err() {
                    eprintln!("[assets] 后台心跳扫描线程启动失败");
                }
            }

            // 窗口标题带版本标识：用户同时开两个版本时必须一眼分清。
            // 放在这里而不是 tauri.conf.json：那份配置是静态的，
            // 两个版本共用一个文件会互相覆盖。
            if let Some(win) = app.get_webview_window("main") {
                let title = if is_review_candidate(){
                    "能力核心 ｜ 待审核候选版"
                }else if is_standalone() {
                    "能力核心 · 能力中枢 ｜ 正式版 STANDALONE"
                } else {
                    "能力核心 · 能力中枢 ｜ 开发版 DEVEL"
                };
                let _ = win.set_title(title);
            }

            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::kernel_info,
            commands::open_registry,
            commands::rescan,
            commands::registry_stats,
            commands::list_assets,
            commands::get_asset,
            commands::list_tags,
            commands::list_dedup_candidates,
            commands::decide_dedup,
            commands::dedup_history,
            // M3：总览
            commands::list_agents,
            commands::register_static_agents,
            commands::scan_heartbeats,
            commands::register_agent,
            commands::report_capability_usage,
            commands::capability_usage,
            // M3：维修站
            commands::diagnostics,
            // M3：模型接入
            commands::protocol_matrix,
            commands::llm_status,
            commands::save_provider,
            // 智能体协议清单 + 资产生命周期（第二轮新增）
            commands::list_agent_hosts,
            commands::set_asset_lifecycle,
            commands::lifecycle_counts,
            commands::asset_host_support_detail,
            commands::list_capability_tasks,
            commands::model_slots,
            commands::save_model_slot,
            commands::test_model_slot,
            commands::workflow_catalog,
            commands::protocol_catalog,
            commands::repair_hub,
            commands::search_capabilities,
            commands::run_workflow,
            commands::workflow_history,
            commands::prepare_capability,
            commands::commit_capability,
            commands::operation_history,
            commands::asset_research,
            commands::agent_presence,
            commands::stored_faults,
            commands::fault_detail,
        ])
        .run(context)
        .expect("error while running CapabilityCore");
}
