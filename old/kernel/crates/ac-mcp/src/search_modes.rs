use ac_registry::{Registry,AssetQuery,AssetSummary};
use serde::Serialize;
use std::{path::Path,io::{Read,Write},time::{Duration,Instant},sync::{Mutex,OnceLock}};
#[derive(Serialize)]pub struct SearchResponse {pub items:Vec<AssetSummary>,pub total:usize,pub mode:String,pub notes:Vec<String>,pub elapsed_ms:u64}
pub fn search(data:&Path,mode:&str,q:&str)->Result<SearchResponse,String>{
 if q.len()>4096{return Err("查询超过 4096 字节".into());}
 let start=Instant::now();let reg=Registry::open(&data.join("registry.db"))?;
 if mode=="keyword" {let query=AssetQuery{text:Some(q.into()),limit:80,..Default::default()};let total=reg.count_assets(&query)? as usize;return Ok(SearchResponse{items:reg.query_assets(&query)?,total,mode:mode.into(),notes:vec![],elapsed_ms:start.elapsed().as_millis()as u64});}
 let all=reg.query_assets(&AssetQuery{limit:5000,..Default::default()})?;
 let mut notes=vec![];
 let scores:Vec<(String,f32)>=match mode {
  "rust"=>{static LOCK:OnceLock<Mutex<()>>=OnceLock::new();let _guard=LOCK.get_or_init(||Mutex::new(())).lock().map_err(|_|"全文索引锁失败")?;
   let index=ac_retrieval::fulltext::FullTextIndex::attach(data.join("desktop-search-index"),&reg,ac_retrieval::fulltext::SyncPolicy::Auto);
   index.search(q,80)?.into_iter().map(|s|(s.id,s.score)).collect()
  },
  "python"=>python_search(&all,q)?,
  "cloud-vector"|"local-vector"=>{
   let slot=if mode=="cloud-vector"{ac_llm::slots::ModelSlot::Embedding}else{ac_llm::slots::ModelSlot::Localembedding};
   let cfg=ac_llm::slots::load_slot(data,slot)?;
   if !cfg.is_callable(){return Err(format!("{} 槽位未配置，未用关键词结果冒充向量结果",slot.as_str()));}
   if mode=="local-vector" && !["http://127.0.0.1:","http://localhost:","http://[::1]:"].iter().any(|p|cfg.endpoint.starts_with(p)){return Err("本地向量槽位必须使用本机回环地址".into());}
   let key=cfg.resolve_key().unwrap_or_default();
   let query=ac_llm::cloud::embed(&cfg.endpoint,&key,&cfg.model,&[q.to_string()],cfg.timeout_ms.clamp(1000,30000))?.remove(0);
   let mut rows=vec![];let mut missing=vec![];
   for a in &all {let text=format!("{}\n{}",a.name,a.description);let hash=ac_registry::parse::content_hash(format!("{}|{}|{}",cfg.endpoint,cfg.model,text).as_bytes());
    if let Some(v)=reg.cached_vector(&hash)?{rows.push((a.id.clone(),v));}else{missing.push((a.id.clone(),hash,text));}}
   // Build incrementally in bounded batches; cached work survives retries.
   for batch in missing.chunks(32) {if start.elapsed()>Duration::from_secs(75){return Err("向量缓存仍在构建；已完成批次已保存，请再次检索继续".into());}
    let text:Vec<_>=batch.iter().map(|r|r.2.clone()).collect();let vectors=ac_llm::cloud::embed(&cfg.endpoint,&key,&cfg.model,&text,cfg.timeout_ms.clamp(1000,30000))?;
    for ((id,hash,_),v) in batch.iter().zip(vectors){reg.cache_vector(hash,&v)?;rows.push((id.clone(),v));}}
   notes.push(format!("{} 个文本向量；缓存按模型、端点与内容隔离",rows.len()));
   let mut scores=vec![];for (id,v) in rows{let score=ac_retrieval::vector::cosine(&query,&v).ok_or("向量维度不一致或向量无效，请检查模型版本")?;scores.push((id,score));}scores
  },
  _=>return Err("未知检索方式".into())
 };
 let mut scores=scores;scores.sort_by(|a,b|b.1.total_cmp(&a.1).then_with(||a.0.cmp(&b.0)));scores.truncate(80);
 let mut items=Vec::new();for (id,_) in scores{if let Some(a)=all.iter().find(|a|a.id==id){items.push(a.clone());}}
 let rerank=ac_llm::slots::load_slot(data,ac_llm::slots::ModelSlot::Rerank)?;
 if mode.ends_with("vector") && rerank.is_callable() && !items.is_empty(){
  let docs=items.iter().map(|a|format!("{} {}",a.name,a.description)).collect::<Vec<_>>();
  match ac_llm::cloud::rerank(&rerank.endpoint,&rerank.resolve_key().unwrap_or_default(),&rerank.model,q,&docs,items.len(),rerank.timeout_ms.clamp(1000,30000)) {
   Ok(hits)=>{items=hits.iter().filter_map(|h|items.get(h.index).cloned()).collect();notes.push("已执行 Reranker".into());},
   Err(e)=>notes.push(format!("Reranker 降级：{e}")),
  }
 }
 Ok(SearchResponse{total:items.len(),items,mode:mode.into(),notes,elapsed_ms:start.elapsed().as_millis()as u64})
}
fn python_search(rows:&[AssetSummary],q:&str)->Result<Vec<(String,f32)>,String>{
 let local=std::env::current_exe().ok().and_then(|p|p.parent().map(|p|p.join("runtime/python/python.exe")));
 let executable=local.filter(|p|p.is_file()).unwrap_or_else(||"python".into());
 let script=include_str!("../../../tools/search_assets.py");
 let mut cmd=std::process::Command::new(executable);cmd.args(["-I","-X","utf8","-c",script]);
 #[cfg(windows)]{use std::os::windows::process::CommandExt;cmd.creation_flags(0x08000000);}
 let mut child=cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().map_err(|e|format!("Python 检索器无法启动: {e}"))?;
 let payload=serde_json::json!({"query":q,"documents":rows.iter().map(|a|serde_json::json!({"id":a.id,"text":format!("{} {}",a.name,a.description)})).collect::<Vec<_>>()}).to_string();
 let mut stdin=child.stdin.take().ok_or("Python stdin 不可用")?;
 let writer=std::thread::spawn(move||stdin.write_all(payload.as_bytes()));
 let stdout=child.stdout.take().ok_or("Python stdout 不可用")?;
 let reader=std::thread::spawn(move||{let mut out=String::new();stdout.take(1024*1024).read_to_string(&mut out).map(|_|out)});
 let start=Instant::now();loop{if child.try_wait().map_err(|e|e.to_string())?.is_some(){break;}if start.elapsed()>Duration::from_secs(10){let _=child.kill();let _=child.wait();return Err("Python 检索超时".into());}std::thread::sleep(Duration::from_millis(10));}
 writer.join().map_err(|_|"Python 输入失败")?.map_err(|e|e.to_string())?;
 let out=reader.join().map_err(|_|"Python 输出失败")?.map_err(|e|e.to_string())?;
 serde_json::from_str(&out).map_err(|e|format!("Python 检索响应无效: {e}"))
}
