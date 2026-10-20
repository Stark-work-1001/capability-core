//! Presence is evidence, not a substring search in process names. A known
//! executable AND product path are required. Running != connected to AssetLibrary.
use serde::{Deserialize, Serialize};
use std::{time::{Duration,Instant}, io::Read};

#[derive(Debug,Clone,Default,Deserialize)]
pub struct ProcessEvidence {
    #[serde(alias="ProcessId")] pub pid:u32,
    #[serde(alias="Name")] pub name:Option<String>,
    #[serde(alias="ExecutablePath")] pub executable:Option<String>,
    #[serde(alias="CommandLine")] pub command_line:Option<String>,
}
#[derive(Debug,Clone,Serialize,Deserialize)]
pub struct Presence {
    pub host:String, pub pid:u32, pub state:String, pub confidence:String,
    pub evidence:Vec<String>,
}
#[derive(Debug,Clone,Serialize,Deserialize)]
pub struct PresenceReport {
    pub observed_at:i64, pub processes_examined:usize, pub agents:Vec<Presence>,
    pub limitations:Vec<String>,
}

pub fn classify(p:&ProcessEvidence)->Option<Presence> {
    let name=p.name.as_deref()?.to_ascii_lowercase();
    let path=p.executable.as_deref()?.replace('\\',"/").to_ascii_lowercase();
    let parts:Vec<_>=path.split('/').collect();
    let products:&[(&str,&[&str],&[&str])]=&[
        ("codex",&["codex.exe","codex"],&["codex","@openai","openai.codex"]),
        ("claude-code",&["claude.exe","claude"],&[".claude","@anthropic-ai"]),
        ("claude-desktop",&["claude.exe","claude"],&["anthropicclaude","claude"]),
        ("cursor",&["cursor.exe","cursor"],&["cursor"]),
        ("windsurf",&["windsurf.exe","windsurf"],&["windsurf"]),
        ("qoder",&["qoder.exe","qoder"],&["qoder"]),
        ("trae",&["trae.exe","trae"],&["trae"]),
        ("kiro",&["kiro.exe","kiro"],&["kiro"]),
    ];
    for (host,names,markers) in products {
        let marker=parts.iter().any(|p|markers.iter().any(|m|p==m || (*m=="openai.codex" && p.starts_with("openai.codex_"))));
        if names.contains(&name.as_str()) && marker {
            return Some(Presence{host:(*host).into(),pid:p.pid,state:"running".into(),confidence:"process-and-path".into(),evidence:vec![format!("进程名 {name} 与产品安装路径同时匹配"),"已运行；是否接入需另有中枢会话/心跳证据".into()]});
        }
    }
    // Generic runtimes are never agents by name. Recognize only a concrete
    // package entry path, not a word appearing in a user's prompt/arguments.
    if ["node.exe","node","python.exe","python","python3"].contains(&name.as_str()) {
        let command=p.command_line.as_deref().unwrap_or("").replace('\\',"/").to_ascii_lowercase();
        let mut args=command.split('"');
        let script=if command.starts_with('"') {args.next();args.next();args.next().unwrap_or("").trim_start()} else {command.split_once(char::is_whitespace).map(|(_,s)|s.trim_start()).unwrap_or("")};
        let entry=script.trim_start_matches('"').split(['"',' ']).next().unwrap_or("");
        for (pkg,host) in [("/@anthropic-ai/claude-code/cli.js","claude-code"),("/@openai/codex/bin/codex.js","codex"),("/@google/gemini-cli/dist/index.js","gemini-cli")] {
            if entry.ends_with(pkg) {return Some(Presence{host:host.into(),pid:p.pid,state:"running".into(),confidence:"package-entry".into(),evidence:vec![format!("运行时入口匹配 {pkg}"),"未以 node/python 名称判定身份".into()]});}
        }
    }
    None
}

pub fn inspect_processes(rows:&[ProcessEvidence])->PresenceReport {
    PresenceReport{observed_at:crate::operations::now(),processes_examined:rows.len(),agents:rows.iter().filter_map(classify).collect(),limitations:vec!["进程路径是识别证据，不是签名认证；进程运行不等于已连接中枢".into(),"权限不足的进程不会猜测身份；浏览器内智能体由主动会话/心跳接入".into()]}
}

pub fn snapshot()->PresenceReport {
    match collect() {Ok(rows)=>inspect_processes(&rows),Err(e)=>PresenceReport{observed_at:crate::operations::now(),processes_examined:0,agents:vec![],limitations:vec![e]}}
}

#[cfg(windows)]
fn collect()->Result<Vec<ProcessEvidence>,String> {
    use std::os::windows::process::CommandExt;
    // No user-controlled shell text. Never return raw command lines (may contain keys).
    let mut child=std::process::Command::new("powershell.exe")
        .args(["-NoProfile","-NonInteractive","-Command","[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new(); @(Get-CimInstance Win32_Process | Select-Object ProcessId,Name,ExecutablePath,CommandLine) | ConvertTo-Json -Compress"])
        .creation_flags(0x08000000).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().map_err(|e|format!("CIM 进程探测不可用: {e}"))?;
    let out=child.stdout.take().ok_or("无法读取进程探测")?;
    let reader=std::thread::spawn(move||{let mut data=Vec::new();out.take(4*1024*1024).read_to_end(&mut data).map(|_|data)});
    let start=Instant::now();
    loop {
        if child.try_wait().map_err(|e|e.to_string())?.is_some(){break;}
        if start.elapsed()>Duration::from_secs(4){let _=child.kill();let _=child.wait();return Err("CIM 进程探测超时，未推断在线状态".into());}
        std::thread::sleep(Duration::from_millis(20));
    }
    let bytes=reader.join().map_err(|_|"进程读取失败")?.map_err(|e|e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e|format!("进程探测结果无效: {e}"))
}
#[cfg(not(windows))]
fn collect()->Result<Vec<ProcessEvidence>,String>{Err("此平台使用主动会话与心跳，CIM 探测仅支持 Windows".into())}

/// Future timestamps are not proof of eternal presence.
pub fn valid_heartbeat(timestamp:i64,now:i64)->bool {timestamp>0 && timestamp<=now+30}

#[cfg(test)] mod tests {
    use super::*;
    fn p(n:&str,path:&str,cmd:&str)->ProcessEvidence {ProcessEvidence{pid:1,name:Some(n.into()),executable:Some(path.into()),command_line:Some(cmd.into())}}
    #[test] fn rejects_generic_and_similar_names(){for item in [p("node.exe","C:/node/node.exe","node app.js --prompt codex"),p("agent.exe","C:/tools/agent.exe",""),p("codex-helper.exe","C:/codex/codex-helper.exe",""),p("cursor.exe","C:/games/cursor.exe","")]{assert!(classify(&item).is_none());}}
    #[test] fn requires_process_and_path(){assert!(classify(&p("Cursor.exe","C:/Builds/PublicUser01/Programs/Cursor/Cursor.exe","")).is_some());assert!(classify(&p("codex.exe","C:/Program Files/WindowsApps/OpenAI.Codex_1/app/codex.exe","")).is_some());}
    #[test] fn recognizes_only_runtime_entry(){assert!(classify(&p("node.exe","C:/node/node.exe","node C:/npm/node_modules/@openai/codex/bin/codex.js")).is_some());assert!(classify(&p("node.exe","C:/node/node.exe","node app.js C:/npm/node_modules/@openai/codex/bin/codex.js")).is_none());}
    #[test] fn rejects_future_heartbeat(){assert!(!valid_heartbeat(200,100));assert!(valid_heartbeat(100,100));}
}
