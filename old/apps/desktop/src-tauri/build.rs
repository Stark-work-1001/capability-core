use std::path::PathBuf;
use std::process::Command;

fn main() {
    // 捕获实际编译器版本，供前端握手时显示
    let rustc = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()))
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=AC_RUSTC_VERSION={rustc}");

    // 版本标识（devel / standalone）——图标、端口、窗口标题、界面徽标都依赖它
    let ed = edition();
    println!("cargo:rustc-env=CORE_EDITION={ed}");
    println!("cargo:warning=assets-desktop edition = {ed}");

    // `mobile` 是 Tauri 模板引入的自定义 cfg，声明它以避免 unexpected_cfgs 警告
    println!("cargo:rustc-check-cfg=cfg(mobile)");

    // ── 必须走 tauri_build，不能自己拼链接器开关 ──────────────────
    //
    // 历史：本项目原先在 build.rs 里手写
    //     cargo:rustc-link-arg-bins=/MANIFEST:EMBED
    //     cargo:rustc-link-arg-bins=/MANIFESTINPUT:<path>
    // 让 MSVC 链接器把 Common-Controls manifest 嵌进 exe。
    // 这**确实**解决了启动崩溃（0xC0000139 / TaskDialogIndirect），
    // 但它只管 manifest：exe 的 **RT_ICON / RT_GROUP_ICON / RT_VERSION**
    // 三类资源全都没有——所以任务栏、Alt-Tab、属性页里都看不到图标。
    //
    // 这三类资源只有 `tauri_build` 的 windows 分支会产出：它内部用
    // `tauri_winres::WindowsResource` 编译一个 .rc，其中同时写入
    // manifest、icon（`set_icon_with_id`）与 version info。见
    // tauri-build-2.7.3/src/lib.rs:793-876。
    //
    // **两者不能叠加**：手写 /MANIFEST:EMBED 与 winres 生成的 manifest
    // 会同时进链接，出现多份同 id 资源 → LNK 冲突或运行时读到旧那份。
    // 所以这里把 manifest 交给 `WindowsAttributes::app_manifest`，
    // 手写链接器开关全部删除。
    embed_windows_resources();

    // 前端产物变化时也要重跑，否则嵌入的 dist/ 会是旧的
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=resources");
    println!("cargo:rerun-if-changed=icons");
}

/// 构建版本标识：`devel`（开发测试）或 `standalone`（正式运行）。
///
/// 为什么要在**构建期**定死而不是运行时猜：
/// - 图标、版本资源、窗口标题、HTTP 端口都要按版本区分，
///   而这些里有一半是编译期资源（exe 的 RT_ICON / RT_VERSION）；
/// - 运行时猜的话，同一个 exe 被拷到别的目录就会"变版本"，
///   用户分不清哪个是自己在测的、哪个是日常在用的。
///
/// 取值优先级：
/// 1. 环境变量 `CORE_EDITION`（显式指定）
/// 2. 编译时的 `--features standalone`（Cargo feature）
/// 3. 默认 `devel`
///
/// 输出 `cargo:rustc-env=CORE_EDITION=...` 供 src/lib.rs 用
/// `env!("CORE_EDITION")` 读取。
fn edition() -> &'static str {
    if let Ok(v) = std::env::var("CORE_EDITION") {
        let v = v.trim().to_ascii_lowercase();
        if v == "standalone" {
            return "standalone";
        }
        if v == "devel" {
            return "devel";
        }
        panic!("CORE_EDITION 只能是 'devel' 或 'standalone'，收到: {v}");
    }
    // CARGO_FEATURE_STANDALONE 由 cargo 在启用 standalone feature 时注入
    if std::env::var_os("CARGO_FEATURE_STANDALONE").is_some() {
        return "standalone";
    }
    "devel"
}

/// 该版本对应的图标目录名。
fn icon_dir_for(edition: &str) -> &'static str {
    if edition == "standalone" {
        "standalone"
    } else {
        "devel"
    }
}

/// 把 manifest / 图标 / 版本信息交给 tauri-build 统一编译进 exe。
///
/// 只在 Windows + MSVC 下有意义；其它平台什么都不做。
fn embed_windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // 必须用**运行时**环境变量，不能用 env!()。
    //
    // env!() 在编译 build.rs 时就把路径固化进了 build-script 二进制。
    // 项目目录一旦搬家（本项目从 C:\external-assets\ 搬到 C:\work\），
    // 复用的 target/ 缓存里那份 build-script 仍指向旧路径，于是
    // "图标缺失 / manifest not found" 会在新目录下永远复现——
    // 而 icons/ 明明就在旁边。cargo 只在 build.rs 源码变更时才重编它，
    // 所以这种错会一直躲在缓存里。
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));

    // 应用清单：Common-Controls v6（TaskDialogIndirect 依赖它）+ DPI 感知 + 支持的系统。
    // 内容与改造前一致，只是改由 winres 编译，而不是走链接器开关。
    let manifest_path = root.join("resources").join("assets-desktop.exe.manifest");
    let manifest = if manifest_path.is_file() {
        println!("cargo:rerun-if-changed={}", manifest_path.display());
        std::fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
            panic!("读不到 manifest {}: {e}", manifest_path.display())
        })
    } else {
        // 退回到 tauri 的默认清单（只含 Common-Controls 依赖）。
        // 不 panic：缺自定义清单时仍要能出包，只是少了 DPI/兼容性声明。
        println!(
            "cargo:warning=manifest not found at {}; falling back to tauri default",
            manifest_path.display()
        );
        String::new()
    };

    // 图标：按版本选子目录（icons/devel 或 icons/standalone）。
    // .ico 必须存在，否则 tauri_build 会直接报错（它拒绝生成无图标资源）。
    let ed = edition();
    let icon_path = root.join("icons").join(icon_dir_for(ed)).join("icon.ico");
    if !icon_path.is_file() {
        panic!(
            "图标缺失：{}（edition={ed}）。任务栏图标依赖它，不能跳过——\
             请运行 `python artifacts/reports/_probes/gen_icon_{ed}.py` 重新生成。",
            icon_path.display()
        );
    }
    println!("cargo:rerun-if-changed={}", root.join("icons").join(icon_dir_for(ed)).display());

    let mut windows = tauri_build::WindowsAttributes::new_without_app_manifest()
        .window_icon_path(&icon_path);
    if !manifest.is_empty() {
        windows = windows.app_manifest(manifest);
    }

    let attrs = tauri_build::Attributes::new().windows_attributes(windows);
    tauri_build::try_build(attrs).expect("tauri-build failed");
}
