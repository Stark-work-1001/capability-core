//! 协调服务层：把网关（只读查询）、路由（分配）与审计（落库）串成一条链路。
//!
//! # 这一层做什么
//!
//! 对外接口的每个读操作都走 `CapabilityService`：
//!
//! 1. 用 `Caller` 做**能力裁剪**（谁能看到什么）；
//! 2. 必要时向 `Router` **申请路由**（谁现在能用）；
//! 3. 把这次调用**落审计**（谁在什么时候调了哪个能力）。
//!
//! MCP 与 HTTP 两个传输层只负责协议，不含任何业务判断，因此两条链路
//! 对同一请求给出完全一致的裁剪与审计结果。
//!
//! # 审计为什么写在这一层而不是传输层
//!
//! 因为"谁"这个信息来自 `Caller`，而 `Caller` 是协议解析出来的。
//! 若审计写在传输层，MCP 与 HTTP 就要各写一遍——两份审计实现迟早漏记一类调用。
//! 落库目标是 M2.5 已有的 `agent_capability_usage`。

use crate::caller::{Caller, CallerIdentity, VisibilityStats};
use crate::gateway::{
    CapabilityDetail, CapabilityGateway, GatewayError, HealthReport, SearchParams, SearchResult,
};
use crate::router::{Allocation, Lease, LeaseMode, Router};
use ac_core::CapabilityIdentity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// 身份解析结果。**未注册一律显式拒绝，不静默退化成匿名。**
///
/// 存在的理由：HTTP 侧曾把 `agent` 参数解析出来却从不使用，`route()`
/// 固定返回匿名调用方。于是多智能体裁剪只在 MCP 侧生效，HTTP 侧所有调用
/// 都是匿名档——一半的协调能力是假的。更糟的是它**静默**：调用方传了
/// 一个写权限的 agent_id，拿到的是一份被匿名收窄的结果，既不报错，
/// 也看不出被裁剪过。把错误挡在门口，至少让它变成一条可查的 400。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerResolution {
    /// 解析成功（具名或匿名）。
    Resolved(Caller),
    /// 带了 agent 参数，但该 agent **未注册**。
    Unregistered(String),
    /// 解析过程中枢自身出错（注册表锁中毒 / SQL 失败）。
    Unavailable(String),
}

impl std::fmt::Display for CallerResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallerResolution::Resolved(c) => write!(f, "resolved({})", c.agent_id),
            CallerResolution::Unregistered(a) => write!(f, "agent_unregistered({a})"),
            CallerResolution::Unavailable(m) => write!(f, "unavailable({m})"),
        }
    }
}

impl CallerResolution {
    /// 取调用方；未解析出来就转成一条**带原因**的错误。
    pub fn into_caller(self) -> Result<Caller, GatewayError> {
        match self {
            CallerResolution::Resolved(c) => Ok(c),
            CallerResolution::Unregistered(a) => Err(GatewayError::UnregisteredCaller(format!(
                "智能体 {a} 未注册：请先调用 register（assets-serve register --id {a}）再带上 agent={a}"
            ))),
            CallerResolution::Unavailable(m) => Err(GatewayError::Unavailable(m)),
        }
    }

    /// 便捷取法：解析成功返回调用方，否则把原因渲染成字符串。
    pub fn caller_or_reason(self) -> Result<Caller, String> {
        match self {
            CallerResolution::Resolved(c) => Ok(c),
            CallerResolution::Unregistered(a) => Err(format!(
                "智能体 {a} 未注册：请先调用 register 再带上 agent={a}"
            )),
            CallerResolution::Unavailable(m) => Err(format!("解析调用方身份失败：{m}")),
        }
    }
}

/// 带裁剪的检索结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrimmedSearchResult {
    pub items: Vec<crate::gateway::CapabilityRow>,
    /// 裁剪前的候选数（库视角，未受权限限制）。
    pub total: u32,
    /// 本调用方实际可见数。
    pub visible: u32,
    /// 被裁掉数。
    pub denied: u32,
    /// 裁剪原因分布，键是稳定原因码。
    pub denied_reasons: BTreeMap<String, u32>,
    /// 本次查询的检索降级说明（缺席/故障的阶段）。
    pub degradation: Vec<String>,
    pub caller: String,
}

/// 一次已审计的调用结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audited<T> {
    pub result: T,
    /// 审计表里该智能体的累计调用次数。
    pub call_count: i64,
    pub lease: Option<Lease>,
}

/// 默认返回的源文件字节上限。超过即截断并标记 `truncated`。
///
/// 定 256 KiB 的理由：最大的 SKILL.md 也就几十 KB，256 KiB 足够覆盖正常
/// 技能文档；真有超大文件时，调用方可以用 `?max_bytes=` 显式要更多——
/// 而默认值必须小到不会让一次 HTTP 响应把内存和网络打爆。
pub const DEFAULT_CONTENT_MAX_BYTES: u64 = 256 * 1024;

/// 单次 `max_bytes` 的硬上限，防止调用方要 1 GB。
pub const MAX_CONTENT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// 能力源文件全文的响应体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityContent {
    pub id: String,
    /// 解析后的绝对路径（便于调用方核对读的是哪一份）。
    pub path: String,
    /// 文件真实总字节数。
    pub bytes: u64,
    /// 本次实际返回的字节数（被截断时小于 `bytes`）。
    pub returned_bytes: u64,
    pub truncated: bool,
    /// 资产种类（skill / plugin / mcp / ...）。
    pub source_kind: String,
    pub content: String,
}

/// 把库里存的 `source_path` 解析成真实绝对路径，并**拒绝越界**。
///
/// `source_path` 支持受控目录中的绝对路径或相对**库父目录**的路径，
/// 不是相对 scan_root —— 这是踩过的坑，这里以库父目录为基准再规范化为绝对路径。
///
/// 返回 `None` 的条件（任一即拒）：
/// - 规范化后仍然无法得到绝对路径；
/// - 结果不在项目根之内（路径穿越 / 软链逃逸）。
///
/// 为什么宁可在这里返回 `None` 也不 `unwrap`：这条路径来自数据库内容，
/// 属于外部数据；一旦有人往库里塞一条 `../../../../etc/passwd`，直接读就是
/// 任意文件读取漏洞。
pub fn resolve_source_path(db_parent: &std::path::Path, source_path: &str) -> Option<std::path::PathBuf> {
    let base=if db_parent.is_absolute(){db_parent.to_path_buf()}else{std::env::current_dir().ok()?.join(db_parent)};
    let base=normalize_lexically(&base)?;
    let raw=std::path::Path::new(source_path);
    let normalized=normalize_lexically(&if raw.is_absolute(){raw.to_path_buf()}else{base.join(raw)})?;
    let project=base.parent()?;
    let roots=[project.join("vendor"),project.join("assets"),base.join("capabilities")];
    let allowed=roots.iter().find(|root|normalized.starts_with(root))?;
    // Existing source files must stay inside the corresponding approved tree
    // after resolving links. Missing files still return a path for diagnosis.
    if normalized.exists() {
        let actual=normalized.canonicalize().ok()?;
        let boundary=allowed.canonicalize().ok()?;
        if !actual.starts_with(boundary){return None;}
    } else {
        for ancestor in normalized.ancestors() {
            if ancestor.exists() {
                let actual=ancestor.canonicalize().ok()?;
                if let Ok(boundary)=allowed.canonicalize(){if !actual.starts_with(boundary){return None;}}
                break;
            }
        }
    }
    Some(normalized)
}

#[cfg(test)]mod source_boundary_tests {
    use super::*;
    #[test]fn registered_sources_allow_absolute_and_relative_paths_inside_asset_trees_only(){
        let root=std::env::current_dir().unwrap().join("target/source-boundary-test");let data=root.join("data");
        let vendor=root.join("vendor/skill/demo/SKILL.md");let managed=data.join("capabilities/skill/demo/rev/SKILL.md");
        assert!(resolve_source_path(&data,&vendor.to_string_lossy()).is_some());
        assert!(resolve_source_path(&data,&managed.to_string_lossy()).is_some());
        assert!(resolve_source_path(&data,"../vendor/skill/demo/SKILL.md").is_some());
        assert!(resolve_source_path(&data,"llm-providers.json").is_none());
        assert!(resolve_source_path(&data,"../../private.txt").is_none());
        assert!(resolve_source_path(&data,"../vendor/../../private.txt").is_none());
    }
}

/// 纯词法规范化：消掉 `.` 与 `..`，不碰文件系统（不解析软链、不要求存在）。
///
/// 不用 `canonicalize` 的理由：它要求路径存在，而"文件不存在"本身
/// 是需要如实报告的一种状态，不该在规范化阶段就混成 `None`。
fn normalize_lexically(p: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut out = std::path::PathBuf::new();
    for comp in p.components() {
        use std::path::Component::*;
        match comp {
            Prefix(x) => {
                #[cfg(windows)]
                if let std::path::Prefix::VerbatimDisk(d)=x.kind(){out.push(format!("{}:",d as char));continue;}
                out.push(x.as_os_str());
            },
            RootDir => out.push(std::path::MAIN_SEPARATOR.to_string()),
            CurDir => {}
            ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Normal(x) => out.push(x),
        }
    }
    Some(out)
}

/// 能力协调服务。
pub struct CapabilityService {
    pub scheduler: Arc<crate::scheduler::Scheduler>,
    gateway: Arc<CapabilityGateway>,
    router: Arc<Router>,
}

impl CapabilityService {
    pub fn new(gateway: Arc<CapabilityGateway>) -> Self {
        Self {
            scheduler: crate::scheduler::global(),
            gateway,
            router: Arc::new(Router::new()),
        }
    }

    pub fn gateway(&self) -> &CapabilityGateway {
        &self.gateway
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    /// 资产身份投影。取不到时按**最宽松**处理是不安全的，
    /// 按最严处理又会让"库里新增但还没解析"的资产全部消失；
    /// 因此取不到就返回 `None`，由调用方显式决定。
    fn identity_of(&self, asset_id: &str) -> Option<CapabilityIdentity> {
        self.gateway.identity_of_asset(asset_id)
    }

    /// 判断一条结果对该调用方是否可见，并记录原因。
    fn admit_row(&self, caller: &Caller, row: &crate::gateway::CapabilityRow) -> Option<&'static str> {
        // 资产身份缺失时退回"只读可见"：本轮对外接口本身只读，
        // 身份不可得的资产不该凭空对所有人隐身，但也不会因此获得写权限。
        if let Some(id) = self.identity_of(&row.id) {
            if let Some(reason) = caller.admits(&id) {
                return Some(reason);
            }
        } else if let Some(reason) = caller.admits(&CapabilityIdentity {
            // 身份不可得时按目标侧最严维度评估：只读身份对只读资产才放行。
            authority: ac_core::Authority::Read,
            ..Default::default()
        }) {
            return Some(reason);
        }
        // 身份层放行后再过白/黑名单（这两层只看 kind/tag/ecosystem）。
        if !caller
            .visibility
            .allows_assets(&row.kind, &row.tags, &row.ecosystem)
        {
            return Some("not_granted");
        }
        if caller
            .visibility
            .blocks_assets(&row.kind, &row.tags, &row.ecosystem)
        {
            return Some("denied_by_policy");
        }
        None
    }

    /// 按调用方身份裁剪后的检索。
    ///
    /// 裁剪在**网关返回之后、本地过滤之前**进行：先取候选（走 SQL 索引），
    /// 再按身份逐条判定。这样裁剪规则变更不需要动 SQL，且与传输层解耦。
    pub fn search(
        &self,
        caller: &Caller,
        p: &SearchParams,
    ) -> Result<TrimmedSearchResult, GatewayError> {
        let res: SearchResult = self.gateway.search(p)?;
        let mut stats = VisibilityStats::default();
        let mut items = Vec::new();
        for row in res.items.iter() {
            stats.examine();
            match self.admit_row(caller, row) {
                None => {
                    stats.record_visible();
                    items.push(row.clone());
                }
                Some(reason) => stats.record_deny(reason),
            }
        }
        Ok(TrimmedSearchResult {
            items,
            total: res.total,
            visible: stats.visible,
            denied: stats.denied,
            denied_reasons: stats.denied_reasons,
            degradation: res.degradation,
            caller: caller.agent_id.clone(),
        })
    }

    /// 取详情（带裁剪）。被裁掉时返回 `NotFound`——不泄露"存在但无权"。
    ///
    /// 这条是**有意的信息隐藏**：若用 403 区分"存在但无权"，外部智能体就能
    /// 用遍历 id 的方式把整个库的存在性摸出来，裁剪就等于没有。
    pub fn detail(&self, caller: &Caller, id: &str) -> Result<CapabilityDetail, GatewayError> {
        let detail = self.gateway.detail(id)?;
        let denied = self
            .identity_of(&detail.manifest.id)
            .map(|idn| caller.admits(&idn).is_some())
            .unwrap_or_else(|| {
                caller
                    .admits(&CapabilityIdentity {
                        authority: ac_core::Authority::Read,
                        ..Default::default()
                    })
                    .is_some()
            });
        let policy_denied = caller.visibility.blocks_assets(
            &detail.manifest.kind.as_str(),
            &detail.tags,
            &detail.ecosystem,
        ) || !caller.visibility.allows_assets(
            &detail.manifest.kind.as_str(),
            &detail.tags,
            &detail.ecosystem,
        );
        if denied || policy_denied {
            return Err(GatewayError::NotFound(format!("能力 {id} 不可见")));
        }
        Ok(detail)
    }

    /// 读取能力的**源文件全文**（技能全文接口）。
    ///
    /// 为什么放在 service 层而不是 http/mcp：两个传输层必须给出同一份裁剪与
    /// 审计结果。若在传输层各写一遍，MCP 与 HTTP 迟早对"能不能读这个文件"
    /// 给出不同答案——这正是 HTTP 身份未接线时踩过的坑。
    ///
    /// 安全：只读；路径必须在项目根之内（穿越即拒）；越权与不存在都返回
    /// NotFound，**不区分**"没这个能力"与"你看不到"，避免探测能力清单。
    pub fn content(
        &self,
        caller: &Caller,
        id: &str,
        max_bytes: Option<u64>,
    ) -> Result<CapabilityContent, GatewayError> {
        let detail = self.detail(caller, id)?;

        let source = detail
            .manifest
            .provenance
            .source_path
            .clone()
            .ok_or_else(|| GatewayError::NotFound(format!("能力 {id} 没有源文件路径")))?;

        let root = self.gateway.project_root();
        let path = resolve_source_path(&root, &source).ok_or_else(|| {
            GatewayError::NotFound(format!("能力 {id} 的源文件路径不可解析或越界：{source}"))
        })?;

        let meta = std::fs::metadata(&path)
            .map_err(|e| GatewayError::NotFound(format!("源文件不可读（{e}）：{}", path.display())))?;
        let total = meta.len();

        let limit = max_bytes.unwrap_or(DEFAULT_CONTENT_MAX_BYTES);
        let truncated = total > limit;

        let bytes = std::fs::read(&path)
            .map_err(|e| GatewayError::Unavailable(format!("读取源文件失败（{e}）：{}", path.display())))?;
        let take = std::cmp::min(bytes.len() as u64, limit) as usize;
        let slice = &bytes[..take];

        // 在 UTF-8 边界处截断，避免把多字节字符劈成半个。
        let text = match std::str::from_utf8(slice) {
            Ok(s) => s.to_string(),
            Err(e) => {
                let valid = e.valid_up_to();
                String::from_utf8_lossy(&slice[..valid]).to_string()
            }
        };

        let source_kind = detail.manifest.kind.as_str().to_string();
        Ok(CapabilityContent {
            id: detail.manifest.id.clone(),
            path: path.display().to_string(),
            bytes: total,
            returned_bytes: take as u64,
            truncated,
            source_kind,
            content: text,
        })
    }

    /// 标签清单。**只统计该调用方可见资产上的标签**。
    ///
    /// 直接把全库标签交给调用方等于泄露：某个标签只存在于高权限资产上时，
    /// 它本身就是一条侧信道。所以这里按可见集合重算计数。
    pub fn tags(&self, caller: &Caller) -> Result<Vec<ac_registry::TagCount>, GatewayError> {
        let all = self.gateway.tags()?;
        let visible = self.visible_assets(caller)?;
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        for (_asset_id, tags) in visible {
            for t in tags {
                *counts.entry(t).or_insert(0) += 1;
            }
        }
        Ok(all
            .into_iter()
            .filter(|t| counts.contains_key(&t.tag))
            .map(|t| {
                // count 只用可见集合重算的值；库内计数对被裁掉的资产没有意义。
                let count = counts.get(&t.tag).copied().unwrap_or(0);
                ac_registry::TagCount { tag: t.tag, count }
            })
            .collect())
    }

    /// 该调用方可见的 (asset_id, tags)。分页上限 1000，与网关 clamp 一致。
    fn visible_assets(
        &self,
        caller: &Caller,
    ) -> Result<Vec<(String, Vec<String>)>, GatewayError> {
        let res = self.gateway.search(&SearchParams {
            limit: Some(1000),
            ..Default::default()
        })?;
        let mut out = Vec::new();
        for row in res.items {
            if self.admit_row(caller, &row).is_none() {
                let tags = self.gateway.tags_of(&row.id);
                out.push((row.id.clone(), tags));
            }
        }
        Ok(out)
    }

    /// 健康自检（与调用方无关，任何接入方都能探）。
    pub fn health(&self) -> HealthReport {
        self.gateway.health()
    }

    // ── 身份解析：MCP 与 HTTP **唯一**的共用入口 ───────────────────

    /// 由 agent_id 还原调用方。**MCP 与 HTTP 都必须走这里**。
    ///
    /// # 为什么放在这一层而不是各传输层各写一份
    ///
    /// 这是本轮接线的全部要害。两条链路一旦各写一份身份解析，
    /// 它们就会在某次改动后给出不同的可见集，而「同一个 agent
    /// 在 MCP 看得见、在 HTTP 看不见」是最难排查的一类故障——
    /// 因为两条链路都「看起来正常」。把它们钉在同一个函数上，
    /// 跨链路一致性就从一个需要人工核对的不变量变成一条编译期事实。
    ///
    /// # 三种结局
    ///
    /// | 输入 | 结局 | 为什么 |
    /// |---|---|---|
    /// | 空 | 匿名（observer 档） | 不带身份是最常见情形，且只读接口下最安全 |
    /// | 已注册 | 该 agent 的身份投影 | **按其身份维度收窄** |
    /// | 未注册 | `Unregistered`（显式拒绝） | 见下，绝不静默退化 |
    ///
    /// # 为什么未注册要拒绝而不是退化成匿名
    ///
    /// 退化看起来更「宽容」，实际是最坏的处置：调用方拼错了一个
    /// agent_id，会拿到一份**看起来完全正常**的匿名结果集，
    /// 既没有 4xx 也没有任何提示。于是「这个 agent 为什么看不到
    /// 写权限的能力」永远查不到根因——排查会去查权限，
    /// 而错的是「它压根没注册」。
    pub fn resolve_caller(&self, agent_id: &str) -> CallerResolution {
        let agent_id = agent_id.trim();
        if agent_id.is_empty() {
            return CallerResolution::Resolved(Caller::anonymous());
        }
        // 显式保留匿名这个保留名：带 agent=anonymous 视为「我要匿名档」，
        // 而不是「我是名叫 anonymous 的智能体」。否则一个真的叫这名字的
        // 智能体永远无法注册（注册就与保留名冲突）。
        if agent_id.eq_ignore_ascii_case(Caller::ANONYMOUS_ID) {
            return CallerResolution::Resolved(Caller::anonymous());
        }
        // with_registry 的外层 Result 是锁中毒、内层是 SQL 失败。
        // 两者都**如实变成 Unavailable**，不得当成「查不到」——
        // 库读不出来时若退化成匿名，那就是本模块开头说的那个静默失败，
        // 而且这次连配置错误都不是，纯粹是中枢自己坏了。
        let records = match self.gateway.with_registry(|g| g.list_agents()) {
            Ok(Ok(v)) => v,
            Ok(Err(sql_err)) => {
                return CallerResolution::Unavailable(format!("读取智能体表失败: {sql_err}"))
            }
            Err(lock_err) => return CallerResolution::Unavailable(lock_err.to_string()),
        };
        match records.into_iter().find(|a| a.agent_id == agent_id) {
            Some(rec) => CallerResolution::Resolved(self.caller_from_record(&rec)),
            None => CallerResolution::Unregistered(agent_id.to_string()),
        }
    }

    /// 把一条 `agents` 表记录投影成调用方。
    ///
    /// 投影规则写在**一处**，两条链路共用；这也是「按其身份维度收窄」
    /// 的具体口径：
    ///
    /// 1. 安全三维取 observer 档（authority=observe / side_effect=none /
    ///    tool_posture=reader）。理由与 `Caller::anonymous` 同源：
    ///    `agents` 表**没有**权限列，凭空造一个 write 档等于自我提权。
    ///    这一档仍能看见 Observe 档资产，因此接口可用。
    /// 2. **生态维度**由该 agent 的 `ecosystem` 声明 → 收窄到同生态资产。
    ///    这是「按其身份维度收窄」里唯一真实生效的一维：库内资产没有
    ///    ecosystem 列，但每个资产都有 `source_path`，而
    ///    `infer_ecosystem` 就是从它推生态的（agents.rs 的 C 通道）。
    ///    于是生态收窄有真实数据支撑，而不是凭空的一个条件。
    ///
    /// 3. `unassigned` **不作为收窄条件**：该键表示「确实没带宿主标记」，
    ///    拿它当筛选会让这类 agent 看到空集——那是把「不知道属于谁」
    ///    误当成「不属于我」，与 caller.rs 里对 dimensions 的同一条纪律相悖。
    fn caller_from_record(&self, rec: &ac_registry::AgentRecord) -> Caller {
        let ecosystem = rec.ecosystem.trim().to_ascii_lowercase();
        let caller = Caller::new(rec.agent_id.clone(), CallerIdentity::observer())
            .with_display_name(&rec.display_name);
        // 生态收窄走 **Visibility.allow**，**不走** CallerIdentity.dimensions。
        //
        // 踩过的坑值得记下来：曾把 `ecosystem=qoder` 放进 identity.dimensions，
        // 结果一个可见项都没有（denied_reasons 全是 identity_insufficient）。
        // 原因是 `CallerIdentity::admits` 的 dimensions 规则是
        // 「调用方声明的每个键都必须与**目标资产 identity 里的 dimensions** 一致」，
        // 而资产 identity 的 dimensions 装的是 `cost_tier` / `domain`——
        // **从来没有 `ecosystem` 这个键**。于是声明一个目标侧不存在的键，
        // 必然全部不匹配。
        //
        // 生态是**行级属性**（由 source_path 推断，见 gateway::ecosystem_of_asset），
        // 归 `Grant` 判定才对：它本来就按 (kind, tags, ecosystem) 匹配。
        if ecosystem.is_empty() || ecosystem == ac_registry::agents::ECOSYSTEM_UNASSIGNED {
            // `unassigned` 不作为收窄条件：它表示「确实没带宿主标记」，
            // 拿它筛等于把「不知道属于谁」当成「不属于我」，会让这类
            // agent 看到空集。与 caller.rs 里对 dimensions 的同一条纪律一致。
            return caller;
        }
        caller.with_visibility(crate::caller::Visibility {
            allow: vec![crate::caller::Grant {
                kind: None,
                tag: None,
                ecosystem: Some(ecosystem),
            }],
            deny: Vec::new(),
        })
    }

    // ── 协调元数据写入（**不是**用户资产）──────────────────────

    /// 注册 / 更新智能体。落库语义与桌面端 M2.5 的 register_agent 完全一致。
    pub fn register_agent(&self, reg: &ac_registry::AgentRegistration) -> Result<String, String> {
        self.gateway
            .with_registry(|g| g.register_agent(reg))
            .map_err(|e| e.to_string())?
    }

    /// 上报一次能力调用，落 `agent_capability_usage`，返回累计次数。
    ///
    /// **未注册的 agent_id 一律报错**（继承 M2.5 的语义）：绝不因为
    /// 一个拼错的 id 就凭空造出"智能体"，否则总览的在线数会变成不可信数字。
    pub fn report_usage(
        &self,
        agent_id: &str,
        capability_id: &str,
        ok: bool,
        error: Option<&str>,
    ) -> Result<i64, String> {
        self.gateway
            .with_registry(|g| g.report_capability_usage(agent_id, capability_id, ok, error))
            .map_err(|e| e.to_string())?
    }

    /// 取一次调用：分配路由 + 落审计 + 返回详情。**这是对外接口的主链路。**
    ///
    /// 顺序刻意是「先分配、再审计」：审计必须能回答"这次调用到底拿到许可没有"，
    /// 反过来就少了一维可核对的事实。
    pub fn acquire_and_audit(
        &self,
        caller: &Caller,
        capability_id: &str,
        ok: bool,
        error: Option<&str>,
    ) -> Result<Audited<Allocation>, GatewayError> {
        // 先落审计再分配：审计是这次调用的**准入凭证**。
        // 顺序反过来（先分配后审计）时，未注册智能体会先拿到内存里的租约、
        // 再在写库时失败——于是"没被授权"却短暂占住了路由位，
        // 而调用方看到的是一个被吞掉的错误。
        //
        // 这里让审计失败**直接冒泡**：M2.5 已定的契约是"未注册一律报错，
        // 绝不顺手建一行 agents"，否则任何拼错的 id 都会凭空造出智能体。
        let call_count = self
            .report_usage(&caller.agent_id, capability_id, ok, error)
            .map_err(|e| GatewayError::Unavailable(format!("审计落库失败（智能体可能未注册）: {e}")))?;
        let lease = self.router.acquire(
            capability_id,
            &caller.agent_id,
            LeaseMode::Shared,
            caller.max_concurrent,
        );
        // 无论拿到许可与否都上报当前分配快照：排队方需要知道自己在谁后面。
        let snapshot = self.router.allocation(capability_id);
        Ok(Audited {
            result: snapshot,
            call_count,
            lease: lease.ok(),
        })
    }

    /// 释放路由。
    pub fn release(&self, capability_id: &str, agent_id: &str) -> bool {
        self.router.release(capability_id, agent_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caller::{CallerIdentity, Grant, Visibility};
    use ac_core::{AssetKind, Authority, CapabilityManifest, ProvidedCapability};
    use ac_retrieval::store::AssetFilter;

    fn seed() -> CapabilityGateway {
        let reg = ac_registry::Registry::open_in_memory().expect("内存库");
        for (id, name, kind, auth) in [
            ("s::read", "只读技能", AssetKind::Skill, Authority::Read),
            ("s::write", "写入技能", AssetKind::Skill, Authority::Write),
            ("m::read", "只读工具", AssetKind::Mcp, Authority::Read),
        ] {
            let mut m = CapabilityManifest::new(id, kind, name);
            m.description = name.to_string();
            m.identity.authority = auth;
            m.provides.push(ProvidedCapability {
                capability_id: format!("{id}::main"),
                name: name.to_string(),
                summary: name.to_string(),
                input_schema: None,
                tags: vec![if auth == Authority::Read {
                    "read_only".into()
                } else {
                    "write".into()
                }],
            });
            reg.upsert(&m).expect("upsert");
        }
        CapabilityGateway::from_registry(reg).expect("网关")
    }

    fn svc() -> CapabilityService {
        CapabilityService::new(Arc::new(seed()))
    }

    /// `AgentRegistration` 没有 derive(Default)（字段刻意必填），
    /// 所以测试里显式构造——这正是"不 derive Default"想逼出来的显式性。
    fn reg(agent_id: &str) -> ac_registry::AgentRegistration {
        ac_registry::AgentRegistration {
            agent_id: agent_id.to_string(),
            display_name: agent_id.to_string(),
            ecosystem: "test".to_string(),
            kind: "host".to_string(),
            endpoint: String::new(),
            meta: "{}".to_string(),
        }
    }

    #[test]
    fn anonymous_sees_read_assets_but_not_write_ones() {
        let s = svc();
        let r = s
            .search(&Caller::anonymous(), &SearchParams::default())
            .expect("search");
        // 库里只读资产 2 个（s::read 与 m::read），写入的 s::write 被裁。
        assert_eq!(r.visible, 2, "只读资产应为 2 个，实际 {:?}", r.items);
        assert_eq!(r.denied, 1, "写入资产应被裁掉");
        assert!(r.denied_reasons.contains_key("identity_insufficient"));
        let ids: Vec<&str> = r.items.iter().map(|i| i.id.as_str()).collect();
        assert!(!ids.contains(&"s::write"));
    }

    #[test]
    fn allow_list_narrows_visible_set() {
        let s = svc();
        let caller = Caller::new(
            "mcp-agent",
            CallerIdentity {
                authority: Authority::Read,
                ..Default::default()
            },
        )
        .with_visibility(Visibility {
            allow: vec![Grant {
                kind: Some("mcp".into()),
                ..Default::default()
            }],
            deny: vec![],
        });
        let r = s.search(&caller, &SearchParams::default()).expect("search");
        assert_eq!(r.visible, 1);
        assert_eq!(r.items[0].id, "m::read");
    }

    #[test]
    fn two_agents_see_different_subsets() {
        let s = svc();
        let a = Caller::new("agent-a", CallerIdentity::read_only());
        let b = Caller::new(
            "agent-b",
            CallerIdentity {
                authority: Authority::Write,
                ..Default::default()
            },
        );
        let ra = s.search(&a, &SearchParams::default()).expect("a");
        let rb = s.search(&b, &SearchParams::default()).expect("b");
        // b 权限更高，看得更多。
        assert!(rb.visible > ra.visible);
        let ia: Vec<&str> = ra.items.iter().map(|i| i.id.as_str()).collect();
        let ib: Vec<&str> = rb.items.iter().map(|i| i.id.as_str()).collect();
        assert!(!ia.contains(&"s::write"));
        assert!(ib.contains(&"s::write"));
    }

    #[test]
    fn detail_of_denied_asset_is_not_found_not_forbidden() {
        let s = svc();
        // 先把只读技能注册进 agents 表以便审计，但匿名取写入技能详情必须 NotFound。
        s.register_agent(&reg("anonymous")).ok();
        let err = s
            .detail(&Caller::anonymous(), "s::write")
            .expect_err("写入技能对匿名应不可见");
        assert!(
            matches!(err, GatewayError::NotFound(_)),
            "应返回 NotFound 以隐藏存在性，实际 {err:?}"
        );
    }

    #[test]
    fn tags_only_report_visible_assets() {
        let s = svc();
        let anon = s.tags(&Caller::anonymous()).expect("tags");
        // "write" 标签只挂在被裁掉的写入技能上，不应出现。
        assert!(!anon.iter().any(|t| t.tag == "write"), "实际 {anon:?}");
        assert!(anon.iter().any(|t| t.tag == "read_only"));
    }

    #[test]
    fn audit_requires_registration() {
        let s = svc();
        let e = s
            .report_usage("ghost-agent", "s::read::main", true, None)
            .expect_err("未注册必须报错");
        assert!(e.contains("未注册"), "实际 {e}");
    }

    #[test]
    fn acquire_and_audit_rejects_unregistered_agent_instead_of_swallowing() {
        // 回归：曾经用 unwrap_or(0) 吞掉审计失败，让未注册智能体
        // "成功"返回 call_count=0 并占住路由位。那会让拼错的 id
        // 既不被发现、又真的参与了协调。
        let s = svc();
        let caller = Caller::new("ghost-agent", CallerIdentity::read_only());
        let r = s.acquire_and_audit(&caller, "s::read::main", true, None);
        assert!(r.is_err(), "未注册智能体必须被拒绝，实际 {r:?}");
        let e = r.err().unwrap();
        assert!(
            matches!(e, GatewayError::Unavailable(ref m) if m.contains("未注册")),
            "错误应说明原因，实际 {e:?}"
        );
        // 拒绝时不得留下路由占用。
        assert!(
            s.router().allocation("s::read::main").holders.is_empty(),
            "被拒绝的调用不得占住路由"
        );
    }

    #[test]
    fn acquire_and_audit_increments_count() {
        let s = svc();
        s.register_agent(&reg("agent-a")).expect("注册");
        let c = Caller::new("agent-a", CallerIdentity::read_only());
        let a1 = s
            .acquire_and_audit(&c, "s::read::main", true, None)
            .expect("第一次");
        let a2 = s
            .acquire_and_audit(&c, "s::read::main", true, None)
            .expect("第二次");
        assert_eq!(a1.call_count, 1);
        assert_eq!(a2.call_count, 2);
        assert!(a2.lease.is_some());
        assert!(a2.result.holders.contains(&"agent-a".to_string()));
    }

    #[test]
    fn search_passes_through_fulltext_degradation_report() {
        let s = svc();
        let r = s
            .search(&Caller::anonymous(), &SearchParams::default())
            .expect("search");
        // 结构化基线就绪时不应报基线故障。
        assert!(!r.degradation.iter().any(|d| d.starts_with("structured: Failed")));
        // 供回归：过滤参数确实透传到网关。
        let only_mcp = s
            .search(
                &Caller::anonymous(),
                &SearchParams {
                    kind: Some("mcp".into()),
                    ..Default::default()
                },
            )
            .expect("mcp");
        assert_eq!(only_mcp.items.len(), 1);
        let _ = AssetFilter::all();
    }
}
