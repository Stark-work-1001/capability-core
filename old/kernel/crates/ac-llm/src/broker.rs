//! Task-specific models, bounded fallback and a per-endpoint circuit breaker.
use crate::slots::{load_slot,ModelSlot};
use serde::Serialize;
use std::{collections::BTreeMap,path::Path,sync::{Mutex,OnceLock},time::{Duration,Instant}};
#[derive(Default)]struct Circuit {failures:u32,until:Option<Instant>}
fn circuits()->&'static Mutex<BTreeMap<String,Circuit>> {static C:OnceLock<Mutex<BTreeMap<String,Circuit>>>=OnceLock::new();C.get_or_init(||Mutex::new(BTreeMap::new()))}
#[derive(Debug,Clone,Serialize)]pub struct ModelAnswer {pub slot:String,pub model:String,pub text:String,pub attempts:Vec<String>,pub elapsed_ms:u64}
pub fn status()->serde_json::Value {
 let c=circuits().lock().unwrap_or_else(|e|e.into_inner());
 serde_json::json!({"failure_threshold":3,"cooldown_seconds":30,"max_attempts":3,"circuits":c.values().map(|v|serde_json::json!({"failures":v.failures,"open":v.until.is_some_and(|t|t>Instant::now())})).collect::<Vec<_>>()})
}
pub fn complete(data:&Path,slot:ModelSlot,system:&str,input:&str)->Result<ModelAnswer,String>{
 if input.len()>64*1024{return Err("模型输入超过 64 KiB".into());}
 let start=Instant::now();let mut attempts=vec![];let mut seen=std::collections::BTreeSet::new();
 for s in [slot,ModelSlot::Judge,ModelSlot::Fallback] {
  let cfg=load_slot(data,s)?;
  if !cfg.is_callable(){attempts.push(format!("{}: 未配置",s.as_str()));continue;}
  let key=format!("{}|{}",cfg.endpoint,cfg.model);
  if !seen.insert(key.clone()){continue;}
  {let c=circuits().lock().map_err(|_|"模型熔断锁不可用")?;if c.get(&key).is_some_and(|v|v.until.is_some_and(|t|t>Instant::now())){attempts.push(format!("{}: 熔断冷却中",s.as_str()));continue;}}
  let secret=cfg.resolve_key().unwrap_or_default();
  let response=crate::cloud::chat(&cfg.endpoint,&secret,&cfg.model,&[("system".into(),system.into()),("user".into(),input.into())],2048,cfg.timeout_ms.clamp(1000,30_000));
  let mut c=circuits().lock().map_err(|_|"模型熔断锁不可用")?;
  let circuit=c.entry(key).or_default();
  match response {
   Ok(text)=>{circuit.failures=0;circuit.until=None;attempts.push(format!("{}: 成功",s.as_str()));return Ok(ModelAnswer{slot:s.as_str().into(),model:cfg.model,text,attempts,elapsed_ms:start.elapsed().as_millis()as u64});}
   Err(e)=>{circuit.failures+=1;if circuit.failures>=3{circuit.until=Some(Instant::now()+Duration::from_secs(30));}attempts.push(format!("{}: {}",s.as_str(),crate::cloud::redact(&e,&secret)));}
  }
 }
 Err(format!("模型工作流未完成：{}",attempts.join("；")))
}
#[cfg(test)]mod tests{use super::*;#[test]fn missing_config_fails_explicitly(){let p=Path::new("nonexistent-broker-test");let e=complete(p,ModelSlot::Repair,"test","hello").unwrap_err();assert!(e.contains("未配置"));} }
