//! Additional import adapters. Importing a declaration does not claim support
//! for executing every operation in the external protocol.
use ac_core::{AssetKind,CapabilityManifest,ProvidedCapability};
use serde_json::{Value,json};
use std::path::Path;
pub fn parse(path:&Path,text:&str,scope:crate::parse::IdScope<'_>)->Result<CapabilityManifest,String>{
 let v:Value=serde_json::from_str(text).map_err(|e|e.to_string())?;
 let key=scope.slug(path);
 if v.get("schema_version").is_some(){return CapabilityManifest::parse(text).map_err(|e|e.to_string());}
 let openapi=v.get("openapi").and_then(Value::as_str).is_some_and(|s|s.starts_with("3."));
 let card=v.get("skills").is_some_and(Value::is_array) && v.get("name").is_some();
 if !openapi && !card{return Err("未识别为 OpenAPI 3 或 A2A Agent Card".into());}
 let name=if openapi{v.pointer("/info/title")}else{v.get("name")}.and_then(Value::as_str).unwrap_or(&key);
 let mut m=CapabilityManifest::new(format!("{}::{key}",if openapi{"openapi"}else{"a2a"}),if openapi{AssetKind::Mcp}else{AssetKind::Ghost},name);
 m.description=if openapi{v.pointer("/info/description")}else{v.get("description")}.and_then(Value::as_str).unwrap_or("").into();
 m.provenance.source_format=Some(if openapi{"openapi_json"}else{"a2a_agent_card"}.into());
 if openapi {if let Some(paths)=v.get("paths").and_then(Value::as_object){for (path,methods) in paths {if let Some(methods)=methods.as_object(){for (method,op) in methods {if !["get","post","put","delete","patch","head","options"].contains(&method.as_str()){continue;}
  let name=op.get("operationId").and_then(Value::as_str).map(str::to_string).unwrap_or_else(||format!("{method} {path}"));
  m.provides.push(ProvidedCapability{capability_id:format!("{}::{}",m.id,crate::parse::content_hash(format!("{method}:{path}").as_bytes())),name,summary:op.get("summary").or_else(||op.get("description")).and_then(Value::as_str).unwrap_or("").into(),input_schema:op.get("requestBody").cloned(),tags:vec!["openapi-declaration".into()]});
 }}}}}
 else{for (i,skill) in v["skills"].as_array().unwrap().iter().enumerate(){m.provides.push(ProvidedCapability{capability_id:format!("{}::{i}",m.id),name:skill.get("name").and_then(Value::as_str).unwrap_or("skill").into(),summary:skill.get("description").and_then(Value::as_str).unwrap_or("").into(),input_schema:None,tags:vec!["a2a-declaration".into()]});}}
 m.provenance.source_path=Some(path.to_string_lossy().into());m.provenance.content_hash=Some(crate::parse::content_hash(text.as_bytes()));Ok(m)
}
pub fn catalog()->Value {json!([
 {"name":"MCP stdio","status":"implemented","scope":"initialize / ping / tools/list / tools/call；会话注册与用量","source":"https://modelcontextprotocol.io/specification/2025-11-25"},
 {"name":"MCP Streamable HTTP","status":"partial","scope":"/mcp：JSON 响应、会话、POST/DELETE；无服务器主动 SSE 流与 OAuth","source":"https://modelcontextprotocol.io/specification/2025-11-25/basic/transports"},
 {"name":"Agent Skills","status":"implemented","scope":"SKILL.md 元数据、正文与资源纳管","source":"https://agentskills.io/specification"},
 {"name":"Claude / Codex Plugins","status":"partial","scope":"plugin.json、SKILL.md、MCP 声明；hooks 不自动执行","source":"https://developers.openai.com/plugins/build/plugins"},
 {"name":"VS Code / Cursor MCP 配置","status":"implemented","scope":"mcpServers 与 servers 两种配置形态","source":"https://code.visualstudio.com/docs/agent-customization/mcp-servers"},
 {"name":"OpenAPI 3","status":"partial","scope":"JSON 规范的操作与输入声明可检索；不自动执行远程业务操作","source":"https://spec.openapis.org/oas/latest.html"},
 {"name":"A2A","status":"partial","scope":"Agent Card 技能导入；任务发送、流式执行尚未实现","source":"https://a2a-protocol.org/latest/specification/"},
 {"name":"ACP · Agent Client Protocol","status":"researched","scope":"已核对编辑器与智能体会话协议；暂无执行适配器","source":"https://agentclientprotocol.com/protocol/overview"}
])}
#[cfg(test)]mod tests{use super::*;#[test]fn openapi_imports_operations_only(){let m=parse(Path::new("openapi.json"),r#"{"openapi":"3.1.0","info":{"title":"Test"},"paths":{"/x":{"parameters":[],"get":{"summary":"Read x"}}}}"#,crate::parse::IdScope::new("x")).unwrap();assert_eq!(m.provides.len(),1);assert_eq!(m.provides[0].summary,"Read x");}#[test]fn a2a_imports_declared_skills(){let m=parse(Path::new("agent-card.json"),r#"{"name":"Agent","skills":[{"id":"a","name":"A","description":"Task A"}]}"#,crate::parse::IdScope::new("a")).unwrap();assert_eq!(m.kind,AssetKind::Ghost);}}
