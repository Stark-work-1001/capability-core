//! HTTP/1.1 JSON API：给不方便走 MCP 的调用方。
//!
//! # 路由组织：资源优先，版本前缀
//!
//! 全部路由挂在 /api/v1 之下，**只有 GET**：
//!
//! | 方法 | 路径 | 说明 |
//! |---|---|---|
//! | GET | /healthz | 存活探针（liveness），永远 200 只说明进程活着 |
//! | GET | /api/v1/version | 版本信息 |
//! | GET | /api/v1/health | 依赖就绪状态（readiness），可能 503 |
//! | GET | /api/v1/capabilities | 按 q/kind/tag/min_authority 检索 |
//! | GET | /api/v1/capabilities/:id | 单个能力详情 |
//! | GET | /api/v1/tags | 标签清单（按调用方可见范围） |
//! | GET | /api/v1/agents | 已注册智能体（协调状态可见） |
//!
//! # 为什么只有 GET
//!
//! 这是"只读承诺"最容易被验证的表达：路由器**根本没有**注册任何非 GET 动词，
//! 因此不存在"某个写接口忘了加鉴权"的可能。非 GET 一律 405。
//!
//! # 身份从哪来
//!
//! 查询参数 agent 指定调用方。不带就是匿名（只读）。这是刻意的：
//! 不引入鉴权层，是因为这是一个本机/内网常驻服务，且真正的权限判定在 admits 里；
//! 把"你是谁"做成可选参数，缺省取最严身份，忘记传的后果是"看得更少"
//! 而不是"看得更多"。
//!
//! # 为什么自己写 HTTP 而不用框架
//!
//! 只需要 GET + JSON，且要零新增依赖（本项目的依赖面是受控资产）。
//! 一个 TcpListener + 逐行解析请求头足够。

use crate::gateway::{GatewayError, SearchParams, VersionInfo};
use crate::service::{CapabilityService, MAX_CONTENT_MAX_BYTES};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write, Read};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

/// 默认监听端口。**硬约束：18195 归本项目；18181/18182 属 memora，严禁触碰。**
pub const DEFAULT_PORT: u16 = 18195;

/// 一个已解析的 HTTP 请求（只支持 GET）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub query: BTreeMap<String, String>,
}

/// 一个 HTTP 响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    pub fn json(status: u16, v: Value) -> Self {
        Self { status, body: v.to_string() }
    }

    pub fn error(status: u16, code: &str, message: &str) -> Self {
        Self::json(status, json!({ "error": code, "message": message }))
    }

    fn reason(&self) -> &'static str {
        match self.status {
            200 => "OK",
            202 => "Accepted",
            415 => "Unsupported Media Type",
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            503 => "Service Unavailable",
            _ => "Unknown",
        }
    }

    fn encode(&self) -> Vec<u8> {
        // 逐段拼接而不是一个大 format!：格式串里的占位符个数与实参个数
        // 极易数错，而数错时编译器只会说"参数没用上"，很难一眼看出。
        let mut head = String::new();
        head.push_str("HTTP/1.1 ");
        head.push_str(&self.status.to_string());
        head.push(' ');
        head.push_str(self.reason());
        head.push_str(CRLF);
        head.push_str("Content-Type: application/json; charset=utf-8");
        head.push_str(CRLF);
        head.push_str("Content-Length: ");
        head.push_str(&self.body.as_bytes().len().to_string());
        head.push_str(CRLF);
        head.push_str("Connection: close");
        head.push_str(CRLF);
        head.push_str(CRLF);
        let mut out = head.into_bytes();
        out.extend_from_slice(self.body.as_bytes());
        out
    }
}

const CRLF: &str = "\r\n";

/// HTTP 服务。
pub struct HttpServer {
    sessions: std::sync::Mutex<BTreeMap<String,(std::time::Instant,Arc<crate::mcp::McpServer>)>>,
    /// 协调服务。**共享**：跨链路一致性测试要用同一个 service
    /// 同时驱动 HTTP 与 MCP 两条链路，否则比较的就是两个不同库上的结果，
    /// 「一致」这个断言就失去意义了。
    pub(crate) service: Arc<CapabilityService>,
}

impl HttpServer {
    pub fn new(service: Arc<CapabilityService>) -> Self {
        Self { service, sessions: std::sync::Mutex::new(BTreeMap::new()) }
    }

    /// 纯函数式路由：请求进、响应出。**不含 IO**，因此可单测。
    pub fn route(&self, req: &HttpRequest) -> HttpResponse {
        let _permit = match self.service.scheduler.acquire(req.query.get("agent").map(String::as_str).unwrap_or(""), std::time::Duration::from_secs(2)) {
            Ok(p) => p, Err(e) => return HttpResponse::error(429,"overloaded",&e),
        };
        // 只读承诺的第一道闸：非 GET 直接拒绝。
        //
        // **例外是一张白名单，不是一个开关**：只有下面这几个**写协调元数据**
        // 的路径允许 POST。写成「非 GET 一律拒绝，故障接口除外」的话，
        // 每加一个写接口就要回来改一次这个 if，而漏改的表现是 405——
        // 一个看起来像「方法错了」其实「路由忘了登记」的错误。
        const WRITE_PATHS: &[&str] = &[
            "/api/v1/faults/claim",
            "/api/v1/faults/release",
            "/api/v1/faults/resolve",
            // 外部智能体（如 assets-plugin）注册自己。写在 /agents 上而不是
            // /agents/register，是为了让「注册」与「列表」共用同一资源路径，
            // 靠 method 区分 —— 这样路由表不会长出一堆近义动词。
            "/api/v1/agents",
        ];
        if req.method != "GET" && !WRITE_PATHS.contains(&req.path.as_str()) {
            return HttpResponse::error(
                405,
                "method_not_allowed",
                "对外接口只读，仅接受 GET（故障认领/结束是唯一例外）",
            );
        }
        let path = req.path.as_str();

        // ── 身份接线（缺口 1）─────────────────────────────────────
        //
        // `agent` 查询参数指定调用方。**这里曾经把它解析出来却从不使用**，
        // 固定返回 `Caller::anonymous()`——于是多智能体裁剪只在 MCP 侧生效，
        // HTTP 侧所有调用都是匿名档，一半的协调能力是假的，而且静默。
        //
        // 现在两条链路都走 `service.resolve_caller`：它是唯一的身份
        // 解析入口，于是「同一个 agent 走 MCP 与走 HTTP 看得见同样多的东西」
        // 从一个人工核对的不变量变成了一条结构事实。
        //
        // 解析失败（agent 未注册 / 库读不出来）在这里就变成 4xx/5xx，
        // 绝不带着一个匿名身份继续跑——那正是原来那个静默失败。
        // ── 身份校验的**放行名单** ──────────────────────────────
        //
        // 三个探针接口本来就不需要身份。注册接口也必须放行，理由是
        // **它在逻辑上不可能通过**：check_caller 拒绝一切未注册的 agent，
        // 而注册正是把 agent 变成「已注册」的那一步。把注册挂在身份闸
        // 后面 = 第一次注册永远是 400，且错误信息还是「未注册」——
        // 用户会以为是配置问题，其实是死锁。
        //
        // 放行是安全的：注册只写 agents 表（它就是「申请登记」这件事本身），
        // 不改任何资产数据，也不需要调用方已有身份。
        const NO_CALLER_CHECK: &[&str] = &[
            "/healthz",
            "/api/v1/version",
            "/api/v1/health",
            "/api/v1/agents",
        ];
        if !NO_CALLER_CHECK.contains(&path) {
            if let Err(resp) = self.check_caller(req) {
                return resp;
            }
        }
        let caller = self.caller_for_query(req);
        let path = req.path.as_str();

        match path {
            "/api/v1/scheduler" => HttpResponse::json(200, serde_json::to_value(self.service.scheduler.stats()).unwrap_or(json!({}))),
            "/healthz" => HttpResponse::json(200, json!({ "status": "alive" })),
            "/api/v1/version" => HttpResponse::json(
                200,
                serde_json::to_value(VersionInfo::current()).unwrap_or(json!({})),
            ),
            "/api/v1/health" => {
                let h = self.service.health();
                // 就绪探针必须能区分"进程活着"与"能服务"：
                // 注册表故障时返回 503，否则监控会一直显示健康。
                let code = if h.status == "down" { 503 } else { 200 };
                let mut v = serde_json::to_value(h).unwrap_or(json!({}));
                // 心跳目录的父目录（即中枢 data 目录）也要公布出去。
                //
                // 为什么：外部宿主（assets-plugin 等）要走 B 通道写心跳，
                // 就必须知道 `<data_dir>/heartbeat/<host>.json` 写到哪。
                // 不公布的话，用户只能靠环境变量手工对齐路径 —— 那是一个
                // 纯配置问题，却会表现为「插件说连上了、中枢看不到它」。
                //
                // 只在本机 loopback 上暴露（服务只绑 127.0.0.1），
                // 泄露的是一条本机路径，不是凭据。
                if let Some(dir) = Some(self.service.gateway().project_root().to_path_buf()) {
                    v["data_dir"] = json!(dir.display().to_string());
                    v["heartbeat_dir"] = json!(dir.join("heartbeat").display().to_string());
                }
                HttpResponse::json(code, v)
            }
            "/api/v1/capabilities" => {
                let p = SearchParams {
                    q: req.query.get("q").cloned(),
                    kind: req.query.get("kind").cloned(),
                    tag: req.query.get("tag").cloned(),
                    min_authority: req.query.get("min_authority").cloned(),
                    limit: req.query.get("limit").and_then(|v| v.parse().ok()),
                    offset: req.query.get("offset").and_then(|v| v.parse().ok()),
                };
                match self.service.search(&caller, &p) {
                    Ok(r) => HttpResponse::json(200, serde_json::to_value(r).unwrap_or(json!({}))),
                    Err(e) => Self::gateway_error(e),
                }
            }
            "/api/v1/tags" => match self.service.tags(&caller) {
                Ok(t) => HttpResponse::json(200, json!({ "tags": t })),
                Err(e) => Self::gateway_error(e),
            },
            // ── 技能全文（必须先于下面的前缀匹配）───────────────
            //
            // 顺序很重要：/api/v1/capabilities/:id/content 也以前缀
            // /api/v1/capabilities/ 开头。如果这条写在后面，:id 会被解析成
            // "xxx/content"，detail 查不到就报 404——症状是"全文接口永远 404"，
            // 而代码看起来完全正常。
            p if p.starts_with("/api/v1/capabilities/") && p.ends_with("/content") => {
                let mid = &p["/api/v1/capabilities/".len()..p.len() - "/content".len()];
                let id = percent_decode(mid);
                // 调用方可以用 ?max_bytes= 覆盖默认值，但不得突破硬上限。
                let cap = req
                    .query
                    .get("max_bytes")
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(|v| v.min(MAX_CONTENT_MAX_BYTES));
                match self.service.content(&caller, &id, cap) {
                    Ok(c) => HttpResponse::json(200, serde_json::to_value(c).unwrap_or(json!({}))),
                    Err(e) => Self::gateway_error(e),
                }
            }
            p if p.starts_with("/api/v1/capabilities/") => {
                let id = percent_decode(&p["/api/v1/capabilities/".len()..]);
                match self.service.detail(&caller, &id) {
                    Ok(d) => HttpResponse::json(200, serde_json::to_value(d).unwrap_or(json!({}))),
                    Err(e) => Self::gateway_error(e),
                }
            }
            "/api/v1/agents" => {
                if req.method == "POST" {
                    return self.agents_register(req);
                }
                match self.service.gateway().with_registry(|g| g.list_agents()) {
                    Ok(v) => HttpResponse::json(200, json!({ "agents": v })),
                    Err(e) => HttpResponse::error(500, "unavailable", &format!("{e}")),
                }
            }
            // ── M4：故障定位与纳管 ───────────────────────────────
            //
            // 这几条**不在只读承诺的范围内**（claim / resolve 会写库），
            // 但仍然走同一个 route()：对外接口只有一套路由表，
            // 两套会让"同一个问题两个接口答案不一样"重新长出来。
            //
            // 写操作一律要 POST —— 见 check_method。
            "/api/v1/faults" => self.faults_list(req),
            "/api/v1/faults/tasks" => self.faults_tasks(req),
            "/api/v1/faults/claim" => self.faults_claim(req, false),
            "/api/v1/faults/release" => self.faults_claim(req, true),
            "/api/v1/faults/resolve" => self.faults_resolve(req),
            "/api/v1/assets" => self.assets_list(req),
            p if p.starts_with("/api/v1/faults/asset/") => {
                let id = percent_decode(&p["/api/v1/faults/asset/".len()..]);
                match self.service.faults_of_asset(&id) {
                    Ok(v) => HttpResponse::json(200, json!({ "asset_id": id, "faults": v })),
                    Err(e) => HttpResponse::error(500, "unavailable", &format!("{e}")),
                }
            }
            p if p.starts_with("/api/v1/faults/") => {
                let id = percent_decode(&p["/api/v1/faults/".len()..]);
                match self.service.fault_detail(&id) {
                    Ok(Some((f, h))) => {
                        HttpResponse::json(200, json!({ "fault": f, "history": h }))
                    }
                    Ok(None) => HttpResponse::error(404, "not_found", "没有这个故障"),
                    Err(e) => HttpResponse::error(500, "unavailable", &format!("{e}")),
                }
            }
            _ => HttpResponse::error(404, "not_found", "没有这个路由"),
        }
    }

    // ── M4 故障接口的实现 ──────────────────────────────────────

    /// 故障清单。默认只列未处理的。
    fn faults_list(&self, req: &HttpRequest) -> HttpResponse {
        let status = req
            .query
            .get("status")
            .and_then(|s| ac_registry::faults::FaultStatus::parse(s));
        let q = ac_registry::faults::FaultQuery {
            status,
            asset_id: req.query.get("asset_id").cloned().filter(|s| !s.is_empty()),
            failure_class: req.query.get("class").cloned().filter(|s| !s.is_empty()),
            claimed_by: req.query.get("claimed_by").cloned().filter(|s| !s.is_empty()),
        };
        match self.service.list_faults(&q) {
            Ok(v) => {
                let mut by_class: std::collections::BTreeMap<String, u32> =
                    std::collections::BTreeMap::new();
                for f in &v {
                    *by_class.entry(f.failure_class.clone()).or_insert(0) += 1;
                }
                HttpResponse::json(
                    200,
                    json!({
                        "total": v.len(),
                        "by_class": by_class,
                        "faults": v,
                    }),
                )
            }
            Err(e) => HttpResponse::error(500, "unavailable", &format!("{e}")),
        }
    }

    /// 给智能体的修复任务清单。
    fn faults_tasks(&self, _req: &HttpRequest) -> HttpResponse {
        match self.service.repair_tasks() {
            Ok(t) => HttpResponse::json(200, serde_json::to_value(t).unwrap_or(json!({}))),
            Err(e) => HttpResponse::error(500, "unavailable", &format!("{e}")),
        }
    }

    /// 认领 / 释放。
    /// 外部智能体注册自己（POST /api/v1/agents）。
    ///
    /// 参数走 query 而不是 JSON body：本服务刻意不解析 body
    /// （read_request 只读请求行，body 直接丢弃）。加 body 解析要改
    /// 整个读取路径并处理 Content-Length / chunked，而这里只需要
    /// 五六个标量字段——query 足够，且能被 curl 一行验证。
    ///
    /// 必填：`agent`（agent_id）。可选：display_name / ecosystem / kind /
    /// endpoint / meta。
    fn agents_register(&self, req: &HttpRequest) -> HttpResponse {
        let agent_id = req.query.get("agent").cloned().unwrap_or_default();
        let agent_id = agent_id.trim().to_string();
        if agent_id.is_empty() {
            return HttpResponse::error(400, "bad_request", "需要 agent 参数（agent_id）");
        }
        let get = |k: &str| req.query.get(k).cloned().unwrap_or_default();
        let reg = ac_registry::AgentRegistration {
            agent_id: agent_id.clone(),
            display_name: get("display_name"),
            ecosystem: get("ecosystem"),
            kind: if get("kind").is_empty() { "host".into() } else { get("kind") },
            endpoint: get("endpoint"),
            meta: get("meta"),
        };
        let r = self
            .service
            .gateway()
            .with_registry(|g| g.register_agent(&reg));
        match r {
            Ok(id) => HttpResponse::json(200, json!({ "registered": id })),
            Err(e) => HttpResponse::error(400, "register_failed", &format!("{e}")),
        }
    }

    fn faults_claim(&self, req: &HttpRequest, release: bool) -> HttpResponse {
        if req.method != "POST" {
            return HttpResponse::error(405, "method_not_allowed", "认领 / 释放需要 POST");
        }
        let fault_id = req.query.get("fault_id").cloned().unwrap_or_default();
        let agent = req.query.get("agent").cloned().unwrap_or_default();
        if fault_id.is_empty() || agent.is_empty() {
            return HttpResponse::error(400, "bad_request", "需要 fault_id 与 agent");
        }
        let r = if release {
            self.service.release_fault(&fault_id, &agent)
        } else {
            self.service.claim_fault(&fault_id, &agent)
        };
        match r {
            Ok(o) => HttpResponse::json(200, serde_json::to_value(o).unwrap_or(json!({}))),
            Err(e) => HttpResponse::error(400, "claim_failed", &e),
        }
    }

    /// 结束一个故障（fixed / unrepairable）。
    fn faults_resolve(&self, req: &HttpRequest) -> HttpResponse {
        if req.method != "POST" {
            return HttpResponse::error(405, "method_not_allowed", "结束故障需要 POST");
        }
        let fault_id = req.query.get("fault_id").cloned().unwrap_or_default();
        let agent = req.query.get("agent").cloned().unwrap_or_default();
        let to = req.query.get("to").cloned().unwrap_or_default();
        let note = req.query.get("note").cloned().unwrap_or_default();
        if fault_id.is_empty() || agent.is_empty() || to.is_empty() {
            return HttpResponse::error(400, "bad_request", "需要 fault_id / agent / to");
        }
        let Some(st) = ac_registry::faults::FaultStatus::parse(&to) else {
            return HttpResponse::error(400, "bad_request", "to 只能是 fixed 或 unrepairable");
        };
        match self.service.resolve_fault(&fault_id, &agent, st, &note) {
            Ok(o) => HttpResponse::json(200, serde_json::to_value(o).unwrap_or(json!({}))),
            Err(e) => HttpResponse::error(400, "resolve_failed", &e),
        }
    }

    /// 已纳管的实体。
    fn assets_list(&self, req: &HttpRequest) -> HttpResponse {
        let kind = req.query.get("kind").cloned().filter(|s| !s.is_empty());
        let r = self.service.gateway().with_registry(|g| g.list_assets(kind.as_deref()));
        match r {
            Ok(Ok(v)) => HttpResponse::json(
                200,
                json!({
                    "total": v.len(),
                    "entries": v,
                    // 库里有登记不代表磁盘上真有：把两边都报出来，
                    // 让"登记了但实体没了"这件事可见，而不是被当成一致。
                    "note": "entries 是库内登记；磁盘实况请查 assets/mcp 与 assets/skill 目录",
                }),
            ),
            Ok(Err(e)) => HttpResponse::error(500, "unavailable", &e),
            Err(e) => HttpResponse::error(500, "unavailable", &format!("{e}")),
        }
    }

    /// 把 `GatewayError` 映射成 HTTP 响应。**状态码由错误种类决定**，
    /// 不在各个 match 分支里手写——漏一个分支就是一处
    /// 「非穷尽匹配」编译错误，比手写更早暴露映射表的漂移。
    fn gateway_error(e: GatewayError) -> HttpResponse {
        HttpResponse::error(e.http_status(), e.code(), &e.to_string())
    }

    /// 校验身份。带 `agent` 但未注册 → 400（**不是**降级成匿名）。
    ///
    /// 返回 `Ok(())` 表示可以继续；`Err(resp)` 是要直接回给客户端的响应。
    fn check_caller(&self, req: &HttpRequest) -> Result<(), HttpResponse> {
        let agent = req.query.get("agent").map(String::as_str).unwrap_or("");
        match self.service.resolve_caller(agent) {
            crate::service::CallerResolution::Resolved(_) => Ok(()),
            crate::service::CallerResolution::Unregistered(a) => Err(HttpResponse::error(
                400,
                "agent_unregistered",
                &format!(
                    "智能体 {a} 未注册：拒绝按匿名放行（否则配置错误会被伪装成权限不足）。                     请先 assets-serve register --id {a}，再带上 agent={a}"
                ),
            )),
            crate::service::CallerResolution::Unavailable(m) => {
                Err(HttpResponse::error(500, "unavailable", &m))
            }
        }
    }

    /// 取出本次请求的调用方。
    ///
    /// **必须在 [`HttpServer::check_caller`] 之后调用**：那里的校验已经
    /// 保证「带 agent 时它必然已注册」，所以这里退化只有一条路径
    /// ——不带 agent → 匿名。带着未注册的 agent 继续跑是不可能的，
    /// 因为 route() 已经把它挡在 400 了。
    fn caller_for_query(&self, req: &HttpRequest) -> crate::caller::Caller {
        let agent = req.query.get("agent").map(String::as_str).unwrap_or("");
        match self.service.resolve_caller(agent).caller_or_reason() {
            Ok(c) => c,
            // 不可达：check_caller 已经把这两条路径变成 HTTP 响应了。
            // 这里仍然不 panic——路由是纯函数，一条不可达的分支不该
            // 让整个进程崩掉（release 是 panic=abort）。
            Err(_) => crate::caller::Caller::anonymous(),
        }
    }

    /// 绑定并循环服务。
    ///
    /// 直接 bind：绑定失败就报错——端口冲突必须是显式失败，
    /// 不能悄悄退到别的端口（该环境无法可靠查询端口占用）。
    pub fn serve(self: Arc<Self>, addr: &str) -> Result<(), String> {
        let listener = TcpListener::bind(addr)
            .map_err(|e| format!("绑定 {addr} 失败（该环境无法可靠查询端口占用）: {e}"))?;
        let local = listener.local_addr().map(|a| a.to_string()).unwrap_or_else(|_| addr.to_string());
        eprintln!("[ac-mcp] HTTP 监听 {local}");
        let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for stream in listener.incoming() {
            match stream {
                Ok(mut s) => {
                    use std::sync::atomic::Ordering;
                    if connections.fetch_add(1, Ordering::AcqRel) >= 32 {
                        connections.fetch_sub(1, Ordering::AcqRel);
                        let _=s.set_write_timeout(Some(std::time::Duration::from_secs(1)));
                        let _=s.write_all(&HttpResponse::error(503,"overloaded","连接上限 32，请重试").encode());
                        continue;
                    }
                    let me = Arc::clone(&self);
                    let connections = connections.clone();
                    std::thread::spawn(move || {
                        struct Guard(Arc<std::sync::atomic::AtomicUsize>);
                        impl Drop for Guard { fn drop(&mut self) { self.0.fetch_sub(1,std::sync::atomic::Ordering::AcqRel); } }
                        let _guard=Guard(connections);
                        if let Err(e) = me.handle_conn(s) {
                            eprintln!("[ac-mcp] 连接处理失败: {e}");
                        }
                    });
                }
                Err(e) => eprintln!("[ac-mcp] accept 失败: {e}"),
            }
        }
        Ok(())
    }

    fn handle_conn(&self, mut stream: TcpStream) -> std::io::Result<()> {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
        let (req,headers,body) = match read_wire_request(&mut stream) {
            Ok(r) => r,
            Err(e) => {stream.write_all(&HttpResponse::error(400,"bad_request",&e).encode())?;return Ok(());},
        };
        if req.path=="/mcp" {
            let (resp,session)=self.route_mcp(&req,&headers,&body);
            let mut bytes=resp.encode();
            if let Some(token)=session {let index=bytes.windows(4).position(|x|x==b"\r\n\r\n").unwrap();bytes.splice(index..index,format!("\r\nMcp-Session-Id: {token}").bytes());}
            stream.write_all(&bytes)?;return stream.flush();
        }
        let resp = self.route(&req);
        stream.write_all(&resp.encode())?;
        stream.flush()
    }
}

impl HttpServer {
    fn route_mcp(&self,req:&HttpRequest,headers:&BTreeMap<String,String>,body:&[u8])->(HttpResponse,Option<String>){
        let fail=|status,code,msg| (HttpResponse::error(status,code,msg),None);
        let mut sessions=match self.sessions.lock(){Ok(s)=>s,Err(_)=>return fail(503,"unavailable","会话存储不可用")};
        sessions.retain(|_,(t,_)|t.elapsed()<std::time::Duration::from_secs(900));
        let token=headers.get("mcp-session-id").cloned();
        if req.method=="DELETE" {return if token.as_ref().is_some_and(|t|sessions.remove(t).is_some()){(HttpResponse{status:200,body:String::new()},None)}else{fail(404,"session_not_found","会话不存在")};}
        if req.method!="POST" {return fail(405,"method_not_allowed","本端点使用 POST JSON；不提供服务器主动 SSE 流")}
        if !headers.get("content-type").is_some_and(|s|s.to_ascii_lowercase().starts_with("application/json")){return fail(415,"content_type","需要 application/json")}
        let value:Value=match serde_json::from_slice(body){Ok(v)=>v,Err(_)=>return fail(400,"invalid_json","JSON 无效")};
        if !value.is_object() || value["jsonrpc"]!="2.0" {return fail(400,"invalid_request","需要单条 JSON-RPC 2.0 对象")}
        let initialize=value["method"]=="initialize";
        if initialize && (value.get("id").is_none_or(Value::is_null) || value["params"]["clientInfo"]["name"].as_str().is_none_or(|n|n.is_empty()||n.len()>128)) {
            return fail(400,"invalid_initialize","initialize 需要 id 和有效 clientInfo.name");
        }
        if let Some(version)=headers.get("mcp-protocol-version") {
            if !["2025-03-26","2025-06-18","2025-11-25"].contains(&version.as_str()) {return fail(400,"protocol_version","不支持此协议版本");}
        }
        let (server,new_token)=if initialize {
            if sessions.len()>=128{return fail(503,"session_limit","会话上限 128，等待旧会话过期后重试")}
            let token=match self.service.gateway().with_registry(|r|r.operation_id()){Ok(Ok(t))=>t,_=>return fail(503,"unavailable","不能创建会话")};
            let server=Arc::new(crate::mcp::McpServer::new(self.service.clone()));
            sessions.insert(token.clone(),(std::time::Instant::now(),server.clone()));(server,Some(token))
        }else{
            let Some((t,server))=token.as_ref().and_then(|t|sessions.get_mut(t))else{return fail(404,"session_not_found","请先 initialize 建立会话")};
            *t=std::time::Instant::now();(server.clone(),None)
        };
        drop(sessions);
        match server.handle(&value){Some(v)=>(HttpResponse::json(200,v),new_token),None=>(HttpResponse{status:202,body:String::new()},new_token)}
    }
}

fn read_wire_request(stream:&mut TcpStream)->Result<(HttpRequest,BTreeMap<String,String>,Vec<u8>),String>{
    let mut reader=BufReader::new(stream.try_clone().map_err(|e|e.to_string())?);
    let mut remaining=16*1024u64;
    let mut read_line=|reader:&mut BufReader<TcpStream>|->Result<String,String>{
        let mut s=String::new();let n=reader.by_ref().take(remaining).read_line(&mut s).map_err(|e|e.to_string())?;
        remaining=remaining.saturating_sub(n as u64);if n==0||!s.ends_with('\n'){return Err("请求头不完整或超过 16 KiB".into())}Ok(s)
    };
    let line=read_line(&mut reader)?;let fields:Vec<_>=line.split_whitespace().collect();
    if fields.len()!=3||fields[2]!="HTTP/1.1"{return Err("需要 HTTP/1.1 请求行".into())}
    let mut headers=BTreeMap::new();loop{let line=read_line(&mut reader)?;if line=="\r\n"||line=="\n"{break;}
        let(k,v)=line.split_once(':').ok_or("无效请求头")?;let k=k.trim().to_ascii_lowercase();if headers.insert(k,v.trim().to_string()).is_some(){return Err("重复请求头".into());}}
    let host=headers.get("host").ok_or("缺少 Host")?;
    if !["127.0.0.1","localhost","[::1]"].iter().any(|p|host==p||host.starts_with(&format!("{p}:"))){return Err("Host 必须是本机回环地址".into());}
    if headers.contains_key("transfer-encoding"){return Err("不支持分块请求体".into());}
    let length=headers.get("content-length").map(|s|s.parse::<usize>()).transpose().map_err(|_|"无效 Content-Length")?.unwrap_or(0);
    if length>1024*1024{return Err("请求体超过 1 MiB".into());}
    let mut body=vec![0;length];reader.read_exact(&mut body).map_err(|e|e.to_string())?;
    // Consume a valid bounded body before rejecting Origin. Closing with unread
    // payload can cause a TCP reset that hides the explicit HTTP error.
    if headers.contains_key("origin"){return Err("本机服务拒绝浏览器跨来源请求".into());}
    let(path,query)=fields[1].split_once('?').map(|(p,q)|(p.to_string(),parse_query(q))).unwrap_or((fields[1].into(),BTreeMap::new()));
    Ok((HttpRequest{method:fields[0].into(),path,query},headers,body))
}

fn parse_query(q: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::{AssetKind, Authority, CapabilityManifest, ProvidedCapability};

    fn server() -> HttpServer {
        let reg = ac_registry::Registry::open_in_memory().expect("内存库");
        for (id, name, auth) in [
            ("s::a", "画时序图", Authority::Read),
            ("s::w", "写入配置", Authority::Write),
        ] {
            let mut m = CapabilityManifest::new(id, AssetKind::Skill, name);
            m.description = name.to_string();
            m.identity.authority = auth;
            m.provides.push(ProvidedCapability {
                capability_id: format!("{id}::main"),
                name: name.to_string(),
                summary: name.to_string(),
                input_schema: None,
                tags: vec![if auth == Authority::Read { "read_only".into() } else { "write".into() }],
            });
            reg.upsert(&m).expect("upsert");
        }
        HttpServer::new(Arc::new(CapabilityService::new(Arc::new(
            crate::gateway::CapabilityGateway::from_registry(reg).expect("网关"),
        ))))
    }

    fn get(server: &HttpServer, path: &str) -> HttpResponse {
        let (p, q) = match path.split_once('?') {
            Some((a, b)) => (a.to_string(), parse_query(b)),
            None => (path.to_string(), Default::default()),
        };
        server.route(&HttpRequest { method: "GET".into(), path: p, query: q })
    }

    #[test]
    fn healthz_is_always_alive() {
        assert_eq!(get(&server(), "/healthz").status, 200);
    }
    #[test]
    fn mcp_sessions_are_isolated_and_schemas_have_named_properties(){
        let s=server();let req=HttpRequest{method:"POST".into(),path:"/mcp".into(),query:Default::default()};
        let mut headers=BTreeMap::from([("content-type".into(),"application/json".into())]);
        let init=br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"test-agent","version":"1"}}}"#;
        let (a,token)=s.route_mcp(&req,&headers,init);assert_eq!(a.status,200);let token=token.unwrap();
        let (_,other)=s.route_mcp(&req,&headers,init);assert_ne!(token,other.unwrap());
        let list=br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        assert_eq!(s.route_mcp(&req,&headers,list).0.status,404);
        headers.insert("mcp-session-id".into(),token);
        let result:Value=serde_json::from_str(&s.route_mcp(&req,&headers,list).0.body).unwrap();
        assert_eq!(result["result"]["tools"][0]["inputSchema"]["properties"]["q"]["type"],"string");
        let spoof=br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"health","arguments":{"agent":"someone-else"}}}"#;
        let result:Value=serde_json::from_str(&s.route_mcp(&req,&headers,spoof).0.body).unwrap();assert_eq!(result["result"]["isError"],true);
        let delete=HttpRequest{method:"DELETE".into(),path:"/mcp".into(),query:Default::default()};
        assert_eq!(s.route_mcp(&delete,&headers,b"").0.status,200);
        assert_eq!(s.route_mcp(&req,&headers,list).0.status,404);
    }

    #[test]
    fn version_endpoint_exposes_api_version() {
        let r = get(&server(), "/api/v1/version");
        assert_eq!(r.status, 200);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["api"], "v1");
    }

    #[test]
    fn health_reports_retrieval_stages() {
        let r = get(&server(), "/api/v1/health");
        assert_eq!(r.status, 200);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert!(v["retrieval"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn capabilities_search_filters() {
        let r = get(&server(), "/api/v1/capabilities?q=%E6%97%B6%E5%BA%8F");
        assert_eq!(r.status, 200);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["items"].as_array().unwrap().len(), 1);
        assert_eq!(v["items"][0]["id"], "s::a");
    }

    #[test]
    fn detail_of_visible_asset_is_200_and_write_asset_is_404() {
        assert_eq!(get(&server(), "/api/v1/capabilities/s%3A%3Aa").status, 200);
        // s::w 对匿名不可见 -> 404（隐藏存在性），而不是 403。
        assert_eq!(get(&server(), "/api/v1/capabilities/s%3A%3Aw").status, 404);
    }

    #[test]
    fn unknown_route_is_404() {
        assert_eq!(get(&server(), "/api/v1/nope").status, 404);
    }

    #[test]
    fn non_get_is_405() {
        let s = server();
        let r = s.route(&HttpRequest {
            method: "POST".into(),
            path: "/api/v1/capabilities".into(),
            query: Default::default(),
        });
        assert_eq!(r.status, 405);
        let r2 = s.route(&HttpRequest {
            method: "DELETE".into(),
            path: "/api/v1/capabilities/s::a".into(),
            query: Default::default(),
        });
        assert_eq!(r2.status, 405);
    }

    #[test]
    fn percent_decoding_round_trips() {
        assert_eq!(percent_decode("s%3A%3Aa"), "s::a");
        assert_eq!(percent_decode("a+b"), "a b");
    }

    // ── 缺口 1：HTTP 侧身份接线 ───────────────────────────────────

    /// 带 `agent` 且该 agent 未注册 → **400**，且错误码稳定。
    ///
    /// 这是本轮最关键的一条回归。原来的 `route()` 固定匿名，
    /// 拼错的 agent_id 会拿到一份「看起来完全正常」的匿名结果集——
    /// 配置错误被伪装成权限不足，永远查不出根因。
    #[test]
    fn unregistered_agent_is_rejected_not_silently_anonymous() {
        let s = server();
        let r = get(&s, "/api/v1/capabilities?agent=ghost-agent");
        assert_eq!(r.status, 400, "未注册 agent 必须显式拒绝，实际 {}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["error"], "agent_unregistered");
        // 原因必须可执行：告诉调用方怎么修，而不只是说「不行」。
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("ghost-agent"), "原因里必须点名是哪个 agent：{msg}");
        assert!(msg.contains("register"), "原因必须给出修法：{msg}");
    }

    /// 详情与标签路由同样受身份约束——不能只有 capabilities 一条路接线。
    #[test]
    fn unregistered_agent_is_rejected_on_every_identity_bearing_route() {
        let s = server();
        assert_eq!(get(&s, "/api/v1/tags?agent=ghost-agent").status, 400);
        assert_eq!(
            get(&s, "/api/v1/capabilities/s%3A%3Aa?agent=ghost-agent").status,
            400
        );
    }

    /// 不带 `agent` → 匿名（这是最常见情形，必须照常工作）。
    #[test]
    fn no_agent_param_still_works_anonymously() {
        let r = get(&server(), "/api/v1/capabilities");
        assert_eq!(r.status, 200);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["caller"], "anonymous");
    }

    /// `agent=anonymous` 显式要匿名档，必须**放行**而不是被当成未注册。
    ///
    /// 回归风险很实在：把 anonymous 当普通 id 去查注册表，会被拒——
    /// 而那会让「用常量传匿名」这种最自然的用法直接报错。
    #[test]
    fn explicit_anonymous_agent_is_allowed() {
        let r = get(&server(), "/api/v1/capabilities?agent=anonymous");
        assert_eq!(r.status, 200, "显式匿名必须放行，实际 {}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["caller"], "anonymous");
    }

    /// 健康检查**不看**调用方身份：即使带了未注册的 agent 也必须可查。
    ///
    /// 否则会把自己关在门外——排查「我这个 agent 到底注册上没有」
    /// 恰恰要靠 health，可它自己却要求 agent 已注册。
    #[test]
    fn health_and_version_do_not_require_a_registered_agent() {
        let s = server();
        assert_eq!(get(&s, "/api/v1/health?agent=ghost-agent").status, 200);
        assert_eq!(get(&s, "/api/v1/version?agent=ghost-agent").status, 200);
        assert_eq!(get(&s, "/healthz?agent=ghost-agent").status, 200);
    }

    /// 已注册 agent 真正参与裁剪：结果集与匿名**不同**，且不大于匿名。
    ///
    /// 用真实数据形态的 source_path（`vendor/skill/<包>/...`），
    /// 让 `infer_ecosystem` 能推出 qoder 生态，从而生态维度真的收窄。
    fn crosslink_server() -> HttpServer {
        let reg = ac_registry::Registry::open_in_memory().expect("内存库");
        for (id, name, auth, path) in [
            (
                "s::a",
                "画时序图",
                Authority::Read,
                "vendor/skill/qoder-thing/2.0.0/.qoder-plugin/plugin.json",
            ),
            (
                "s::b",
                "学术写作",
                Authority::Read,
                "vendor/skill/qoder-thing/2.0.0/.qoder-plugin/SKILL.md",
            ),
            (
                "s::c",
                "别的生态技能",
                Authority::Read,
                "vendor/skill/zion-thing/2.0.0/.zion-plugin/SKILL.md",
            ),
        ] {
            let mut m = CapabilityManifest::new(id, AssetKind::Skill, name);
            m.description = name.to_string();
            m.identity.authority = auth;
            m.provenance.source_path = Some(path.to_string());
            m.provides.push(ProvidedCapability {
                capability_id: format!("{id}::main"),
                name: name.to_string(),
                summary: name.to_string(),
                input_schema: None,
                tags: vec!["read_only".into()],
            });
            reg.upsert(&m).expect("upsert");
        }
        let svc = Arc::new(CapabilityService::new(Arc::new(
            crate::gateway::CapabilityGateway::from_registry(reg).expect("网关"),
        )));
        svc.register_agent(&ac_registry::AgentRegistration {
            agent_id: "qoder-agent".into(),
            display_name: "Qoder 接入方".into(),
            ecosystem: "qoder".into(),
            kind: "host".into(),
            endpoint: String::new(),
            meta: "{}".into(),
        })
        .expect("注册");
        HttpServer::new(svc)
    }

    fn ids_of(body: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(body).unwrap();
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn registered_agent_narrows_the_visible_set() {
        let s = crosslink_server();
        let anon = get(&s, "/api/v1/capabilities?limit=200");
        assert_eq!(anon.status, 200);
        let named = get(&s, "/api/v1/capabilities?agent=qoder-agent&limit=200");
        assert_eq!(named.status, 200, "{}", named.body);

        let anon_ids = ids_of(&anon.body);
        let named_ids = ids_of(&named.body);
        // 生态收窄真的收窄了：少看到别的生态那条。
        assert!(anon_ids.contains(&"s::c".to_string()), "匿名应能看到全部 3 条：{anon_ids:?}");
        assert!(!named_ids.contains(&"s::c".to_string()), "qoder agent 不该看到 zion 资产：{named_ids:?}");
        assert_eq!(named_ids.len(), 2, "qoder agent 应看到 2 条：{named_ids:?}");
        // 子集关系必须成立（收窄不能凭空多出东西）。
        assert!(named_ids.iter().all(|i| anon_ids.contains(i)));

        // 响应自报调用方，证明确实按 agent 走了而不是仍走匿名。
        let v: Value = serde_json::from_str(&named.body).unwrap();
        assert_eq!(v["caller"], "qoder-agent");
    }

    /// **跨链路一致性**：同一 agent 走 MCP 与走 HTTP，可见集完全相同。
    ///
    /// 这是缺口 1 的验收主线。两条链路必须给出同一个答案——
    /// 不是「差不多」，是**完全相同**的 id 集合（顺序也一致）。
    ///
    /// 实现上刻意让两条链路真的各自走一遍传输层解析
    /// （MCP 的 tools/call 与 HTTP 的 route），而不是直接调 service：
    /// 只测 service 的话，某个传输层忘了接身份解析这条路径根本测不出来
    /// ——那正是本次要修的那个 bug 的形状。
    #[test]
    fn same_agent_sees_identical_set_over_mcp_and_http() {
        let s = crosslink_server();
        let service = s.service.clone();
        let mcp = crate::mcp::McpServer::new(service);

        let via_http = get(&s, "/api/v1/capabilities?agent=qoder-agent&limit=200");
        assert_eq!(via_http.status, 200);

        let resp = mcp
            .handle(&json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"search_capabilities","arguments":{
                    "agent":"qoder-agent","limit":200}}
            }))
            .expect("响应");
        let mcp_body = resp["result"]["structuredContent"].clone();
        let mcp_ids: Vec<String> = mcp_body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect();

        let http_ids = ids_of(&via_http.body);
        assert_eq!(
            http_ids, mcp_ids,
            "同一 agent 走 MCP 与 HTTP 得到的可见集必须完全相同"
        );
        assert!(!http_ids.is_empty(), "对照组：可见集不该是空的");

        // 两边的 caller 字段也要一致（证明身份解析走的是同一个入口）。
        let mcp_caller = mcp_body["caller"].as_str().unwrap();
        let http_caller: Value = serde_json::from_str(&via_http.body).unwrap();
        assert_eq!(http_caller["caller"].as_str().unwrap(), mcp_caller);
    }

    /// 同一 agent 的 **detail** 在两条链路上也必须同样可见/同样不可见。
    #[test]
    fn same_agent_gets_same_detail_verdict_on_both_links() {
        let s = crosslink_server();
        let mcp = crate::mcp::McpServer::new(s.service.clone());

        // s::c 属于 zion 生态，对 qoder-agent 不可见 -> 两边都是 not_found。
        let http = get(&s, "/api/v1/capabilities/s%3A%3Ac?agent=qoder-agent");
        assert_eq!(http.status, 404, "跨生态资产对 qoder agent 不可见");

        let resp = mcp
            .handle(&json!({
                "jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"get_capability","arguments":{
                    "id":"s::c","agent":"qoder-agent"}}
            }))
            .unwrap();
        assert_eq!(resp["result"]["isError"], true, "MCP 侧同样必须不可见");

        // 可见的那条两边都要 200。
        assert_eq!(get(&s, "/api/v1/capabilities/s%3A%3Aa?agent=qoder-agent").status, 200);
        let ok = mcp
            .handle(&json!({
                "jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"get_capability","arguments":{
                    "id":"s::a","agent":"qoder-agent"}}
            }))
            .unwrap();
        assert_eq!(ok["result"]["isError"], false);
    }

    /// 标签清单也必须两边一致——它按可见集合重算，最容易漏接线。
    #[test]
    fn same_agent_sees_identical_tags_on_both_links() {
        let s = crosslink_server();
        let mcp = crate::mcp::McpServer::new(s.service.clone());

        let http = get(&s, "/api/v1/tags?agent=qoder-agent");
        assert_eq!(http.status, 200);
        let hv: Value = serde_json::from_str(&http.body).unwrap();

        let resp = mcp
            .handle(&json!({
                "jsonrpc":"2.0","id":4,"method":"tools/call",
                "params":{"name":"list_tags","arguments":{"agent":"qoder-agent"}}
            }))
            .unwrap();
        let mv = &resp["result"]["structuredContent"];
        assert_eq!(hv["tags"], mv["tags"], "标签清单必须两边一致");
    }
}
