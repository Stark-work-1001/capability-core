// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod runtime;

fn main() {
    // Prefer the bundled Fixed Version runtime before Tauri starts any threads.
    if let Ok(exe)=std::env::current_exe() {
        if let Some(root)=exe.parent() {
            let runtime=root.join("runtime/webview2");
            if runtime.join("msedgewebview2.exe").is_file() {
                let location=runtime::bundled(root).unwrap_or_else(|e|{eprintln!("[assets] 运行时缓存未就绪，尝试随包目录: {e}");runtime});
                std::env::set_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",location);
            }
            // A candidate must coexist with the user's daily standalone.
            if root.join("CANDIDATE.json").is_file() && std::env::var_os("CORE_HTTP_PORT").is_none() {
                std::env::set_var("CORE_HTTP_PORT","18297");
            }
        }
    }
    assets_desktop_lib::run()
}
