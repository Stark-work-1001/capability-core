//! MCP 服务端（stdio transport）。
//!
//! # 为什么选 stdio 而不是 HTTP 作为 MCP 的主形态
//!
//! MCP 的宿主集成方式就是**把服务端当子进程拉起**，用 stdin/stdout 说 JSON-RPC。
//! 这样有三个直接好处：
//!
//! 1. **不占端口**。本项目已有硬约束（memora 占 18181/18182，本项目用 18195），
//!    而 stdio 形态下 MCP 这一路根本不需要端口，也就永远不可能撞车。
//! 2. **天然单租户**。一个 stdio 服务端只服务拉起它的那一个宿主，
//!    不存在"谁能连我的服务"这个问题，也就不需要鉴权层。
//! 3. **对齐主流软件**。Claude Desktop / 主流 MCP 客户端都默认走 stdio。
//!
//! HTTP 那一路（[`crate::http`]`）面向的是**跨进程、跨语言、跨机器**的调用方，
//! 两者不是替代关系。
//!
//! # 协议实现范围
//!
//! 实现 MCP 的只读子集：initialize / tools/list / tools/call，外加 ping。
//! **刻意不实现任何会改动用户资产的工具**——对外只读是本轮的产品要求。
//!
//! # 协议细节的三个坑
//!
//! 1. **一行一条 JSON**（JSONL），不能用别的格式，也不能有裸换行。
//! 2. **id 可为 null**（通知类请求），此时**不得**回响应。
//!    回了会让客户端的 pending 表永远等不到正确配对。
//! 3. **错误码要能区分**：协议错（-32700/-32600/-32601）与
//!    业务错（能力不存在）用不同通道表达。

use crate::gateway::{GatewayError, SearchParams, VersionInfo};
use crate::service::CapabilityService;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Arc;

/// JSON-RPC 2.0。
pub const JSONRPC_VERSION: &str = "2.0";

/// MCP 协议版本。
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// MCP 服务端。
pub struct McpServer {
    service: Arc<CapabilityService>,
    session: std::sync::Mutex<Option<ac_registry::AgentRegistration>>,
}

impl McpServer {
    pub fn new(service: Arc<CapabilityService>) -> Self {
        Self { service, session: std::sync::Mutex::new(None) }
    }

    /// 工具清单。**只读**：没有任何会改动资产的工具。
    pub fn tools(&self) -> Value {
        let tools = vec![
            tool(
                "search_capabilities",
                "在中枢里按关键词、类型、标签、权限查找可用能力（只读）。",
                vec![
                    prop("q", "string", "关键词（名称/描述/标签的包含匹配）"),
                    prop("kind", "string", "资产类型：skill|mcp|plugin|suite|ghost|organization"),
                    prop("tag", "string", "标签（任一能力带该标签即命中）"),
                    prop("min_authority", "string", "最低权限：read|observe|write|destructive"),
                    prop("limit", "integer", "最多返回条数，1..200，默认 50"),
                    prop("agent", "string", "调用方 agent_id（决定可见子集）"),
                ],
                false,
            ),
            tool(
                "get_capability",
                "取单个能力的完整详情（只读）。不可见时返回 not_found，不泄露存在性。",
                vec![prop("id", "string", "能力/资产 id"), prop("agent", "string", "调用方 agent_id")],
                true,
            ),
            tool(
                "get_capability_content",
                "取能力的源文件全文（如 SKILL.md 的完整操作说明）。只读；越权与不存在同样返回 not_found；大文件截断并标记 truncated。",
                vec![
                    prop("id", "string", "能力/资产 id"),
                    prop("agent", "string", "调用方 agent_id"),
                    prop("max_bytes", "number", "可选，返回字节上限（硬上限 8MiB，默认 256KiB）"),
                ],
                true,
            ),
            tool(
                "list_tags",
                "列出标签及其在该调用方可见范围内的计数（只读）。",
                vec![prop("agent", "string", "调用方 agent_id")],
                false,
            ),
            tool(
                "health",
                "中枢健康自检：版本、依赖（检索索引/向量层）是否就绪。",
                vec![],
                false,
            ),
            // ── M4：能力故障定位与纳管 ──────────────────────────
            tool(
                "list_faults",
                "列出中枢检测到的能力故障及其精确定位详情（只读）。默认只列未处理的。",
                vec![
                    prop("status", "string", "pending|repairing|fixed|unrepairable"),
                    prop("asset_id", "string", "只看某个资产"),
                    prop("class", "string", "只看某个故障类别，如 dead_root"),
                    prop("claimed_by", "string", "只看某个智能体认领的"),
                ],
                false,
            ),
            tool(
                "get_fault",
                "取单个故障的完整定位详情与修复历史（只读）。",
                vec![prop("id", "string", "fault_id，形如 mcp::x#serverName")],
                true,
            ),
            tool(
                "repair_tasks",
                "取给智能体接手的「修复任务清单」：每条含定位、实测证据、前置条件与建议维修类别。只读。",
                vec![],
                false,
            ),
            tool(
                "claim_fault",
                "认领一个故障，避免两个智能体重复修同一个。认领者必须已注册。",
                vec![
                    prop("id", "string", "fault_id"),
                    prop("agent", "string", "本智能体 agent_id"),
                ],
                true,
            ),
            tool(
                "resolve_fault",
                "结束一个故障：标记为已修或不可修。只有认领人（或无人认领时）能结束。",
                vec![
                    prop("id", "string", "fault_id"),
                    prop("agent", "string", "本智能体 agent_id"),
                    prop("to", "string", "fixed | unrepairable"),
                    prop("note", "string", "修法或不可修的理由（会进历史）"),
                ],
                true,
            ),
            tool(
                "list_assets",
                "列出已纳管的能力实体（assets/ 目录下的那一份，只读）。",
                vec![prop("kind", "string", "mcp | skill")],
                false,
            ),
        ];
        json!({ "tools": tools })
    }

    fn server_info(&self) -> Value {
        let v = VersionInfo::current();
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": {
                "name": "capability-core",
                "version": v.crate_version,
                "title": "Capability Core 能力中枢"
            }
        })
    }

    /// 处理一条 JSON-RPC 请求，返回 Some(响应) 或 None（通知）。
    pub fn handle(&self, req: &Value) -> Option<Value> {
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));

        // 通知（无 id 或 id 显式为 null）：执行但不回响应。
        let is_notification = matches!(id, None | Some(Value::Null));
        let out: Result<Value, RpcError> = match method {
            "initialize" => self.initialize(&params),
            "ping" => { self.touch(); Ok(json!({})) },
            "tools/list" => Ok(self.tools()),
            "tools/call" => self.call_tool(&params),
            other => Err(RpcError::method_not_found(other)),
        };

        if is_notification {
            return None;
        }
        let rid = id.unwrap_or(Value::Null);
        Some(match out {
            Ok(result) => json!({"jsonrpc": JSONRPC_VERSION, "id": rid, "result": result}),
            Err(e) => json!({"jsonrpc": JSONRPC_VERSION, "id": rid, "error": e.to_json()}),
        })
    }

    fn call_tool(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| RpcError::invalid_params("缺少 name"))?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let supplied = args.get("agent").and_then(|a| a.as_str()).unwrap_or("");
        let session = self.session.lock().map_err(|_|RpcError::invalid_request("会话不可用"))?.clone();
        let agent = if let Some(s)=session {
            if !supplied.is_empty() && supplied!=s.agent_id {return Ok(tool_error("不能用当前会话冒用另一个 agent 身份".into()));}
            s.agent_id
        } else {supplied.to_string()};
        self.touch();
        let _permit = match self.service.scheduler.acquire(&agent,std::time::Duration::from_secs(2)) { Ok(p)=>p, Err(e)=>return Ok(tool_error(e)) };
        // 身份解析**只在需要调用方身份的工具上做**。health 不看调用方，
        // 让它也走这条路的话，一个拼错的 agent 会连健康检查都查不了——
        // 而健康检查恰恰是排查「我这个 agent 到底注册上没有」要用的工具。
        let caller = if name == "health" {
            crate::caller::Caller::anonymous()
        } else {
            match self.service.resolve_caller(&agent).caller_or_reason() {
                Ok(c) => c,
                // 工具级错误（MCP 约定 isError），不是 JSON-RPC error：
                // 请求本身合法，是参数里的身份不成立。
                Err(reason) => return Ok(tool_error(reason)),
            }
        };

        match name {
            "search_capabilities" => {
                let p = SearchParams {
                    q: args.get("q").and_then(|v| v.as_str()).map(String::from),
                    kind: args.get("kind").and_then(|v| v.as_str()).map(String::from),
                    tag: args.get("tag").and_then(|v| v.as_str()).map(String::from),
                    min_authority: args.get("min_authority").and_then(|v| v.as_str()).map(String::from),
                    limit: args.get("limit").and_then(|v| v.as_u64()).map(|v| v as u32),
                    offset: args.get("offset").and_then(|v| v.as_u64()).map(|v| v as u32),
                };
                match self.service.search(&caller, &p) {
                    Ok(r) => Ok(tool_json(serde_json::to_value(r).unwrap_or(json!({})))),
                    Err(e) => Ok(tool_error(format!("{e}"))),
                }
            }
            "get_capability_content" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| RpcError::invalid_params("缺少 id"))?;
                let cap = args
                    .get("max_bytes")
                    .and_then(|v| v.as_u64())
                    .map(|v| v.min(crate::service::MAX_CONTENT_MAX_BYTES));
                match self.service.content(&caller, id, cap) {
                    Ok(c) => Ok(tool_json(serde_json::to_value(c).unwrap_or(json!({})))),
                    Err(GatewayError::NotFound(m)) => Ok(tool_error(format!("not_found: {m}"))),
                    Err(e) => Ok(tool_error(format!("{e}"))),
                }
            }
            "get_capability" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| RpcError::invalid_params("缺少 id"))?;
                match self.service.detail(&caller, id) {
                    Ok(d) => Ok(tool_json(serde_json::to_value(d).unwrap_or(json!({})))),
                    Err(GatewayError::NotFound(m)) => Ok(tool_error(format!("not_found: {m}"))),
                    Err(e) => Ok(tool_error(format!("{e}"))),
                }
            }
            "list_tags" => match self.service.tags(&caller) {
                Ok(t) => Ok(tool_json(json!({ "tags": t }))),
                Err(e) => Ok(tool_error(format!("{e}"))),
            },
            "health" => Ok(tool_json(serde_json::to_value(self.service.health()).unwrap_or(json!({})))),
            // ── M4：故障定位与纳管 ──────────────────────────────
            "list_faults" => {
                let q = ac_registry::faults::FaultQuery {
                    status: args
                        .get("status")
                        .and_then(|v| v.as_str())
                        .and_then(ac_registry::faults::FaultStatus::parse),
                    asset_id: args.get("asset_id").and_then(|v| v.as_str()).map(String::from),
                    failure_class: args.get("class").and_then(|v| v.as_str()).map(String::from),
                    claimed_by: args.get("claimed_by").and_then(|v| v.as_str()).map(String::from),
                };
                match self.service.list_faults(&q) {
                    Ok(v) => Ok(tool_json(json!({ "total": v.len(), "faults": v }))),
                    Err(e) => Ok(tool_error(format!("{e}"))),
                }
            }
            "get_fault" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| RpcError::invalid_params("缺少 id"))?;
                match self.service.fault_detail(id) {
                    Ok(Some((f, h))) => Ok(tool_json(json!({ "fault": f, "history": h }))),
                    Ok(None) => Ok(tool_error(format!("not_found: 没有故障 {id}"))),
                    Err(e) => Ok(tool_error(format!("{e}"))),
                }
            }
            "repair_tasks" => match self.service.repair_tasks() {
                Ok(t) => Ok(tool_json(serde_json::to_value(t).unwrap_or(json!({})))),
                Err(e) => Ok(tool_error(format!("{e}"))),
            },
            "claim_fault" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| RpcError::invalid_params("缺少 id"))?;
                match self.service.claim_fault(id, &agent) {
                    Ok(o) => Ok(tool_json(serde_json::to_value(o).unwrap_or(json!({})))),
                    Err(e) => Ok(tool_error(e)),
                }
            }
            "resolve_fault" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| RpcError::invalid_params("缺少 id"))?;
                let to = args
                    .get("to")
                    .and_then(|v| v.as_str())
                    .and_then(ac_registry::faults::FaultStatus::parse)
                    .ok_or_else(|| RpcError::invalid_params("to 只能是 fixed 或 unrepairable"))?;
                let note = args.get("note").and_then(|v| v.as_str()).unwrap_or("");
                match self.service.resolve_fault(id, &agent, to, note) {
                    Ok(o) => Ok(tool_json(serde_json::to_value(o).unwrap_or(json!({})))),
                    Err(e) => Ok(tool_error(e)),
                }
            }
            "list_assets" => {
                let kind = args.get("kind").and_then(|v| v.as_str());
                let r = self
                    .service
                    .gateway()
                    .with_registry(|g| g.list_assets(kind));
                match r {
                    Ok(Ok(v)) => Ok(tool_json(json!({ "total": v.len(), "entries": v }))),
                    Ok(Err(e)) => Ok(tool_error(e)),
                    Err(e) => Ok(tool_error(format!("{e}"))),
                }
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }

    fn initialize(&self,params:&Value)->Result<Value,RpcError>{
        if let Some(name)=params.pointer("/clientInfo/name").and_then(Value::as_str) {
            if name.len()>128 {return Err(RpcError::invalid_params("clientInfo.name 过长"));}
            let mut session=self.session.lock().map_err(|_|RpcError::invalid_request("会话不可用"))?;
            if session.is_none(){
                let token=self.service.gateway().with_registry(|r|r.operation_id()).map_err(RpcError::from)?.map_err(|e|RpcError::invalid_request(&e))?;
                let reg=ac_registry::AgentRegistration{agent_id:format!("session-{token}"),display_name:name.into(),ecosystem:"unassigned".into(),kind:"host".into(),endpoint:"mcp-session".into(),meta:json!({"evidence":"mcp-initialize","client_name":name,"authenticated_vendor":false}).to_string()};
                self.service.register_agent(&reg).map_err(|e|RpcError::invalid_request(&e))?;*session=Some(reg);
            }
        }
        let mut info=self.server_info();
        if let Some(v)=params.get("protocolVersion").and_then(Value::as_str){if ["2025-03-26","2025-06-18","2025-11-25"].contains(&v){info["protocolVersion"]=json!(v);}}
        Ok(info)
    }
    fn touch(&self){if let Ok(s)=self.session.lock(){if let Some(reg)=s.as_ref(){let _=self.service.register_agent(reg);}}}

    /// 从 stdin 逐行读 JSON-RPC，响应写到 stdout。返回处理的请求数。
    pub fn serve_stdio(&self) -> std::io::Result<usize> {
        let stdin = std::io::stdin();
        let mut out = std::io::stdout();
        let mut count = 0usize;
        let mut reader=BufReader::new(stdin.lock());
        loop {
            let mut line=String::new();
            let n=reader.by_ref().take(1024*1024+1).read_line(&mut line)?;
            if n==0{break;}
            if n>1024*1024{return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,"MCP message exceeds 1 MiB"));}
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            count += 1;
            let req: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    // 解析失败：id 无法得知，按 null 回协议错误。
                    let resp = json!({
                        "jsonrpc": JSONRPC_VERSION, "id": Value::Null,
                        "error": RpcError::parse_error(&e.to_string()).to_json()
                    });
                    writeln!(out, "{resp}")?;
                    out.flush()?;
                    continue;
                }
            };
            if let Some(resp) = self.handle(&req) {
                writeln!(out, "{resp}")?;
                out.flush()?;
            }
        }
        Ok(count)
    }
}

/// 一个工具的 inputSchema 属性。
fn prop(ty: &str, kind: &str, desc: &str) -> (String, Value) {
    (
        ty.to_string(),
        json!({ "type": kind, "description": desc }),
    )
}

fn tool(name: &str, description: &str, props: Vec<(String, Value)>, require_id: bool) -> Value {
    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), json!("object"));
    let mut p = serde_json::Map::new();
    for (k, v) in props {
        p.insert(k, v);
    }
    schema.insert("properties".into(), Value::Object(p));
    schema.insert("additionalProperties".into(), json!(false));
    if require_id {
        schema.insert("required".into(), json!(["id"]));
    }
    json!({
        "name": name,
        "description": description,
        "inputSchema": Value::Object(schema)
    })
}

/// 业务成功：内容用 JSON 文本承载，同时给 structuredContent 便于程序读取。
fn tool_json(payload: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&payload).unwrap_or_default() }],
        "structuredContent": payload,
        "isError": false
    })
}

/// 业务失败：MCP 约定用 isError: true 表达工具级错误，
/// 而不是 JSON-RPC error——后者留给"请求本身不合法"。
fn tool_error(msg: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": msg }],
        "isError": true
    })
}

/// JSON-RPC 错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn parse_error(msg: &str) -> Self {
        Self { code: -32700, message: format!("Parse error: {msg}") }
    }
    pub fn invalid_request(msg: &str) -> Self {
        Self { code: -32600, message: format!("Invalid Request: {msg}") }
    }
    pub fn method_not_found(m: &str) -> Self {
        Self { code: -32601, message: format!("Method not found: {m}") }
    }
    pub fn invalid_params(msg: &str) -> Self {
        Self { code: -32602, message: format!("Invalid params: {msg}") }
    }
    pub fn to_json(&self) -> Value {
        json!({ "code": self.code, "message": self.message })
    }
}

impl From<GatewayError> for RpcError {
    fn from(e: GatewayError) -> Self {
        match e {
            GatewayError::Unavailable(m) => Self { code: -32603, message: m },
            GatewayError::NotFound(m) => Self { code: -32004, message: m },
            // 调用方身份不成立 = 参数错（-32602），不是内部错误。
            // 与 HTTP 侧 400 同义：请求不合法，中枢没坏。
            GatewayError::UnregisteredCaller(m) => Self { code: -32602, message: m },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::{AssetKind, Authority, CapabilityManifest, ProvidedCapability};

    fn svc() -> Arc<CapabilityService> {
        let reg = ac_registry::Registry::open_in_memory().expect("内存库");
        let mut m = CapabilityManifest::new("s::a", AssetKind::Skill, "画时序图");
        m.description = "画时序图技能".into();
        m.identity.authority = Authority::Read;
        m.provides.push(ProvidedCapability {
            capability_id: "s::a::main".into(),
            name: "画时序图".into(),
            summary: "画时序图".into(),
            input_schema: None,
            tags: vec!["read_only".into()],
        });
        reg.upsert(&m).expect("upsert");
        Arc::new(CapabilityService::new(Arc::new(
            crate::gateway::CapabilityGateway::from_registry(reg).expect("网关"),
        )))
    }

    #[test]
    fn initialize_returns_protocol_and_server_info() {
        let s = McpServer::new(svc());
        let resp = s
            .handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
            .expect("有 id 必须有响应");
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], "capability-core");
    }

    #[test]
    fn notification_gets_no_response() {
        let s = McpServer::new(svc());
        assert!(s.handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
        assert!(s.handle(&json!({"jsonrpc":"2.0","id":null,"method":"ping"})).is_none());
    }

    #[test]
    fn tools_list_is_read_only() {
        let s = McpServer::new(svc());
        let resp = s.handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).expect("响应");
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"search_capabilities"));
        assert!(names.contains(&"get_capability"));
        assert!(names.contains(&"health"));
        for forbidden in ["write_asset", "delete_asset", "update_asset", "upsert", "install"] {
            assert!(!names.contains(&forbidden), "不得暴露写工具 {forbidden}");
        }
    }

    #[test]
    fn search_tool_returns_results() {
        let s = McpServer::new(svc());
        let resp = s
            .handle(&json!({
                "jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"search_capabilities","arguments":{"q":"时序"}}
            }))
            .expect("响应");
        assert_eq!(resp["result"]["isError"], false);
        let items = resp["result"]["structuredContent"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["name"], "画时序图");
    }

    #[test]
    fn get_capability_missing_is_tool_error_not_rpc_error() {
        let s = McpServer::new(svc());
        let resp = s
            .handle(&json!({
                "jsonrpc":"2.0","id":4,"method":"tools/call",
                "params":{"name":"get_capability","arguments":{"id":"nope"}}
            }))
            .expect("响应");
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("not_found"));
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let s = McpServer::new(svc());
        let resp = s.handle(&json!({"jsonrpc":"2.0","id":5,"method":"no/such"})).expect("响应");
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn health_tool_reports_version_and_stages() {
        let s = McpServer::new(svc());
        let resp = s
            .handle(&json!({
                "jsonrpc":"2.0","id":6,"method":"tools/call",
                "params":{"name":"health","arguments":{}}
            }))
            .expect("响应");
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["version"]["crate_name"], "ac-mcp");
        assert!(sc["retrieval"].as_array().is_some());
    }
}
