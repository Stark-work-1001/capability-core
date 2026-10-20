//! Offline runtime deployment. The package owns the source bytes; an immutable
//! local cache avoids Fixed Runtime child-process launch failures on some drives.
use std::{collections::BTreeMap, path::{Path,PathBuf},io::Write};
use serde::Deserialize;
#[derive(Deserialize)]struct RuntimeManifest {version:String,files:BTreeMap<String,String>}

pub fn bundled(root:&Path)->Result<PathBuf,String>{
    let mut choices=vec![];
    if let Some(explicit)=std::env::var_os("CORE_RUNTIME_CACHE_DIR"){choices.push(PathBuf::from(explicit));}
    else {
        if let Some(local)=std::env::var_os("LOCALAPPDATA"){choices.push(PathBuf::from(local).join("CapabilityCore/runtime-cache"));}
        if let Some(shared)=std::env::var_os("PROGRAMDATA"){choices.push(PathBuf::from(shared).join("CapabilityCore/runtime-cache"));}
    }
    let mut errors=vec![];
    for cache in choices {match deploy(root,&cache){Ok(path)=>return Ok(path),Err(e)=>errors.push(format!("{}: {e}",cache.display()))}}
    // The Windows deployment helper can apply normal OS file-copy semantics in
    // environments where a portable application's direct writes are restricted.
    helper(root).map_err(|e|format!("离线运行时缓存部署失败: {}；{e}",errors.join("；")))
}
fn helper(root:&Path)->Result<PathBuf,String>{
    use std::{io::Read,process::{Command,Stdio},time::{Instant,Duration}};
    let manifest=std::fs::read(root.join("runtime/webview2-manifest.json")).map_err(|e|e.to_string())?;
    let hash=ac_registry::parse::sha256_digest(&manifest);
    let system=std::env::var_os("SystemRoot").map(PathBuf::from).ok_or("SystemRoot missing")?;
    let mut command=Command::new(system.join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    command.args(["-NoProfile","-NonInteractive","-Command",include_str!("runtime-bootstrap.ps1")])
        .env("CORE_BOOTSTRAP_ROOT",root).env("CORE_BOOTSTRAP_MANIFEST",&hash).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]{use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);}
    let mut child=command.spawn().map_err(|e|e.to_string())?;
    let stdout=child.stdout.take().ok_or("runtime helper stdout missing")?;
    let stderr=child.stderr.take().ok_or("runtime helper stderr missing")?;
    let output=std::thread::spawn(move||{let mut b=vec![];let _=stdout.take(65536).read_to_end(&mut b);b});
    let errors=std::thread::spawn(move||{let mut b=vec![];let _=stderr.take(65536).read_to_end(&mut b);b});
    let start=Instant::now();let status=loop{if let Some(s)=child.try_wait().map_err(|e|e.to_string())?{break s;}if start.elapsed()>Duration::from_secs(90){let _=child.kill();let _=child.wait();return Err("运行时部署助手超时".into());}std::thread::sleep(Duration::from_millis(50));};
    let out=output.join().map_err(|_|"runtime output thread failed")?;let err=errors.join().map_err(|_|"runtime error thread failed")?;
    if !status.success(){return Err(String::from_utf8_lossy(&err).chars().take(1500).collect());}
    let path=PathBuf::from(String::from_utf8(out).map_err(|e|e.to_string())?.trim());
    if !path.is_absolute() || path.file_name().is_none_or(|p|p!=hash.as_str()){return Err("运行时部署助手返回了无效目录".into());}
    Ok(path)
}
fn deploy(root:&Path,cache_base:&Path)->Result<PathBuf,String>{
    let source=root.join("runtime/webview2");
    let raw=std::fs::read(root.join("runtime/webview2-manifest.json")).map_err(|e|format!("运行时清单缺失: {e}"))?;
    if raw.len()>1024*1024{return Err("运行时清单过大".into());}
    let manifest:RuntimeManifest=serde_json::from_slice(&raw).map_err(|e|e.to_string())?;
    if manifest.files.is_empty() || manifest.files.len()>2048 || !manifest.files.contains_key("msedgewebview2.exe"){return Err("运行时清单不完整".into());}
    let cache_key=ac_registry::parse::sha256_digest(&raw);
    let cache=cache_base.join(&cache_key);
    let ready=cache.join(".complete");
    // Verify critical images on warm starts; a marker alone is insufficient.
    if std::fs::read_to_string(&ready).ok().as_deref()==Some(&cache_key) {
        let mut intact=true;
        for name in ["msedgewebview2.exe","msedge.dll"]{
            if let Some(expected)=manifest.files.get(name){
                intact &= std::fs::read(cache.join(name)).is_ok_and(|b|ac_registry::parse::sha256_digest(&b)==*expected);
            }else{intact=false;}
        }
        if intact{return Ok(cache);}
        return Err("本机运行时缓存完整性校验失败；请移走该缓存后重试".into());
    }
    let source_root=source.canonicalize().map_err(|e|e.to_string())?;
    for (name,expected) in manifest.files {
        let rel=ac_registry::operations::safe_relative(&name)?;
        if expected.len()!=64 || !expected.bytes().all(|b|b.is_ascii_hexdigit()){return Err("运行时文件摘要无效".into());}
        let input=source.join(&rel);
        if !input.canonicalize().map_err(|e|e.to_string())?.starts_with(&source_root){return Err("运行时来源越界".into());}
        let bytes=std::fs::read(&input).map_err(|e|e.to_string())?;
        if ac_registry::parse::sha256_digest(&bytes)!=expected{return Err(format!("随包运行时内容被修改: {name}"));}
        let parent=rel.parent().unwrap_or(Path::new(""));
        let target=ac_registry::operations::checked_dir(&cache,parent)?.join(rel.file_name().ok_or("文件名缺失")?);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&target){
            Ok(mut file)=>file.write_all(&bytes).and_then(|_|file.sync_all()).map_err(|e|e.to_string())?,
            Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>{
                let deadline=std::time::Instant::now()+std::time::Duration::from_secs(10);
                loop {
                    if std::fs::read(&target).is_ok_and(|b|ac_registry::parse::sha256_digest(&b)==expected){break;}
                    if std::time::Instant::now()>=deadline{return Err(format!("缓存写入冲突或文件损坏: {name}"));}
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            },
            Err(e)=>return Err(e.to_string())
        }
    }
    std::fs::write(ready,&cache_key).map_err(|e|e.to_string())?;
    eprintln!("[assets] 已从随包文件部署离线 WebView2 {}: {}",manifest.version,cache.display());
    Ok(cache)
}
