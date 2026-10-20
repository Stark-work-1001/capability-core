//! Managed, immutable revisions. SQLite is the publication boundary; files are
//! written before publication. Failed/crashed writes remain unreferenced and can
//! never replace an active asset. Vendor sources are never modified.
use crate::Registry;
use ac_core::{AssetKind, CapabilityManifest, ProvidedCapability};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io::Write, path::{Component, Path, PathBuf}};

pub fn now() -> i64 { crate::agents::now_unix() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRequest {
    pub id: String,
    pub kind: AssetKind,
    pub name: String,
    pub description: String,
    pub entry: String,
    pub files: BTreeMap<String, String>,
    #[serde(default)] pub author: String,
    #[serde(default)] pub repository: String,
    #[serde(default)] pub license: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportPlan {
    pub operation_id: String,
    pub digest: String,
    pub asset_id: String,
    pub revision: String,
    pub files: Vec<String>,
    pub bytes: usize,
    pub warnings: Vec<String>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: String, pub action: String, pub asset_id: String,
    pub status: String, pub created_at: i64, pub detail: Value,
}

/// Reject Windows aliases/ADS as well as traversal, absolute paths and control
/// characters, on every OS (packages are portable).
pub fn safe_relative(s: &str) -> Result<PathBuf, String> {
    if s.is_empty() || s.len() > 220 || s.contains(['\\', ':']) || s.chars().any(|c| c.is_control()) {
        return Err("文件路径为空、过长或含非法字符".into());
    }
    let p = Path::new(s);
    for c in p.components() {
        let Component::Normal(v) = c else { return Err("文件路径必须位于包内".into()) };
        let n = v.to_string_lossy();
        let stem = n.split('.').next().unwrap_or("").to_ascii_uppercase();
        if n.ends_with(['.', ' ']) || matches!(stem.as_str(), "CON"|"PRN"|"AUX"|"NUL")
            || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit()) {
            return Err("文件路径含 Windows 保留名称".into());
        }
    }
    if s.split('/').any(|v| v.is_empty() || v == "." || v == "..") { return Err("文件路径含空段或点段".into()); }
    Ok(p.to_path_buf())
}

fn validate(r: &ImportRequest) -> Result<Vec<String>, String> {
    if r.id.is_empty() || r.id.len() > 80 || !r.id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') {
        return Err("id 仅允许小写字母、数字和连字符，最长 80 字符".into());
    }
    if r.name.trim().is_empty() || r.description.trim().is_empty() || r.name.len() > 256 || r.description.len() > 8192 {
        return Err("名称和功能描述必填，且不能过长".into());
    }
    if r.files.is_empty() || r.files.len() > 256 || r.files.values().map(String::len).sum::<usize>() > 8*1024*1024 {
        return Err("单个能力包限 256 个文本文件、8 MiB".into());
    }
    let mut names = std::collections::BTreeSet::new();
    for name in r.files.keys() {
        safe_relative(name)?;
        if !names.insert(name.to_ascii_lowercase()) { return Err("包内文件名大小写冲突".into()); }
        let lower = name.to_ascii_lowercase();
        if lower.split('/').any(|s| matches!(s, ".git"|".env"|"llm-providers.json")) { return Err("不能收录凭据或 Git 内部文件".into()); }
    }
    safe_relative(&r.entry)?;
    let entry = r.files.get(&r.entry).ok_or("入口不在文件清单中")?;
    if r.kind == AssetKind::Mcp || r.entry.ends_with(".json") {
        let v: Value = serde_json::from_str(entry).map_err(|e| format!("入口 JSON 无效: {e}"))?;
        if r.kind == AssetKind::Mcp && !v.get("mcpServers").or_else(|| v.get("servers")).is_some_and(Value::is_object) {
            return Err("MCP 配置需包含 mcpServers 或 servers 对象".into());
        }
    }
    let mut warnings = Vec::new();
    if r.author.is_empty() { warnings.push("作者未声明".into()); }
    if r.repository.is_empty() { warnings.push("仓库来源未声明".into()); }
    if r.license.is_empty() { warnings.push("许可证待核验".into()); }
    if r.files.keys().any(|s| [".ps1", ".sh", ".py", ".js", ".exe", ".bat"].iter().any(|ext| s.ends_with(ext))) {
        warnings.push("包含可执行内容：仅收录，导入时不执行".into());
    }
    Ok(warnings)
}

/// Prevent directory symlinks/junctions from redirecting managed writes.
pub fn checked_dir(root: &Path, rel: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut p = root.clone();
    for c in rel.components() {
        let Component::Normal(n) = c else { return Err("非法纳管路径".into()) };
        p.push(n);
        if let Ok(m) = fs::symlink_metadata(&p) {
            #[cfg(windows)] {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 0x400 != 0 { return Err("纳管路径不能包含 junction / reparse point".into()); }
            }
            if m.file_type().is_symlink() || !m.is_dir() { return Err("纳管路径不是普通目录".into()); }
        } else { fs::create_dir(&p).map_err(|e| e.to_string())?; }
        if !p.canonicalize().map_err(|e| e.to_string())?.starts_with(&root) { return Err("写入路径越界".into()); }
    }
    Ok(p)
}

fn digest(r: &ImportRequest) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(r).map_err(|e| e.to_string())?)))
}

/// Redact recursively before audit/network/model use. Values with known secret
/// keys are never retained. Free-form token-bearing lines are omitted wholesale.
pub fn redact(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k,v)| {
            let key = k.to_ascii_lowercase();
            let secret = ["api_key", "apikey", "authorization", "password", "token", "secret", "cookie"];
            (k.clone(), if secret.iter().any(|s| key.contains(s)) { json!("[REDACTED]") } else { redact(v) })
        }).collect()),
        Value::Array(v) => Value::Array(v.iter().map(redact).collect()),
        Value::String(s) => json!(s.lines().map(|line| {
            let l = line.to_ascii_lowercase();
            if ["sk-", "bearer ", "api_key", "apikey", "api-key", "password=", "token=", "secret="].iter().any(|x| l.contains(x)) { "[REDACTED]" } else { line }
        }).collect::<Vec<_>>().join("\n")),
        _ => v.clone(),
    }
}

impl Registry {
    pub fn check_index_consistency(&self)->Result<Value,String> {
        let orphan:i64=self.conn.query_row("SELECT count(*) FROM capabilities c LEFT JOIN assets a ON a.id=c.asset_id WHERE a.id IS NULL",[],|r|r.get(0)).map_err(|e|e.to_string())?;
        let mut mismatch=vec![];
        for id in self.list_ids()? {if let Some(m)=self.get_manifest(&id)? {let count:i64=self.conn.query_row("SELECT count(*) FROM capabilities WHERE asset_id=?1",[&id],|r|r.get(0)).map_err(|e|e.to_string())?;if count!=m.provides.len()as i64{mismatch.push(id);}}}
        Ok(json!({"orphan_capabilities":orphan,"manifest_mismatches":mismatch,"consistent":orphan==0&&mismatch.is_empty()}))
    }
    pub(crate) fn init_operations(&self) -> Result<(), String> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS asset_operations (
          id TEXT PRIMARY KEY, action TEXT NOT NULL, asset_id TEXT NOT NULL, status TEXT NOT NULL,
          created_at INTEGER NOT NULL, detail TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS asset_drafts (id TEXT PRIMARY KEY, digest TEXT NOT NULL,
          request TEXT NOT NULL, base_hash TEXT NOT NULL, state TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS asset_revisions (asset_id TEXT NOT NULL, revision TEXT NOT NULL,
          manifest TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(asset_id, revision));
          CREATE TABLE IF NOT EXISTS workflow_runs (id TEXT PRIMARY KEY, workflow TEXT NOT NULL,
          status TEXT NOT NULL, created_at INTEGER NOT NULL, result TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS embedding_cache (cache_key TEXT PRIMARY KEY, vector TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS asset_metadata (asset_id TEXT PRIMARY KEY, updated_at INTEGER NOT NULL, detail TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS idx_ops_time ON asset_operations(created_at);").map_err(|e| e.to_string())
    }

    pub fn operation_id(&self) -> Result<String, String> {
        self.conn.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0)).map_err(|e| e.to_string())
    }
    pub fn save_asset_metadata(&self,id:&str,detail:&Value)->Result<(),String>{
        self.conn.execute("INSERT OR REPLACE INTO asset_metadata VALUES (?1,?2,?3)",params![id,now(),redact(detail).to_string()]).map_err(|e|e.to_string())?;Ok(())
    }
    pub fn asset_metadata(&self,id:&str)->Result<Value,String>{
        let raw:Option<String>=self.conn.query_row("SELECT detail FROM asset_metadata WHERE asset_id=?1",[id],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        raw.map(|s|serde_json::from_str(&s).map_err(|e|e.to_string())).unwrap_or(Ok(Value::Null))
    }
    pub fn cached_vector(&self,key:&str)->Result<Option<Vec<f32>>,String>{
        let raw:Option<String>=self.conn.query_row("SELECT vector FROM embedding_cache WHERE cache_key=?1",[key],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        raw.map(|s|serde_json::from_str(&s).map_err(|e|e.to_string())).transpose()
    }
    pub fn cache_vector(&self,key:&str,vector:&[f32])->Result<(),String>{
        self.conn.execute("INSERT OR REPLACE INTO embedding_cache VALUES (?1,?2)",params![key,serde_json::to_string(vector).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;Ok(())
    }

    pub fn audit_operation(&self, action: &str, asset: &str, status: &str, detail: Value) -> Result<String,String> {
        let id = self.operation_id()?;
        self.conn.execute("INSERT INTO asset_operations VALUES (?1,?2,?3,?4,?5,?6)",
            params![id,action,asset,status,now(),redact(&detail).to_string()]).map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub fn operation_history(&self, limit: u32) -> Result<Vec<OperationRecord>,String> {
        let mut st = self.conn.prepare("SELECT id,action,asset_id,status,created_at,detail FROM asset_operations ORDER BY created_at DESC,rowid DESC LIMIT ?1").map_err(|e| e.to_string())?;
        let rows = st.query_map([limit.clamp(1,500)], |r| Ok(OperationRecord {id:r.get(0)?, action:r.get(1)?,asset_id:r.get(2)?,status:r.get(3)?,created_at:r.get(4)?,detail:serde_json::from_str(&r.get::<_,String>(5)?).unwrap_or(Value::Null)})).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>,_>>().map_err(|e| e.to_string())
    }

    pub fn prepare_import(&self, request: &ImportRequest) -> Result<ImportPlan,String> {
        let warnings = validate(request)?;
        let id = self.operation_id()?;
        let hash = digest(request)?;
        let asset = format!("managed::{}::{}",request.kind.as_str(),request.id);
        let base = self.get_manifest(&asset)?.and_then(|m|m.provenance.content_hash).unwrap_or_default();
        self.conn.execute("INSERT INTO asset_drafts VALUES (?1,?2,?3,?4,'prepared')", params![id,hash,serde_json::to_string(request).map_err(|e|e.to_string())?,base]).map_err(|e|e.to_string())?;
        self.audit_operation("prepare_import",&asset,"prepared",json!({"operation_id":id,"digest":hash,"files":request.files.keys().collect::<Vec<_>>(),"warnings":warnings}))?;
        Ok(ImportPlan {operation_id:id,digest:hash.clone(),asset_id:asset,revision:hash,files:request.files.keys().cloned().collect(),bytes:request.files.values().map(String::len).sum(),warnings,state:"prepared".into()})
    }

    pub fn commit_import(&self, root: &Path, operation: &str, expected_digest: &str) -> Result<CapabilityManifest,String> {
        let (hash,raw,base,state):(String,String,String,String) = self.conn.query_row("SELECT digest,request,base_hash,state FROM asset_drafts WHERE id=?1",[operation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|e|e.to_string())?;
        if hash != expected_digest { return Err("预览摘要不匹配，请重新预览".into()); }
        let req: ImportRequest = serde_json::from_str(&raw).map_err(|e|e.to_string())?;
        validate(&req)?;
        let asset = format!("managed::{}::{}",req.kind.as_str(),req.id);
        if state == "committed" {
            let saved:String=self.conn.query_row("SELECT manifest FROM asset_revisions WHERE asset_id=?1 AND revision=?2",params![asset,hash],|r|r.get(0)).map_err(|e|e.to_string())?;
            return serde_json::from_str(&saved).map_err(|e|e.to_string());
        }
        let revision_dir = PathBuf::from("capabilities").join(req.kind.as_str()).join(&req.id).join(&hash);
        let dir = checked_dir(root, &revision_dir)?;
        // Immutable bytes: retries may reuse identical completed files. Changed
        // content or link substitution is rejected, never overwritten.
        for (name,text) in &req.files {
            let rel = safe_relative(name)?;
            let parent = rel.parent().filter(|p|!p.as_os_str().is_empty());
            let parent = if let Some(p)=parent {checked_dir(&dir,p)?} else {dir.clone()};
            let target = parent.join(rel.file_name().ok_or("入口无文件名")?);
            match fs::OpenOptions::new().write(true).create_new(true).open(&target) {
                Ok(mut f) => { f.write_all(text.as_bytes()).and_then(|_|f.sync_all()).map_err(|e|e.to_string())?; }
                Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists => {
                    let meta=fs::symlink_metadata(&target).map_err(|e|e.to_string())?;
                    if meta.file_type().is_symlink() || fs::read(&target).map_err(|e|e.to_string())? != text.as_bytes() {return Err("不可变版本内容已被修改".into());}
                }
                Err(e)=>return Err(e.to_string()),
            }
        }
        let mut m=if req.kind==AssetKind::Mcp {
            crate::parse::parse_mcp_json(&dir.join(&req.entry),&req.files[&req.entry],crate::parse::IdScope::default())
        }else{CapabilityManifest::new(&asset,req.kind,&req.name)};
        m.id=asset.clone();m.name=req.name.clone();
        for (i,c) in m.provides.iter_mut().enumerate(){c.capability_id=format!("{asset}::{i}");}
        m.description=req.description.clone(); m.version=hash[..12].into();
        let portable=self.conn.path().and_then(|p|Path::new(p).parent()).and_then(|p|p.canonicalize().ok())==root.canonicalize().ok();
        m.provenance.source_path=Some(if portable {revision_dir.join(&req.entry).to_string_lossy().replace('\\',"/")}else{dir.join(&req.entry).to_string_lossy().into()});
        m.provenance.content_hash=Some(hash.clone());
        m.provenance.source_format=Some(if req.kind==AssetKind::Skill {"skill_md"} else if req.kind==AssetKind::Mcp {"mcp_json"} else {"managed_manifest"}.into());
        m.provenance.author=Some(req.author.clone()); m.provenance.author_source=Some("manifest".into());
        if m.provides.is_empty(){m.provides.push(ProvidedCapability {capability_id:format!("{asset}::main"),name:req.name.clone(),summary:req.description.clone(),input_schema:None,tags:vec!["managed".into()]});}
        self.conn.execute_batch("BEGIN IMMEDIATE").map_err(|e|e.to_string())?;
        let result=(|| {
            let current=self.get_manifest(&asset)?;
            let current_hash=current.as_ref().and_then(|m|m.provenance.content_hash.clone()).unwrap_or_default();
            if current_hash != base {return Err("资产已被另一操作修改，请重新预览".into());}
            if let Some(old)=current {self.save_revision(&old)?;}
            self.upsert(&m)?; self.save_revision(&m)?;
            self.save_asset_metadata(&asset,&json!({"author":req.author,"repository":req.repository,"license":req.license,"verified_online":false,"metadata_source":"import declaration"}))?;
            self.conn.execute("UPDATE asset_drafts SET state='committed' WHERE id=?1",[operation]).map_err(|e|e.to_string())?;
            self.audit_operation("commit_import",&asset,"completed",json!({"revision":hash,"repository":req.repository,"license":req.license,"author":req.author,"operation_id":operation}))?;
            Ok(m)
        })();
        match result {Ok(v)=>{self.conn.execute_batch("COMMIT").map_err(|e|e.to_string())?;Ok(v)},Err(e)=>{let _=self.conn.execute_batch("ROLLBACK");Err(e)}}
    }

    fn save_revision(&self,m:&CapabilityManifest)->Result<(),String> {
        self.conn.execute("INSERT OR IGNORE INTO asset_revisions VALUES (?1,?2,?3,?4)",params![m.id,m.provenance.content_hash.clone().unwrap_or_default(),serde_json::to_string(m).map_err(|e|e.to_string())?,now()]).map_err(|e|e.to_string())?;Ok(())
    }

    pub fn revisions(&self,id:&str)->Result<Vec<Value>,String> {
        let mut st=self.conn.prepare("SELECT revision,created_at FROM asset_revisions WHERE asset_id=?1 ORDER BY created_at DESC,rowid DESC").map_err(|e|e.to_string())?;
        let rows=st.query_map([id],|r|Ok(json!({"revision":r.get::<_,String>(0)?,"created_at":r.get::<_,i64>(1)?}))).map_err(|e|e.to_string())?;
        rows.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())
    }

    pub fn rollback_revision(&self,id:&str,revision:&str)->Result<CapabilityManifest,String> {
        if !id.starts_with("managed::") {return Err("仅支持回滚中枢纳管资产".into());}
        let raw:String=self.conn.query_row("SELECT manifest FROM asset_revisions WHERE asset_id=?1 AND revision=?2",params![id,revision],|r|r.get(0)).optional().map_err(|e|e.to_string())?.ok_or("版本不存在")?;
        let m:CapabilityManifest=serde_json::from_str(&raw).map_err(|e|e.to_string())?;
        let source=m.provenance.source_path.as_ref().ok_or("版本缺少入口")?;
        let path=if Path::new(source).is_absolute(){PathBuf::from(source)}else{
            let parent=self.conn.path().and_then(|p|Path::new(p).parent()).ok_or("版本缺少数据库路径")?;
            parent.join(safe_relative(source)?)
        };
        if !path.is_file(){return Err("历史版本文件缺失，不能回滚".into());}
        let request:String=self.conn.query_row("SELECT request FROM asset_drafts WHERE digest=?1 AND state='committed' LIMIT 1",[revision],|r|r.get(0)).map_err(|e|e.to_string())?;
        let request:ImportRequest=serde_json::from_str(&request).map_err(|e|e.to_string())?;
        let mut dir=path;
        for _ in Path::new(&request.entry).components(){dir.pop();}
        for (name,content) in &request.files {
            let rel=safe_relative(name)?;
            if let Some(parent)=rel.parent().filter(|p|!p.as_os_str().is_empty()){checked_dir(&dir,parent)?;}
            let file=dir.join(rel);let meta=fs::symlink_metadata(&file).map_err(|e|e.to_string())?;
            if meta.file_type().is_symlink() || fs::read(&file).map_err(|e|e.to_string())?!=content.as_bytes(){return Err("历史版本完整性校验失败，拒绝回滚".into());}
        }
        self.conn.execute_batch("BEGIN IMMEDIATE").map_err(|e|e.to_string())?;
        let result=(||{self.upsert(&m)?;self.audit_operation("rollback",id,"completed",json!({"revision":revision}))?;Ok(m)})();
        match result {Ok(v)=>{self.conn.execute_batch("COMMIT").map_err(|e|e.to_string())?;Ok(v)},Err(e)=>{let _=self.conn.execute_batch("ROLLBACK");Err(e)}}
    }

    pub fn record_workflow(&self,workflow:&str,status:&str,result:&Value)->Result<String,String> {
        let id=self.operation_id()?;
        self.conn.execute("INSERT INTO workflow_runs VALUES (?1,?2,?3,?4,?5)",params![id,workflow,status,now(),redact(result).to_string()]).map_err(|e|e.to_string())?;
        Ok(id)
    }
    pub fn workflow_history(&self)->Result<Vec<Value>,String> {
        let mut st=self.conn.prepare("SELECT id,workflow,status,created_at,result FROM workflow_runs ORDER BY created_at DESC,rowid DESC LIMIT 100").map_err(|e|e.to_string())?;
        let rows=st.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"workflow":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"created_at":r.get::<_,i64>(3)?,"result":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null)}))).map_err(|e|e.to_string())?;
        rows.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())
    }
}

#[cfg(test)] mod tests {
    use super::*;
    fn request()->ImportRequest { ImportRequest{id:"sample".into(),kind:AssetKind::Skill,name:"Sample".into(),description:"A sample skill".into(),entry:"SKILL.md".into(),files:BTreeMap::from([("SKILL.md".into(),"---\nname: sample\n---\nUse this skill".into())]),author:String::new(),repository:String::new(),license:String::new()} }
    #[test] fn paths_block_windows_aliases_and_traversal(){ for p in ["../a","/a","C:/a","a\\b","a:b","CON.txt","a/../b","a/./b","a.","a//b"]{assert!(safe_relative(p).is_err(),"{p}");} }
    #[test] fn publish_retry_conflict_and_rollback(){
        let s=crate::testutil::Scratch::new("managed-versions");let db=Registry::open_in_memory().unwrap();let mut r=request();
        let p=db.prepare_import(&r).unwrap();assert!(db.commit_import(s.path(),&p.operation_id,"wrong").is_err());
        let m=db.commit_import(s.path(),&p.operation_id,&p.digest).unwrap();assert_eq!(db.total().unwrap(),1);
        db.commit_import(s.path(),&p.operation_id,&p.digest).unwrap();
        r.files.insert("SKILL.md".into(),"second".into());let p2=db.prepare_import(&r).unwrap();
        r.files.insert("SKILL.md".into(),"third".into());let p3=db.prepare_import(&r).unwrap();
        db.commit_import(s.path(),&p2.operation_id,&p2.digest).unwrap();assert!(db.commit_import(s.path(),&p3.operation_id,&p3.digest).is_err());
        let old=db.rollback_revision(&m.id,&p.digest).unwrap();assert_eq!(fs::read_to_string(old.provenance.source_path.unwrap()).unwrap(),request().files["SKILL.md"]);
        assert_eq!(db.revisions(&m.id).unwrap().len(),2);
    }
    #[test] fn audit_redacts_nested_credentials(){let v=redact(&json!({"api_key":"secret","nested":[{"Authorization":"Bearer abc"}],"message":"error sk-abcd"}));assert!(!v.to_string().contains("abcd"));assert!(!v.to_string().contains("Bearer"));}
    #[test] fn corrupted_historical_bytes_cannot_be_published_by_rollback(){
        let s=crate::testutil::Scratch::new("rollback-corruption");let db=Registry::open_in_memory().unwrap();let r=request();
        let p=db.prepare_import(&r).unwrap();assert_eq!(p.digest.len(),64);
        let m=db.commit_import(s.path(),&p.operation_id,&p.digest).unwrap();
        fs::write(m.provenance.source_path.unwrap(),"tampered").unwrap();
        assert!(db.rollback_revision(&m.id,&p.digest).unwrap_err().contains("完整性"));
    }
    #[test] fn managed_mcp_retains_each_server_and_runtime_refs(){
        let s=crate::testutil::Scratch::new("managed-mcp");let db=Registry::open_in_memory().unwrap();let mut r=request();
        r.kind=AssetKind::Mcp;r.entry="mcp.json".into();r.files=BTreeMap::from([("mcp.json".into(),r#"{"servers":{"one":{"command":"python","args":["missing.py"]},"two":{"url":"https://example.org/mcp"}}}"#.into())]);
        let p=db.prepare_import(&r).unwrap();let m=db.commit_import(s.path(),&p.operation_id,&p.digest).unwrap();
        assert_eq!(m.provides.len(),2);assert!(m.provenance.runtime_refs.contains(&"missing.py".into()));
    }
    #[test]fn disk_registry_publishes_portable_paths_and_can_rollback(){
        let s=crate::testutil::Scratch::new("portable-disk");let db=Registry::open(&s.path().join("registry.db")).unwrap();let mut r=request();
        let p=db.prepare_import(&r).unwrap();let m=db.commit_import(s.path(),&p.operation_id,&p.digest).unwrap();
        let path=m.provenance.source_path.unwrap();assert!(!Path::new(&path).is_absolute());assert!(!path.contains('\\'));assert!(s.path().join(path).is_file());
        r.files.insert("SKILL.md".into(),"next version".into());let p2=db.prepare_import(&r).unwrap();db.commit_import(s.path(),&p2.operation_id,&p2.digest).unwrap();
        db.rollback_revision(&m.id,&p.digest).unwrap();
    }
}
