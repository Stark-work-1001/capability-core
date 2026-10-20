//! assets-serve —— 对外服务入口（stdio MCP + HTTP）。
//!
//! # 两种启动形态
//!
//! - assets-serve mcp  [--db <path>]   走 MCP stdio（宿主拉起它，不占端口）
//! - assets-serve http [--db <path>] [--port N]
//!   起 HTTP 常驻服务（默认 18195；18181/18182 属 memora，严禁使用）
//!
//! 两者共用一个 CapabilityService，因此裁剪、路由、审计语义完全一致。

use ac_mcp::{CapabilityGateway, CapabilityService, HttpServer, McpServer};
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(|s| s.as_str()).unwrap_or("help");
    let db = arg_value(&args, "--db");
    let port = arg_value(&args, "--port")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(ac_mcp::http::DEFAULT_PORT);

    if mode == "help" || mode == "--help" || mode == "-h" {
        print_help();
        return;
    }

    // 统一成 PathBuf：--db 给的是字符串，默认值也要同类型，
    // 否则 unwrap_or_else 的两个分支类型不一致（实测踩过）。
    let db_path: std::path::PathBuf = db
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("[assets-serve] 未指定 --db，使用 ./data/registry.db");
            std::path::PathBuf::from("data/registry.db")
        });
    if ["workflow","search","prepare","commit","rollback","history","ingest","catalog"].contains(&mode) {
        let result=(|| -> Result<serde_json::Value,String> {
            if arg_value(&args,"--db").is_none(){return Err("管理命令必须显式指定 --db".into());}
            let data=db_path.parent().ok_or("数据库路径没有父目录")?;
            if db_path.file_name().is_none_or(|s|s!="registry.db"){return Err("管理命令的数据库文件名必须为 registry.db".into());}
            let reg=ac_registry::Registry::open(&db_path)?;
            let required=|name:&str|arg_value(&args,name).ok_or_else(||format!("缺少 {name}"));
            let read_json=|name:&str|->Result<String,String>{let p=required(name)?;let size=std::fs::metadata(&p).map_err(|e|e.to_string())?.len();if size>8*1024*1024{return Err("输入文件超过 8 MiB".into());}std::fs::read_to_string(p).map_err(|e|e.to_string())};
            match mode {
                "catalog"=>Ok(serde_json::json!({"workflows":ac_mcp::workflows::catalog(),"protocols":ac_registry::formats::catalog()})),
                "workflow"=>{let name=required("--name")?;let input=if arg_value(&args,"--input").is_some(){serde_json::from_str(&read_json("--input")?).map_err(|e|e.to_string())?}else{Default::default()};serde_json::to_value(ac_mcp::workflows::execute(data,&name,input)?).map_err(|e|e.to_string())},
                "search"=>serde_json::to_value(ac_mcp::search_modes::search(data,&required("--mode")?,&required("--query")?)?).map_err(|e|e.to_string()),
                "prepare"=>{let request=serde_json::from_str(&read_json("--input")?).map_err(|e|e.to_string())?;serde_json::to_value(reg.prepare_import(&request)?).map_err(|e|e.to_string())},
                "commit"=>serde_json::to_value(reg.commit_import(data,&required("--operation")?,&required("--digest")?)?).map_err(|e|e.to_string()),
                "rollback"=>serde_json::to_value(reg.rollback_revision(&required("--asset")?,&required("--revision")?)?).map_err(|e|e.to_string()),
                "history"=>serde_json::to_value(reg.operation_history(100)?).map_err(|e|e.to_string()),
                "ingest"=>{let report=ac_registry::ingest(&reg,std::path::Path::new(&required("--root")?))?;Ok(serde_json::json!({"parsed":report.parsed,"inserted":report.inserted,"updated":report.updated,"removed":report.removed,"skipped":report.skipped.len(),"total":reg.total()?}))},
                _=>Err("未知管理命令".into())
            }
        })();
        match result {Ok(value)=>println!("{}",value),Err(e)=>{eprintln!("{e}");std::process::exit(1);}}
        return;
    }

    // 向量层随主服务一起装配。地址与超时走环境变量
    // （AC_VECTOR_URL / AC_VECTOR_TIMEOUT_MS），默认 127.0.0.1:18191 与 2500ms。
    //
    // **装配失败不影响服务启动**：向量是可选增强，探不通就是
    // vector: failed(原因)，检索降级到结构化基线加 BM25。
    // 启动日志里如实打一行，免得「为什么没向量召回」要靠猜。
    let gateway = match CapabilityGateway::open(&db_path) {
        Ok(g) => {
            let g = g.with_vector_from_env();
            match g.vector() {
                Some(v) => {
                    let st = v.state();
                    let endpoint = v.client().config().endpoint.clone();
                    if st.is_usable() {
                        eprintln!("[assets-serve] 向量层已装配：{endpoint}（{st}）");
                    } else {
                        eprintln!(
                            "[assets-serve] 向量层不可用：{endpoint}（{st}）——检索降级为结构化基线加 BM25"
                        );
                    }
                }
                None => {}
            }
            Arc::new(g)
        }
        Err(e) => {
            eprintln!("[assets-serve] 打开数据库失败: {e}");
            std::process::exit(2);
        }
    };
    let service = Arc::new(CapabilityService::new(gateway));

    match mode {
        "mcp" => {
            let server = McpServer::new(service);
            match server.serve_stdio() {
                Ok(n) => eprintln!("[assets-serve] MCP stdio 已处理 {n} 条请求"),
                Err(e) => {
                    eprintln!("[assets-serve] MCP stdio 失败: {e}");
                    std::process::exit(1);
                }
            }
        }
        "register" => {
            let id = arg_value(&args, "--id").unwrap_or_default();
            let name = arg_value(&args, "--name").unwrap_or_else(|| id.clone());
            let eco = arg_value(&args, "--ecosystem").unwrap_or_else(|| "unassigned".into());
            if id.trim().is_empty() {
                eprintln!("[assets-serve] register 需要 --id");
                std::process::exit(2);
            }
            let reg = ac_registry::AgentRegistration {
                agent_id: id.clone(),
                display_name: name,
                ecosystem: eco,
                kind: "host".into(),
                endpoint: String::new(),
                meta: "{}".into(),
            };
            match service.register_agent(&reg) {
                Ok(v) => eprintln!("[assets-serve] 已注册 {v}"),
                Err(e) => {
                    eprintln!("[assets-serve] 注册失败: {e}");
                    std::process::exit(1);
                }
            }
        }
        "use" => {
            // 一次"协调调用"：申请路由 + 落审计，返回分配快照。
            let id = arg_value(&args, "--id").unwrap_or_default();
            let cap = arg_value(&args, "--cap").unwrap_or_default();
            if id.trim().is_empty() || cap.trim().is_empty() {
                eprintln!("[assets-serve] use 需要 --id 与 --cap");
                std::process::exit(2);
            }
            let auth = arg_value(&args, "--authority").unwrap_or_else(|| "observe".into());
            let identity = match ac_core::Authority::parse(&auth) {
                Some(a) => ac_mcp::CallerIdentity {
                    authority: a,
                    side_effect: ac_core::SideEffect::None,
                    tool_posture: ac_core::ToolPosture::Reader,
                    ..Default::default()
                },
                None => {
                    eprintln!("[assets-serve] authority 非法: {auth}");
                    std::process::exit(2);
                }
            };
            let caller = ac_mcp::Caller::new(id.clone(), identity);
            match service.acquire_and_audit(&caller, &cap, true, None) {
                Ok(a) => {
                    println!("{}", serde_json::to_string(&a).unwrap_or_default());
                }
                Err(e) => {
                    eprintln!("[assets-serve] 调用失败: {e}");
                    std::process::exit(1);
                }
            }
        }
        "http" => {
            let server = Arc::new(HttpServer::new(service));
            let addr = format!("127.0.0.1:{port}");
            eprintln!("[assets-serve] 启动 HTTP: {addr} (db={})", db_path.display());
            if let Err(e) = server.serve(&addr) {
                eprintln!("[assets-serve] {e}");
                std::process::exit(1);
            }
        }
        other => {
            eprintln!("[assets-serve] 未知模式 {other}");
            print_help();
            std::process::exit(2);
        }
    }
}

fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn print_help() {
    eprintln!("用法:");
    eprintln!("  assets-serve mcp      [--db <path>]");
    eprintln!("  assets-serve http     [--db <path>] [--port N]   (默认端口 18195)");
    eprintln!("  assets-serve register --db <path> --id <agent> [--name N] [--ecosystem E]");
    eprintln!("  assets-serve use      --db <path> --id <agent> --cap <capability_id> [--authority A]");
    eprintln!("  assets-serve workflow --db <data/registry.db> --name r1..r16|dedup|merge [--input <JSON file>]");
    eprintln!("  assets-serve search   --db <data/registry.db> --mode keyword|python|rust|local-vector|cloud-vector --query <text>");
    eprintln!("  assets-serve prepare  --db <data/registry.db> --input <ImportRequest JSON file>");
    eprintln!("  assets-serve commit   --db <data/registry.db> --operation <id> --digest <SHA256>");
    eprintln!("  assets-serve rollback --db <data/registry.db> --asset <id> --revision <SHA256>");
    eprintln!("  assets-serve ingest   --db <data/registry.db> --root <asset directory>");
    eprintln!("  assets-serve history|catalog --db <data/registry.db>");
}
