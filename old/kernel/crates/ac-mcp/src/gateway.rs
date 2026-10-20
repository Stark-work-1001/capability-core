//! 对外查询网关：**只读**的核心，HTTP 与 MCP 共用。
//!
//! # 为什么网关放在这一层
//!
//! 两个对外形态（MCP stdio / HTTP）都必须回答同样的四个问题：
//! 按什么条件找能力、某个能力的详情是什么、库里有哪些标签、我现在能不能服务。
//! 逻辑写两份就会在某次改动后给出不同答案。因此它们都只做**协议解析**，
//! 剩下的全部落在这里。
//!
//! # 检索链路
//!
//! `search` 走 M2 已有的 `RetrievalEngine`：结构化基线（永远可用）
//! + 可选 BM25 全文层（缺席/故障时降级，状态由 `DegradationReport` 如实上报）。
//! 本模块**不碰向量与嵌入**——它们是可选增强，缺席时接口照常工作。
//!
//! # 错误边界
//!
//! `GatewayError` 区分两类：`Unavailable`（库打不开，属于中枢自身故障）
//! 与 `NotFound`（库里没这条，属于正常结果）。前者会让健康检查变红，
//! 后者只是 404。把两者混成一个"查询失败"，健康检查就会在最需要报警的时候
//! 显示一切正常。

use ac_core::{Authority, CapabilityIdentity};
use ac_registry::Registry;
use ac_retrieval::pipeline::{RetrievalEngine, SearchQuery};
use ac_retrieval::store::AssetFilter;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// 网关错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayError {
    /// 中枢自身不可用（库打不开 / 读失败）。
    Unavailable(String),
    /// 目标不存在。这是**正常结果**，不是故障。
    NotFound(String),
    /// 调用方自报的 agent_id **未注册**。
    ///
    /// 与 `NotFound` 必须分开：NotFound 是「资产不存在」，正常结果是
    /// 404 且不泄露存在性；而这一条是**调用方给错了身份**，处置是 400 +
    /// 明确的「先注册」提示。
    ///
    /// 为什么不让它退化成匿名：见 `crate::service::CallerResolution`。
    /// 静默退化会把配置错误伪装成一次权限不足的正常查询，而调用方看到的
    /// 只是一份「少了点东西」的结果——这恰恰是「多智能体裁剪看起来在工作、
    /// 其实根本没接线」最难发现的形态。
    UnregisteredCaller(String),
}

impl GatewayError {
    /// 该错误应映射成的 HTTP 状态码。
    ///
    /// 调用方身份错是 **400**：请求本身不合法，中枢没坏。
    /// 报成 500 会让监控把配置错误算成服务故障。
    pub fn http_status(&self) -> u16 {
        match self {
            GatewayError::Unavailable(_) => 500,
            GatewayError::NotFound(_) => 404,
            GatewayError::UnregisteredCaller(_) => 400,
        }
    }

    /// 稳定的机器可读错误码。
    pub fn code(&self) -> &'static str {
        match self {
            GatewayError::Unavailable(_) => "unavailable",
            GatewayError::NotFound(_) => "not_found",
            GatewayError::UnregisteredCaller(_) => "agent_unregistered",
        }
    }
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayError::Unavailable(e) => write!(f, "unavailable: {e}"),
            GatewayError::NotFound(e) => write!(f, "not_found: {e}"),
            GatewayError::UnregisteredCaller(e) => write!(f, "agent_unregistered: {e}"),
        }
    }
}

/// 对外接口版本与构建信息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionInfo {
    pub api: String,
    pub crate_version: String,
    pub schema_version: String,
    pub parser_rev: i64,
    pub crate_name: String,
}

impl VersionInfo {
    pub fn current() -> Self {
        Self {
            api: crate::API_VERSION.to_string(),
            crate_version: env!("CARGO_PKG_VERSION").to_string(),
            schema_version: ac_registry::store::SCHEMA_VERSION.to_string(),
            parser_rev: ac_registry::store::PARSER_REV,
            crate_name: crate::CRATE_NAME.to_string(),
        }
    }
}

/// 检索条件（对外协议的参数面）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchParams {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub min_authority: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

impl SearchParams {
    fn to_filter(&self) -> Result<AssetFilter, GatewayError> {
        let min_authority = match &self.min_authority {
            None => None,
            Some(s) => Some(Authority::parse(s).ok_or_else(|| {
                GatewayError::Unavailable(format!(
                    "min_authority 非法：{s}（可用 read|observe|write|destructive）"
                ))
            })?),
        };
        let text = self
            .q
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        Ok(AssetFilter {
            kind: non_empty(self.kind.clone()),
            min_authority,
            tag: non_empty(self.tag.clone()),
            text,
        })
    }
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// 一条能力检索结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityRow {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub authority: String,
    pub side_effect: String,
    pub tool_posture: String,
    pub role: Option<String>,
    pub tags: Vec<String>,
    /// 所属生态（由调用方登记时声明；库内资产自身不带，取 "unassigned"）。
    pub ecosystem: String,
    pub capability_count: u32,
    pub source_path: Option<String>,
    pub logical_id: Option<String>,
}

impl CapabilityRow {
    /// 由结构化行投影成对外行。
    ///
    /// `ecosystem` 的口径见 [`ecosystem_of_asset`]：**由资产的
    /// `source_path` 推断**，不是写死 `unassigned`。
    ///
    /// 写死的后果是具名的 ecosystem 收窄永远不生效——那是缺口 1 的
    /// 核心诉求（HTTP 侧所有调用都退化成匿名档），而 ecosystem 是
    /// 目前**唯一有真实数据支撑**的收窄维度。
    fn from_row(r: &ac_retrieval::store::AssetRow, source_path: Option<&str>) -> Self {
        Self {
            id: r.id.clone(),
            kind: r.kind.clone(),
            name: r.name.clone(),
            description: r.description.clone(),
            authority: r.authority.clone(),
            side_effect: String::new(),
            tool_posture: String::new(),
            role: None,
            tags: r.tags.clone(),
            ecosystem: ecosystem_of_asset(source_path),
            capability_count: 0,
            source_path: source_path.map(str::to_string),
            logical_id: r.logical_id.clone(),
        }
    }
}

/// 资产所属生态：由磁盘路径推断，与 C 通道静态归属**同一条规则**。
///
/// # 为什么不在库里另开一列
///
/// `assets` 表没有 ecosystem 列（schema v5 实测）。而生态归属本来就
/// 是从 `source_path` **推断**出来的（agents.rs 的 `infer_ecosystem`），
/// 开一列就要多一套回填与一致性维护，而结果与推断值同源——
/// 那正是 M2.5 索引增量闸门已经踩过的坑：同一件事两处各持一套说法。
///
/// # 为什么 `unassigned` 是合法答案
///
/// 库里确实有大量资产不带宿主标记（vendor 树里的裸包）。那是**结论**
/// 不是占位，所以这里如实返回 `unassigned`，不做兜底猜测。
pub fn ecosystem_of_asset(source_path: Option<&str>) -> String {
    match source_path.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => ac_registry::agents::infer_ecosystem(p).ecosystem,
        None => ac_registry::agents::ECOSYSTEM_UNASSIGNED.to_string(),
    }
}

/// 检索结果 + 降级说明。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchResult {
    pub items: Vec<CapabilityRow>,
    pub total: u32,
    /// 本次查询**没能**用上的检索阶段及原因。如实上报，不静默吞掉。
    pub degradation: Vec<String>,
    pub visible: u32,
    pub denied: u32,
}

/// 单个能力的详情。
///
/// **不 derive PartialEq/Eq**：UsageRow（ac-registry 定义）两者都没实现，
/// 而 derive 是"整包要求"的——为它单独给上游加 Eq 会把一个只读投影
/// 变成需要维护的跨 crate 契约，不值当。需要比较时比较序列化结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDetail {
    pub manifest: ac_core::CapabilityManifest,
    pub tags: Vec<String>,
    pub ecosystem: String,
    pub usage: Vec<ac_registry::UsageRow>,
}

/// 依赖与自身健康状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthReport {
    /// ok / degraded / down
    pub status: String,
    pub version: VersionInfo,
    pub registry: ComponentHealth,
    pub retrieval: Vec<StageHealth>,
    pub agents: u32,
    /// 本机 unix 秒，便于调用方判断时钟。
    pub now: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComponentHealth {
    /// ready / failed
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StageHealth {
    pub stage: String,
    pub state: String,
    pub detail: String,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 只读网关。
///
/// 内部用 `Mutex` 包住 `Registry`：`Registry` 自身不是 `Sync`，
/// 而 MCP stdio 与 HTTP 会从不同线程进来。锁的粒度是**单次查询**，
/// 不跨调用持有——否则一个慢查询会把整条对外链路堵死。
///
/// `engine` 一旦建好就不再变——全文索引在建网关时一次性挂上
/// （见 `CapabilityGateway::open`），因此查询路径上没有任何
/// 「挂没挂上」的分支，也就不可能出现「健康检查说 ready、
/// 查询却按结构化基线走」的时序错位。
pub struct CapabilityGateway {
    registry: Arc<Mutex<Registry>>,
    engine: std::sync::RwLock<RetrievalEngine<RegistryHandle>>,
    /// 全文索引的落盘目录，如实报进健康检查详情。
    ///
    /// 挂载失败时索引句柄仍然存在（FullTextIndex::failed 造的坏句柄），
    /// 所以「有没有句柄」不能代表「能不能用」——
    /// 状态一律取 engine.fulltext().state()。
    fulltext_dir: Option<std::path::PathBuf>,
    /// 向量层句柄。**与全文层同级**：挂没挂、活没活，只影响
    /// 健康检查里 vector 那一格，检索照常。
    ///
    /// 状态一律取 VectorIndex::probe_state()（**每次真探**），不取缓存值：
    /// 健康检查如果复用上一次的判定，服务停了以后只要没人查询就会
    /// 一直报 ready，而这正是要防的「假装 ready」。
    vector: Option<Arc<ac_retrieval::vector::VectorIndex>>,
    /// 库文件所在目录。技能全文接口用它解析 `source_path`。
    ///
    /// 库里存的是**相对库父目录**的路径（`..\vendor\...`），不是相对
    /// scan_root。少了这个基准就会拼出 `vendor\..\vendor\...` 这种二次前缀
    /// ——M4 的协议检查曾因同一个错误把 506 条资产全判成「源文件已不存在」。
    db_parent: std::path::PathBuf,
}

impl CapabilityGateway {
    /// 库文件所在目录，作为 `source_path` 的解析基准。
    pub fn project_root(&self) -> &std::path::Path {
        &self.db_parent
    }

    /// 打开（或创建）指定库并构建网关，**同时挂上 BM25 全文索引**。
    ///
    /// # 索引位置随 DB 目录改道
    ///
    /// 索引目录是 `db_path.parent()/search-index/`（`FullTextIndex::index_dir_for`），
    /// 而不是 `%USERPROFILE%` 下的固定位置。原因写在该函数文档里：
    /// 本机有一条按目录生效的 FS 策略，从 `C:\external-assets\` 启动的进程
    /// 写 `%USERPROFILE%` 会 `ERROR_ACCESS_DENIED`。挂��库旁边就
    /// **自动继承**中枢既有的 data_dir 改道机制（`--db` 指到哪，
    /// 索引就跟到哪），不需要在本项目里重写一遍改道逻辑。
    ///
    /// # 挂载失败绝不冒泡，也绝不假装 ready
    ///
    /// `FullTextIndex::attach` **永不返回 Err**：它把索引不存在 / 过期 /
    /// 损坏 / 输入源不可用四种结局都写进 `state()`。这里只是把那个
    /// 状态原样带进健康检查。于是：
    ///
    /// - 挂上了 → `full_text: ready`，关键词检索走 BM25；
    /// - 没挂上 → `full_text: failed(<原因>)`，检索**自动降级**为结构化基线。
    ///
    /// 「启动时索引还没建好、但服务必须先起来」这一常见时序由
    /// `SyncPolicy::Auto` 覆盖：它当场把索引建出来（实测 0.29-0.51s，
    /// 506 条资产），而不是让服务在没有全文层的情况下跑起来。
    pub fn open(db_path: &std::path::Path) -> Result<Self, GatewayError> {
        let reg = Registry::open(db_path).map_err(GatewayError::Unavailable)?;
        let index_dir = ac_retrieval::fulltext::FullTextIndex::index_dir_for(db_path);
        Self::from_registry_with_fulltext(reg, index_dir)
    }

    /// 用已有 Registry 构建网关（**不挂全文层**）。
    ///
    /// 供测试与「库不在磁盘上」的场合使用。真实服务一律走
    /// [`Self::open`]——它会挂全文层。
    pub fn from_registry(reg: Registry) -> Result<Self, GatewayError> {
        let shared = Arc::new(Mutex::new(reg));
        let engine_store = RegistryHandle(shared.clone());
        let engine = RetrievalEngine::new(
            engine_store,
            ac_retrieval::health::HealthSnapshot::structured_only(),
        );
        Ok(Self {
            registry: shared,
            engine: std::sync::RwLock::new(engine),
            fulltext_dir: None,
            vector: None,
            db_parent: std::path::PathBuf::from("."),
        })
    }

    /// 用已有 Registry 构建网关并**在指定目录挂上 BM25**。
    fn from_registry_with_fulltext(
        reg: Registry,
        index_dir: std::path::PathBuf,
    ) -> Result<Self, GatewayError> {
        let shared = Arc::new(Mutex::new(reg));
        // 共享适配器：让 RetrievalEngine 拿到 &Registry，同时网关还能对外共享。
        let engine_store = RegistryHandle(shared.clone());
        // 快照里把 FullText 声明成 Ready 是**故意**的：装配那一刻我们确实
        // 挂了一个句柄上去，而它的真实状态由 `FullTextIndex::state()` 覆盖快照
        // （见 pipeline 的 run_fulltext：挂了索引就以索引自己的状态为准）。
        // 绝不能因为「怕它坏」就在快照里写 Absent——那会让一条健康的索引
        // 永远只报缺席，而 `health()` 走的是索引句柄，不受这份快照影响。
        let health = ac_retrieval::health::HealthSnapshot::new([(
            ac_retrieval::health::Stage::FullText,
            ac_retrieval::health::StageState::Ready,
        )]);
        let index = ac_retrieval::fulltext::FullTextIndex::attach(
            &index_dir,
            &engine_store,
            ac_retrieval::fulltext::SyncPolicy::Auto,
        );
        let engine = RetrievalEngine::new(engine_store, health)
            .with_fulltext(std::sync::Arc::new(index));
        // index_dir 是 db_parent/search-index，父目录即库所在目录。
        let db_parent = index_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        Ok(Self {
            registry: shared,
            engine: std::sync::RwLock::new(engine),
            fulltext_dir: Some(index_dir),
            vector: None,
            db_parent,
        })
    }

    /// 挂上向量召回层。
    ///
    /// **纯增量且永不失败**：装配只做一次探活，探不通就是 Failed(原因)，
    /// 检索照常降级到结构化基线加 BM25。这与全图层的失败语义完全一致。
    ///
    /// 地址与超时从环境变量读（AC_VECTOR_URL / AC_VECTOR_TIMEOUT_MS），
    /// 默认 http://127.0.0.1:18191 与 2500ms。
    /// **懒建**：这里不嵌入任何东西，首次带文本的查询才建表。
    pub fn with_vector(mut self, index: Arc<ac_retrieval::vector::VectorIndex>) -> Self {
        *self.engine.get_mut().unwrap_or_else(|e|e.into_inner()).vector_mut() = Some(index.clone());
        self.vector = Some(index);
        self
    }

    /// 按环境变量装配向量层（默认地址 127.0.0.1:18191）。
    pub fn with_vector_from_env(self) -> Self {
        let (cfg, warnings) = ac_retrieval::vector::VectorConfig::from_env();
        for w in &warnings {
            tracing::warn!("向量层配置：{w}");
        }
        let idx = ac_retrieval::vector::VectorIndex::attach(cfg);
        self.with_vector(Arc::new(idx))
    }

    /// 挂上的向量层句柄（未挂载则为 None）。
    pub fn vector(&self) -> Option<&Arc<ac_retrieval::vector::VectorIndex>> {
        self.vector.as_ref()
    }

    /// 全文索引目录（未挂载为 `None`）。
    pub fn fulltext_dir(&self) -> Option<&std::path::Path> {
        self.fulltext_dir.as_deref()
    }

    fn manifest_of(&self, id: &str) -> Result<Option<ac_core::CapabilityManifest>, GatewayError> {
        let g = self.registry.lock().map_err(|e| {
            GatewayError::Unavailable(format!("注册表锁已中毒: {e}"))
        })?;
        g.get_manifest(id).map_err(GatewayError::Unavailable)
    }

    fn refresh_fulltext(&self) {
        use ac_retrieval::store::AssetStore;
        let Some(dir)=self.fulltext_dir.as_ref() else{return;};
        let store=RegistryHandle(self.registry.clone());
        let Ok(snapshot)=store.index_source() else{return;};let fingerprint=snapshot.fingerprint();
        let dirty=self.engine.read().unwrap_or_else(|e|e.into_inner()).fulltext().is_some_and(|i|i.meta().fingerprint!=fingerprint || !i.state().is_usable());
        if !dirty{return;}
        let mut engine=self.engine.write().unwrap_or_else(|e|e.into_inner());
        if let Some(index)=engine.fulltext_mut().as_mut(){
            if index.meta().fingerprint==fingerprint && index.state().is_usable(){return;}
            if index.state().is_usable(){if let Some(index)=Arc::get_mut(index){let _=index.sync(&store);return;}}
        }
        *engine.fulltext_mut()=Some(Arc::new(ac_retrieval::fulltext::FullTextIndex::attach(dir,&store,ac_retrieval::fulltext::SyncPolicy::Auto)));
    }

    /// 按条件找能力。**不裁剪**——裁剪在 [`crate::service`] 层做，
    /// 因为它需要调用方身份，网关本身是匿名的。
    pub fn search(&self, p: &SearchParams) -> Result<SearchResult, GatewayError> {
        let filter = p.to_filter()?;
        let limit = p.limit.unwrap_or(50).clamp(1, 200);
        let offset = p.offset.unwrap_or(0);
        let q = SearchQuery {
            filter: filter.clone(),
            text: filter.text.clone(),
            limit: limit.saturating_add(offset),
        };
        self.refresh_fulltext();
        let res = self.engine.read().unwrap_or_else(|e|e.into_inner()).search(&q);
        // 生态收窄需要每条资产的 source_path，而 `AssetRow`（ac-retrieval 的
        // 结构化行）刻意不带它——那是跨 crate 契约，不为本需求扩它。
        // 一次查询的行数已被 limit clamp 到 <=200，所以这里**按本批 id
        // 一次性**补齐，而不是每行一次查询：后者会把 P95 从毫秒级打到秒级。
        let page: Vec<&ac_retrieval::store::AssetRow> = res
            .hits
            .iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        let paths = self.source_paths_of(page.iter().map(|r| r.id.as_str()));
        let items: Vec<CapabilityRow> = page
            .iter()
            .map(|r| {
                let p = paths.get(r.id.as_str()).map(String::as_str);
                CapabilityRow::from_row(r, p)
            })
            .collect();
        let total = {
            let g = self
                .registry
                .lock()
                .map_err(|e| GatewayError::Unavailable(format!("注册表锁已中毒: {e}")))?;
            g.count_assets(&ac_registry::retrieval_impl::to_asset_query(&filter, 1000))
                .map_err(GatewayError::Unavailable)?
        };
        let degradation: Vec<String> = res
            .degradation
            .skipped
            .iter()
            .map(|(s, st)| format!("{s}: {st}"))
            .collect();
        let n = items.len() as u32;
        Ok(SearchResult {
            visible: n,
            denied: 0,
            total,
            items,
            degradation,
        })
    }

    /// 批量取本批资产的 source_path，供生态推断用。
    ///
    /// **失败一律退化成空映射**，绝不冒泡：source_path 只用于
    /// *推断* 生态，拿不到时该资产的 ecosystem 就是 `unassigned`，
    /// 这是合法答案（库里确实有资产不带宿主标记），不该让整次查询失败。
    /// 这与「索引读不到就报 Failed」是两种不同的东西，别混。
    fn source_paths_of<'a, I>(&self, ids: I) -> std::collections::HashMap<String, String>
    where
        I: Iterator<Item = &'a str>,
    {
        let ids: Vec<String> = ids.map(str::to_string).collect();
        match self
            .registry
            .lock()
            .map_err(|e| e.to_string())
            .and_then(|g| g.source_paths_of(&ids))
        {
            Ok(m) => m,
            // 见函数文档：拿不到路径不是致命故障（生态就是 unassigned）。
            Err(_) => Default::default(),
        }
    }

    /// 取单个能力详情。
    pub fn detail(&self, id: &str) -> Result<CapabilityDetail, GatewayError> {
        let id = id.trim();
        if id.is_empty() {
            return Err(GatewayError::NotFound("id 不能为空".into()));
        }
        let manifest = self
            .manifest_of(id)?
            .ok_or_else(|| GatewayError::NotFound(format!("能力 {id} 不存在")))?;
        let (tags, usage, agents) = {
            let g = self
                .registry
                .lock()
                .map_err(|e| GatewayError::Unavailable(format!("注册表锁已中毒: {e}")))?;
            (
                g.tags_of_asset(id).unwrap_or_default(),
                g.capability_usage(200).unwrap_or_default(),
                g.list_agents().map(|v| v.len()).unwrap_or(0),
            )
        };
        let _ = agents;
        let usage = usage.into_iter().filter(|u| u.asset_id == id).collect();
        // 必须在把 manifest 移进结构体**之前**算：移走之后就借不到了。
        let ecosystem = ecosystem_of_asset(manifest.provenance.source_path.as_deref());
        Ok(CapabilityDetail {
            manifest,
            tags,
            ecosystem,
            usage,
        })
    }

    /// 库内标签清单。
    pub fn tags(&self) -> Result<Vec<ac_registry::TagCount>, GatewayError> {
        let g = self
            .registry
            .lock()
            .map_err(|e| GatewayError::Unavailable(format!("注册表锁已中毒: {e}")))?;
        g.all_tags().map_err(GatewayError::Unavailable)
    }

    /// 取某资产的身份投影，供裁剪判定用。
    ///
    /// 取不到时返回 `None`（而不是默认身份）：调用方据此决定是
    /// "按未知身份从严拒绝"还是"跳过这条"，由 service 层决定。
    pub fn identity_of_asset(&self, asset_id: &str) -> Option<CapabilityIdentity> {
        self.manifest_of(asset_id).ok().flatten().map(|m| m.identity)
    }

    /// 列出某资产的标签。
    pub fn tags_of(&self, asset_id: &str) -> Vec<String> {
        self.registry
            .lock()
            .ok()
            .and_then(|g| g.tags_of_asset(asset_id).ok())
            .unwrap_or_default()
    }

    /// 以可变方式访问底层注册表（**仅供协调元数据写入**，如注册智能体、上报用量）。
    ///
    /// 刻意不暴露成"随便什么都能写"的裸引用：调用点必须自己说明用途，
    /// 且审阅时一眼能看出哪些地方动了库。
    pub fn with_registry<R>(&self, f: impl FnOnce(&Registry) -> R) -> Result<R, GatewayError> {
        let g = self
            .registry
            .lock()
            .map_err(|e| GatewayError::Unavailable(format!("注册表锁已中毒: {e}")))?;
        Ok(f(&g))
    }

    /// 健康自检：中枢自身 + 各依赖是否就绪。
    pub fn health(&self) -> HealthReport {
        let (registry_status, registry_detail, agents): (String, String, u32) =
            match self.registry.lock() {
                Ok(g) => match g.total() {
                    Ok(n) => (
                        "ready".to_string(),
                        format!("资产 {n} 条"),
                        g.list_agents().map(|v| v.len() as u32).unwrap_or(0),
                    ),
                    Err(e) => ("failed".to_string(), e.clone(), 0),
                },
                Err(_) => ("failed".to_string(), "注册表锁已中毒".to_string(), 0),
            };

        // 检索各阶段的真实状态。结构化基线永远是 ready（它没有外部依赖），
        // 全文层取决于索引挂载结果——缺席或故障都要如实说，不粉饰。
        let mut retrieval = vec![StageHealth {
            stage: ac_retrieval::health::Stage::Structured.as_str().to_string(),
            state: if registry_status == "ready" { "ready" } else { "failed" }
                .to_string(),
            detail: registry_detail.clone(),
        }];
        let engine=self.engine.read().unwrap_or_else(|e|e.into_inner());
        let ft = engine.fulltext();
        match ft {
            None => retrieval.push(StageHealth {
                stage: ac_retrieval::health::Stage::FullText.as_str().to_string(),
                state: "absent".to_string(),
                detail: "未挂载全文索引，检索降级为结构化基线（不影响可用性）".to_string(),
            }),
            Some(idx) => {
                // **如实反映索引此刻的真实状态**，不做任何美化：
                // ready 才报 ready，坏掉的索引带原因报 failed。
                // 索引目录一并写进 detail——排查「为什么搜不到」时，
                // 第一件事就是要知道它到底去哪儿找了。
                let st = idx.state();
                let where_ = self
                    .fulltext_dir
                    .as_ref()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|| "（未记录）".to_string());
                let detail = match st {
                    ac_retrieval::health::StageState::Ready => format!(
                        "BM25 全文索引就绪：文档 {} 篇，目录 {}",
                        idx.doc_count(),
                        where_
                    ),
                    other => format!(
                        "BM25 全文索引不可用（{}），检索已降级为结构化基线；目录 {}",
                        other,
                        where_
                    ),
                };
                retrieval.push(StageHealth {
                    stage: ac_retrieval::health::Stage::FullText.as_str().to_string(),
                    state: st.as_str().to_string(),
                    detail,
                });
            }
        }
        // 向量层：**每次健康检查都真探一次**，绝不复用上一次的判定。
        // 复用会让「服务停了但没人查询」这一格一直报 ready —— 正是要防的谎报。
        match self.vector.as_ref() {
            None => retrieval.push(StageHealth {
                stage: ac_retrieval::health::Stage::Vector.as_str().to_string(),
                state: "absent".to_string(),
                detail: "未挂载向量层，检索降级为结构化基线加 BM25（不影响可用性）"
                    .to_string(),
            }),
            Some(idx) => {
                let st = idx.probe_state();
                let endpoint = idx.client().config().endpoint.clone();
                let detail = match &st {
                    ac_retrieval::health::StageState::Ready => {
                        // ready 时必须给出**可核对的身份**：模型名与维度。
                        // 一句空话的「就绪」无法与「假装就绪」区分开。
                        let info = idx.info();
                        let model = info
                            .as_ref()
                            .map(|i| i.model.clone())
                            .filter(|m| !m.is_empty())
                            .unwrap_or_else(|| "（服务未自报模型名）".to_string());
                        let dim = info
                            .as_ref()
                            .and_then(|i| i.dim)
                            .map(|d| d.to_string())
                            .unwrap_or_else(|| "（未自报）".to_string());
                        // 正在后台建表时，**必须把这件事实说出来**。
                        // 只报「就绪 + 已入表 0 条」会让读者以为一切正常，
                        // 而实际上此刻的查询一定拿不到向量召回。
                        let progress = if idx.is_building() {
                            "（首次建表中，完成前的查询会降级）".to_string()
                        } else {
                            String::new()
                        };
                        format!(
                            "向量召回就绪：模型 {}，维度 {}，已入表 {} 条{}，端点 {}",
                            model,
                            dim,
                            idx.len(),
                            progress,
                            endpoint
                        )
                    }
                    other => format!(
                        "向量召回不可用（{}），检索已降级为结构化基线加 BM25；端点 {}",
                        other, endpoint
                    ),
                };
                retrieval.push(StageHealth {
                    stage: ac_retrieval::health::Stage::Vector.as_str().to_string(),
                    state: st.as_str().to_string(),
                    detail,
                });
            }
        }

        let failed = registry_status != "ready";
        let degraded = retrieval.iter().any(|s| !s.state.eq("ready"));
        HealthReport {
            status: if failed {
                "down".into()
            } else if degraded {
                "degraded".into()
            } else {
                "ok".into()
            },
            version: VersionInfo::current(),
            registry: ComponentHealth {
                status: registry_status,
                detail: registry_detail,
            },
            retrieval,
            agents,
            now: now_unix(),
        }
    }
}

/// `AssetStore` 的线程安全适配器。
///
/// `RetrievalEngine` 要求 `S: AssetStore`，而 `Registry` 不是 `Sync`。
/// 这里持 `Arc<Mutex<Registry>>`，在 trait 方法内部取锁——
/// 锁只覆盖单次查询，不会在流水线之外被持有。
struct RegistryHandle(Arc<Mutex<Registry>>);

impl RegistryHandle {
    fn with<R>(&self, f: impl FnOnce(&Registry) -> R) -> Result<R, String> {
        let g = self.0.lock().map_err(|e| format!("注册表锁已中毒: {e}"))?;
        Ok(f(&g))
    }
}

impl ac_retrieval::store::AssetStore for RegistryHandle {
    fn query(&self, filter: &AssetFilter, limit: u32) -> Vec<ac_retrieval::store::AssetRow> {
        self.try_query(filter, limit).unwrap_or_default()
    }

    fn try_query(
        &self,
        filter: &AssetFilter,
        limit: u32,
    ) -> Result<Vec<ac_retrieval::store::AssetRow>, String> {
        // with() 的闭包返回 Result，因此这里整体就是 Result<Result<..>>，
        // 用 and_then 拍平——调用方只该看到一个错误通道。
        self.with(|g| {
            let q = ac_registry::retrieval_impl::to_asset_query(filter, limit);
            let summaries = g.query_assets(&q)?;
            // AssetSummary -> AssetRow：用 tags_of_asset 补标签，文本拼 name+desc+tags。
            // 锁在这一层就够：整批取完再返回，不跨调用持有。
            Ok(summaries
                .into_iter()
                .map(|s| {
                    let tags = g.tags_of_asset(&s.id).unwrap_or_default();
                    let text = format!("{} {} {}", s.name, s.description, tags.join(" "));
                    ac_retrieval::store::AssetRow {
                        id: s.id,
                        kind: s.kind,
                        name: s.name,
                        description: s.description,
                        authority: s.authority,
                        tags,
                        text,
                        logical_id: s.logical_id,
                        location_count: s.location_count,
                    }
                })
                .collect())
        })
        .and_then(|r| r)
    }

    fn count(&self, filter: &AssetFilter) -> u32 {
        self.with(|g| {
            g.count_assets(&ac_registry::retrieval_impl::to_asset_query(filter, 1000))
                .unwrap_or(0)
        })
        .unwrap_or(0)
    }

    fn index_source(&self) -> Result<ac_retrieval::store::IndexSnapshot, String> {
        self.with(|g| g.index_source()).and_then(|r| r)
    }

    fn try_query_in(
        &self,
        filter: &AssetFilter,
        ids: &[String],
        limit: u32,
    ) -> Result<Vec<ac_retrieval::store::AssetRow>, String> {
        self.with(|g| g.try_query_in(filter, ids, limit)).and_then(|r| r)
    }

    fn is_healthy(&self) -> bool {
        self.0.lock().map(|g| g.total().is_ok()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::{AssetKind, CapabilityManifest, ProvidedCapability};

    fn gw_with(manifests: Vec<CapabilityManifest>) -> CapabilityGateway {
        let reg = Registry::open_in_memory().expect("内存库");
        for m in &manifests {
            reg.upsert(m).expect("upsert");
        }
        CapabilityGateway::from_registry(reg).expect("网关")
    }

    fn manifest(id: &str, name: &str, auth: Authority, kind: AssetKind) -> CapabilityManifest {
        let mut m = CapabilityManifest::new(id, kind, name);
        m.description = format!("{name} 的说明");
        m.identity.authority = auth;
        m.provides.push(ProvidedCapability {
            capability_id: format!("{id}::main"),
            name: name.to_string(),
            summary: format!("{name} 能力"),
            input_schema: None,
            tags: vec!["read_only".into()],
        });
        m
    }

    #[test]
    fn search_filters_by_kind_and_text() {
        let gw = gw_with(vec![
            manifest("s::a", "画时序图", Authority::Read, AssetKind::Skill),
            manifest("m::b", "zotero", Authority::Read, AssetKind::Mcp),
        ]);
        let r = gw
            .search(&SearchParams {
                kind: Some("mcp".into()),
                ..Default::default()
            })
            .expect("search");
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].id, "m::b");
    }

    #[test]
    fn search_by_keyword_finds_asset() {
        let gw = gw_with(vec![manifest("s::a", "画时序图", Authority::Read, AssetKind::Skill)]);
        let r = gw
            .search(&SearchParams {
                q: Some("时序".into()),
                ..Default::default()
            })
            .expect("search");
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].name, "画时序图");
    }

    #[test]
    fn detail_returns_manifest_and_missing_is_not_found() {
        let gw = gw_with(vec![manifest("s::a", "画时序图", Authority::Read, AssetKind::Skill)]);
        let d = gw.detail("s::a").expect("detail");
        assert_eq!(d.manifest.name, "画时序图");
        assert!(d.tags.contains(&"read_only".to_string()));

        assert!(matches!(
            gw.detail("nope"),
            Err(GatewayError::NotFound(_))
        ));
    }

    #[test]
    fn health_reports_ready_with_structured_baseline() {
        let gw = gw_with(vec![]);
        let h = gw.health();
        // 结构化基线永远就绪，整体至少不是 down。
        assert_ne!(h.status, "down");
        assert_eq!(h.registry.status, "ready");
        // 向量层未挂载 → 如实缺席，且**不影响**整体可用性。
        // 这条锁定的是「没挂就报缺席」，与挂了以后的 ready/failed 是两条路径。
        assert!(h.retrieval.iter().any(|s| s.stage == "vector" && s.state == "absent"));
        assert!(gw.vector().is_none());
    }

    // ── 缺口 2：BM25 全文索引挂载 ─────────────────────────────────

    /// 测试用临时目录。
    ///
    /// **不用 %TEMP%**：本机有一条按目录生效的 FS 策略，从 C:\external-assets\
    /// 启动的进程写 %USERPROFILE%（含 %TEMP%）会 ERROR_ACCESS_DENIED。
    /// 放在 target/ 下：短路径、可写、随构建产物一起清掉。
    /// 与 ac-retrieval 的 fulltext 测试同一处纪律。
    fn scratch(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::current_exe().expect("current_exe");
        p.pop(); // deps
        if p.file_name().map(|n| n == "deps").unwrap_or(false) {
            p.pop(); // release / debug
        }
        p.push("ac-mcp-gw-tests");
        p.push(format!("{}-{}-{}",tag,std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&p).expect("创建测试目录");
        p
    }

    /// 建一个**挂上全文层**的网关（内存库 + 指定索引目录）。
    fn gw_with_fulltext(tag: &str, manifests: Vec<CapabilityManifest>) -> CapabilityGateway {
        let reg = Registry::open_in_memory().expect("内存库");
        for m in &manifests {
            reg.upsert(m).expect("upsert");
        }
        let dir = scratch(tag);
        CapabilityGateway::from_registry_with_fulltext(reg, dir).expect("网关")
    }

    /// 判别器：同一份语料，**没挂全文层**时中文召回必须召不到 `s::split`。
    ///
    /// 这条与上面的正例配对，用来证明那条约��**有鉴别力**。
    /// 只断言「搜『时序图』能搜到 s::exact」的话，一个「全文层压根没挂、
    /// 纯靠 LIKE 跑」的网关照样能过——而那正是缺口本身。
    /// 正例负责「挂上就有」，本条负责「不挂就没有」，两条合起来才排除了
    /// 「测试其实测的是 LIKE」的误读。
    #[test]
    fn without_fulltext_layer_chinese_recall_loses_the_split_document() {
        let reg = Registry::open_in_memory().expect("内存库");
        for m in ft_manifests() {
            reg.upsert(&m).expect("upsert");
        }
        let gw = CapabilityGateway::from_registry(reg).expect("网关");
        let r = gw
            .search(&SearchParams {
                q: Some("时序图".into()),
                ..Default::default()
            })
            .expect("search");
        let ids: Vec<&str> = r.items.iter().map(|i| i.id.as_str()).collect();
        assert!(
            !ids.contains(&"s::split"),
            "没挂全文层时不该召回到 s::split（正文里「时序」与「图」不相邻）"
        );
        assert!(
            ids.contains(&"s::exact"),
            "对照组：整串 LIKE 仍应命中 s::exact，实际 {ids:?}"
        );
    }

    /// 一条正文与名字不成连续子串的资产。
    ///
    /// 关键在 `s::split`：它的正文里「时序」与「图」之间隔着别的字，
    /// 所以结构化基线的 `LIKE '%时序图%'` **抓不到**它，
    /// 而 BM25 按词匹配能抓到。这条资产就是「全文层到底有没有参与」
    /// 的判别器——只查名字里正好含「时序图」的那条，证明不了任何事。
    fn ft_manifests() -> Vec<CapabilityManifest> {
        let mut a = manifest("s::exact", "时序图", Authority::Read, AssetKind::Skill);
        a.description = "把消息流转画成时序图".into();
        let mut b = manifest("s::split", "多关键词", Authority::Read, AssetKind::Skill);
        b.description = "用于绘制时序 组件 与 流程图".into();
        let mut c = manifest("s::noise", "完全无关", Authority::Read, AssetKind::Skill);
        c.description = "数据库备份与恢复脚本".into();
        vec![a, b, c]
    }

    /// 挂载后健康检查必须报 `full_text: ready`，且**如实**带文档数。
    ///
    /// 这是「不许假装 ready」的正向一侧：报 ready 时必须真的挂上了，
    /// 且 detail 里要有可核对的数字，不是空话。
    #[test]
    fn fulltext_attaches_and_health_reports_ready_truthfully() {
        let gw = gw_with_fulltext("ready", ft_manifests());
        let h = gw.health();
        let ft = h
            .retrieval
            .iter()
            .find(|s| s.stage == "full_text")
            .expect("健康检查必须提到 full_text");
        assert_eq!(
            ft.state, "ready",
            "挂载成功后必须报 ready，实际 {}（{}）",
            ft.state, ft.detail
        );
        assert!(
            ft.detail.contains("3"),
            "ready 时必须如实报文档数，实际：{}",
            ft.detail
        );
        // 目录要写进 detail：排查「为什么搜不到」时第一件事就是知道它去哪找了。
        assert!(
            ft.detail.contains("search-index") || ft.detail.contains("ac-mcp-gw-tests"),
            "detail 必须带索引目录，实际：{}",
            ft.detail
        );
    }

    /// **中文关键词真的能召回**——且召回到结构化 LIKE 抓不到的那条。
    ///
    /// 这条是缺口 2 的验收主线。若只断言「搜『时序图』能搜到 s::exact」，
    /// 一个「全文层没挂、纯靠 LIKE」的实��也能通过——那正是缺口本身。
    /// 必须断言 `s::split` 也被召回，才能证明 BM25 真的参与了。
    #[test]
    fn chinese_keyword_search_recalls_what_substring_match_cannot() {
        let gw = gw_with_fulltext("cjk-recall", ft_manifests());
        // 对照组：结构化基线的整串 LIKE。
        let like = gw
            .search(&SearchParams {
                q: Some("时序图".into()),
                ..Default::default()
            })
            .expect("search");
        let like_ids: Vec<&str> = like.items.iter().map(|i| i.id.as_str()).collect();
        assert!(
            like_ids.contains(&"s::exact"),
            "对照组：LIKE 应至少命中 s::exact，实际 {like_ids:?}"
        );

        let r = gw
            .search(&SearchParams {
                q: Some("时序图".into()),
                ..Default::default()
            })
            .expect("search");
        let ids: Vec<&str> = r.items.iter().map(|i| i.id.as_str()).collect();
        assert!(
            ids.contains(&"s::split"),
            "BM25 应召回到 LIKE 抓不到的 s::split（正文里「时序」与「图」不相邻）。             实际召回 {ids:?}，degradation={:?} —— 若 s::split 不在，说明全文层没参与，             降级说明应能看出来",
            r.degradation
        );
        assert!(
            !ids.contains(&"s::noise"),
            "毫不相干的资产不该被召回，实际 {ids:?}"
        );
        // 降级说明里不得出现 full_text 故障——它确实参与了。
        assert!(
            !r.degradation.iter().any(|d| d.starts_with("full_text: failed")),
            "全文层参与时不得报故障，实际 degradation={:?}",
            r.degradation
        );
    }

    /// 挂载失败必须**优雅降级**：结构化结果照样返回，且状态如实为 failed。
    ///
    /// 用「索引目录的位置被一个普通文件占住」构造必然失败——这是
    /// ac-retrieval 里 `degradation_mode_3_index_build_failed` 用的同一招。
    #[test]
    fn failed_mount_degrades_to_structured_and_reports_failed_not_ready() {
        let reg = Registry::open_in_memory().expect("内存库");
        for m in ft_manifests() {
            reg.upsert(&m).expect("upsert");
        }
        let base = scratch("mount-fail");
        let blocked = base.join("blocked");
        std::fs::write(&blocked, b"not a directory").expect("写占位文件");

        let gw = CapabilityGateway::from_registry_with_fulltext(reg, blocked).expect("网关");
        // ① 优雅降级：检索**仍然有结果**。
        let r = gw
            .search(&SearchParams {
                q: Some("时序图".into()),
                ..Default::default()
            })
            .expect("search");
        assert!(
            !r.items.is_empty(),
            "挂载失败也必须返回结构化结果，实际 0 条"
        );
        // ② 状态如实：failed + 带原因，绝不 ready。
        let h = gw.health();
        let ft = h.retrieval.iter().find(|s| s.stage == "full_text").unwrap();
        assert_eq!(
            ft.state, "failed",
            "挂载失败必须报 failed 而不是 ready"
        );
        assert!(!ft.detail.is_empty(), "失败必须带原因，不能只有状态");
        assert!(
            h.status != "ok",
            "有阶段故障时整体不该是 ok，实际 {}",
            h.status
        );
    }

    /// 索引目录必须**随 db 目录**走——这是本机那条 FS 策略的要求。
    #[test]
    fn index_dir_follows_the_database_directory() {
        let p = std::path::Path::new("D:/some/where/data/registry.db");
        let dir = ac_retrieval::fulltext::FullTextIndex::index_dir_for(p);
        assert_eq!(dir, std::path::Path::new("D:/some/where/data/search-index"));
        // 绝不能落在 %USERPROFILE% 一类与 db 无关的位置。
        assert!(dir.parent().unwrap() == std::path::Path::new("D:/some/where/data"));
    }

    /// 未挂载全文层的网关（`from_registry`）仍要如实报 absent。
    ///
    /// 反向锁定：`open` 与 `from_registry` 是两条不同的装配路径，
    /// 断言前者的 ready 不会掩盖后者仍然是 absent。
    #[test]
    fn gateway_without_fulltext_reports_absent_not_ready() {
        let gw = gw_with(vec![]);
        let h = gw.health();
        let ft = h.retrieval.iter().find(|s| s.stage == "full_text").unwrap();
        assert_eq!(ft.state, "absent", "没挂全文层必须报 absent");
        assert!(gw.fulltext_dir().is_none());
    }


    // ── 向量层接入：健康检查必须如实 ─────────────────────────────
    //
    // 这一组守的是任务里那条最容易被糊弄的要求：
    //   服务在 → vector: ready（**带模型名与维度**）；
    //   服务停 → vector: failed/absent（**带原因**）；
    //   **不许假装 ready**。
    //
    // 用真的回环 socket，不用 mock：健康检查最关键的失败模式
    // 恰恰是「HTTP 通了但服务没就绪」，mock 掉就把这一格测没了。

    use ac_retrieval::vector::testing::{dead_endpoint, FakeEmbedder, Mode};

    fn vec_manifests() -> Vec<CapabilityManifest> {
        let mut a = manifest("s::seq", "画时序图", Authority::Read, AssetKind::Skill);
        a.description = "把消息流转画成时序图".into();
        let mut b = manifest("s::flow", "画流程图", Authority::Read, AssetKind::Skill);
        b.description = "流程图与分支判断".into();
        vec![a, b]
    }

    fn gw_with_vector(
        tag: &str,
        endpoint: &str,
        timeout_ms: u64,
        manifests: Vec<CapabilityManifest>,
    ) -> CapabilityGateway {
        let reg = Registry::open_in_memory().expect("内存库");
        for m in &manifests {
            reg.upsert(m).expect("upsert");
        }
        let base = CapabilityGateway::from_registry(reg).expect("网关");
        let _ = tag;
        let cfg = ac_retrieval::vector::VectorConfig {
            endpoint: endpoint.to_string(),
            timeout: std::time::Duration::from_millis(timeout_ms),
            build_timeout: std::time::Duration::from_millis(10_000),
            cooldown: std::time::Duration::from_millis(0),
        };
        base.with_vector(Arc::new(ac_retrieval::vector::VectorIndex::attach(cfg)))
    }

    fn vector_stage<'a>(h: &'a HealthReport) -> &'a StageHealth {
        h.retrieval
            .iter()
            .find(|s| s.stage == "vector")
            .expect("健康检查必须提到 vector")
    }

    /// 服务在 → ready，且 detail 必须带**可核对的**模型名与维度。
    #[test]
    fn vector_health_reports_ready_with_model_and_dim_when_service_is_up() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let gw = gw_with_vector("vec-ready", &svc.endpoint, 2000, vec_manifests());
        let h = gw.health();
        let v = vector_stage(&h);
        assert_eq!(v.state, "ready", "服务在时必须报 ready，实际 {}（{}）", v.state, v.detail);
        // 「ready」三个字母没有信息量。必须有模型名与维度可供核对。
        assert!(
            v.detail.contains("fake-embedder"),
            "ready 时必须如实报模型名，实际：{}",
            v.detail
        );
        assert!(
            v.detail.contains(&ac_retrieval::vector::testing::TEST_DIM.to_string()),
            "ready 时必须如实报维度，实际：{}",
            v.detail
        );
        assert!(
            v.detail.contains("127.0.0.1"),
            "detail 必须带端点，排查时第一件事就是知道它连哪儿，实际：{}",
            v.detail
        );
    }

    /// 服务停 → **必须** failed 且带原因，绝不 ready。
    #[test]
    fn vector_health_reports_failed_with_reason_when_service_is_down() {
        let gw = gw_with_vector("vec-down", &dead_endpoint(), 300, vec_manifests());
        let h = gw.health();
        let v = vector_stage(&h);
        assert_eq!(
            v.state, "failed",
            "服务停时必须报 failed 而不是 ready，实际 {}（{}）",
            v.state, v.detail
        );
        assert!(!v.detail.trim().is_empty(), "失败必须带原因，不能只有状态");
        assert!(
            v.detail.contains("127.0.0.1"),
            "失败原因必须带端点，实际：{}",
            v.detail
        );
        // 整体状态不得是 ok —— 有阶段故障时把它报成 ok 就是自我美化。
        assert_ne!(h.status, "ok", "有阶段故障时整体不该是 ok");
    }

    /// 端口通、HTTP 200，但模型还在加载 → 必须 failed。
    ///
    /// 这是「不许假装 ready」最硬的一格：只看连得上的实现会在这里变绿。
    #[test]
    fn vector_health_does_not_pretend_ready_while_model_is_loading() {
        let svc = FakeEmbedder::start(Mode::Loading);
        let gw = gw_with_vector("vec-loading", &svc.endpoint, 2000, vec_manifests());
        let h = gw.health();
        let v = vector_stage(&h);
        assert_eq!(
            v.state, "failed",
            "端口通但模型没加载好，必须报 failed，实际 {}（{}）",
            v.state, v.detail
        );
        assert!(
            v.detail.contains("loading"),
            "原因必须带上服务自报状态，实际：{}",
            v.detail
        );
    }

    /// **服务从在到停**：健康检查必须跟着变红。
    ///
    /// 这条专治「健康检查复用上一次判定」的实现：那种实现在服务停掉之后
    /// 只要没人查询，就会一直报 ready。
    #[test]
    fn vector_health_turns_red_when_the_service_actually_stops() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let endpoint = svc.endpoint.clone();
        let gw = gw_with_vector("vec-updown", &endpoint, 300, vec_manifests());
        assert_eq!(vector_stage(&gw.health()).state, "ready", "前置条件：先得是 ready");

        drop(svc); // 真的把服务停掉
        // 给 socket 一点时间真正释放。这里等的是**操作系统**归还端口，
        // 不是等我们的代码 —— 后面的断言才是证据。
        std::thread::sleep(std::time::Duration::from_millis(120));

        let h = gw.health();
        let v = vector_stage(&h);
        assert_eq!(
            v.state, "failed",
            "服务停掉后健康检查必须立刻变红，实际 {}（{}）—— 报 ready 就是假装就绪",
            v.state, v.detail
        );
    }

    /// 服务停掉时，**查询必须仍然返回结构化加 BM25 的结果**，
    /// 且 degradation 里如实记 vector 失败并带原因。
    #[test]
    fn search_still_works_and_reports_vector_failure_when_service_is_down() {
        let gw = gw_with_vector("vec-search-down", &dead_endpoint(), 300, vec_manifests());
        let r = gw
            .search(&SearchParams {
                q: Some("时序图".into()),
                ..Default::default()
            })
            .expect("search 绝不该失败");
        assert!(!r.items.is_empty(), "向量层挂了也必须返回结构化结果");
        // degradation 里必须有 vector，且**不是** ready。
        let vd = r
            .degradation
            .iter()
            .find(|d| d.starts_with("vector:"))
            .unwrap_or_else(|| panic!("降级说明里必须出现 vector，实际 {:?}", r.degradation));
        assert!(
            !vd.contains("ready"),
            "向量层失败时绝不能在降级说明里出现 ready，实际：{vd}"
        );
        assert!(vd.contains("failed"), "必须如实记 failed，实际：{vd}");
        // 原因不能是空的 —— 「failed: 」后面什么都没有等于没归因。
        assert!(
            vd.len() > "vector: failed".len() + 2,
            "失败必须带原因，实际：{vd}"
        );
    }

    /// 服务就绪时，向量层必须真的参与（degradation 里不出现 vector 故障）。
    #[test]
    fn search_uses_the_vector_layer_when_the_service_is_up() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let gw = gw_with_vector("vec-search-up", &svc.endpoint, 2000, vec_manifests());
        // 先预热一次，让后台建表把表建好（首查按设计是降级的）。
        let _ = gw.search(&SearchParams {
            q: Some("时序图".into()),
            ..Default::default()
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut r = gw
            .search(&SearchParams {
                q: Some("时序图".into()),
                ..Default::default()
            })
            .expect("search");
        while std::time::Instant::now() < deadline
            && r.degradation.iter().any(|d| d.starts_with("vector:"))
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
            r = gw
                .search(&SearchParams {
                    q: Some("时序图".into()),
                    ..Default::default()
                })
                .expect("search");
        }
        assert!(!r.items.is_empty(), "向量层就绪时当然要有结果");
        assert!(
            !r.degradation.iter().any(|d| d.starts_with("vector:")),
            "向量层参与时不得出现 vector 的降级说明，实际 {:?}",
            r.degradation
        );
    }

    /// 后台建表期间，健康检查必须报 ready 但 detail 如实写明「已入表 0 条」。
    ///
    /// 服务**确实**是就绪的（探活通、模型在），只是表还在建。
    /// 把它报成 failed 是过度告警，报成 ready 而不提建表又是粉饰。
    /// 所以：状态 ready（服务可用），detail 必须写清「已入表 0 条」——
    /// 那个 0 就是「还没建好」的可核对证据。
    #[test]
    fn vector_health_reports_ready_but_zero_vectors_before_the_table_is_built() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let gw = gw_with_vector("vec-lazy", &svc.endpoint, 2000, vec_manifests());
        let h = gw.health();
        let v = vector_stage(&h);
        assert_eq!(
            v.state, "ready",
            "服务可达就必须 ready（可选层本身是好的），实际 {}（{}）",
            v.state, v.detail
        );
        // 关键：必须如实报出「入表 0 条」，而不是含糊地宣称一切正常。
        assert!(
            v.detail.contains("已入表 0 条"),
            "建表前必须如实报 0 条，实际：{}",
            v.detail
        );
    }

    #[test]
    fn version_info_is_populated() {
        let v = VersionInfo::current();
        assert_eq!(v.api, "v1");
        assert_eq!(v.crate_name, "ac-mcp");
    }

    #[test]
    fn invalid_min_authority_is_rejected_not_ignored() {
        let gw = gw_with(vec![]);
        assert!(matches!(
            gw.search(&SearchParams {
                min_authority: Some("superuser".into()),
                ..Default::default()
            }),
            Err(GatewayError::Unavailable(_))
        ));
    }
}
