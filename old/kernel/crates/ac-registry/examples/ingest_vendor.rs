//! 把扫描根下的资产导入 SQLite。
//!
//! 开发期用来验证解析器对真实资产的效果，也是**首次建库**的手段。
//!
//! ```text
//! cargo run --release -p ac-registry --example ingest_vendor -- [db路径] [扫描根]
//! ```
//!
//! # 关于「默认写用户的 registry.db」（M2 事故后的硬纪律）
//!
//! 与其它 example 不同，**写入确实是本例程的用途**——它就是建库/导入工具。
//! 但「用途正当」不等于「默认就该改用户的库」：
//! 一次手滑的 `cargo run --example ingest_vendor` 会执行全量扫描、
//! 重写 assets 行、并把浏览器预览快照覆盖掉，而这在终端里看不出任何提示。
//!
//! 因此这里的纪律是：
//! 1. **不带参数运行时完全不写任何东西**，只打印将要做什么然后退出；
//! 2. 要真正写入，必须显式给出**绝对路径**的 db 参数 —— 相对路径默认路径
//!    一律拒绝，因为「相对路径」正是当初那个静默默认的形态；
//! 3. 写入前先把原库 SHA256 记下来，跑完再比一次，把影响面显式化。
//!
//! `Registry::open` 会建表/迁移，**打开本身就是写操作**。
//! 只读的体检请用 `m2_verify_db`（mode=ro）；要跑检索例程请用
//! `m2_e2e` / `m2_fulltext_e2e`，它们都先把库复制到工作目录再打开。

use ac_registry::{ingest, AssetQuery, AssetSummary, Registry};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// `CARGO_MANIFEST_DIR` = `<包根>/kernel/crates/ac-registry`，上溯三层到包根。
fn pack_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 文件 SHA256。用 `std::fs::read` 读原始字节，不经过 SQLite。
fn sha256_of(p: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(p).expect("读取文件算 SHA256");
    let mut h = Sha256::new();
    h.update(&bytes);
    format!("{:x}", h.finalize())
}

/// 浏览器预览用的数据快照。
///
/// Tauri 之外的界面只能看到 mock，看不出真实数据下的排版问题。
/// 这里把库里的真实数据导出一份 JSON，浏览器里直接 `fetch` 它，
/// 迭代界面就不必每次重编 Rust。
#[derive(Serialize)]
struct Preview {
    generated_at_unix: u64,
    db_path: String,
    scan_root: String,
    total: u32,
    counts: Vec<ac_registry::KindCount>,
    items: Vec<PreviewItem>,
    /// M1.5：待用户确认的去重候选。
    ///
    /// 预览模式没有 Tauri 命令可调（`list_dedup_candidates` 走 IPC），
    /// 不带这份数据的话，去重确认面板在浏览器里永远显示"没有待确认的重复项"，
    /// 而那只是因为**拿不到数据**，不是因为真的没有重复——会误导评审。
    dedup_candidates: Vec<ac_core::DedupCandidate>,
    /// M1.5：逻辑资产总数（一条内容 = 一个）
    logical_total: u32,
    /// M1.5：重复位置数
    alias_count: u32,
}

/// 列表行 + 完整 manifest。
///
/// 带上 manifest 是因为浏览器预览模式没有 `get_asset` 命令可用，
/// 不带的话点开一条资产永远出不来详情——而详情面板正是要反复调的地方。
#[derive(Serialize)]
struct PreviewItem {
    #[serde(flatten)]
    summary: AssetSummary,
    manifest: ac_core::CapabilityManifest,
}

fn write_preview(reg: &Registry, root: &Path, db: &Path) -> std::io::Result<()> {
    let summaries = reg
        .query_assets(&AssetQuery {
            limit: 5000,
            ..Default::default()
        })
        .map_err(std::io::Error::other)?;

    let mut items = Vec::with_capacity(summaries.len());
    for s in summaries {
        if let Ok(Some(manifest)) = reg.get_manifest(&s.id) {
            items.push(PreviewItem {
                summary: s,
                manifest,
            });
        }
    }

    let preview = Preview {
        generated_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        db_path: db.display().to_string(),
        scan_root: root.display().to_string(),
        total: reg.total().map_err(std::io::Error::other)?,
        counts: reg
            .counts_by_kind()
            .map_err(std::io::Error::other)?,
        items,
        dedup_candidates: reg.dedup_candidates().map_err(std::io::Error::other)?,
        logical_total: reg.logical_total().map_err(std::io::Error::other)?,
        alias_count: reg.locations_of_count_alias().map_err(std::io::Error::other)?,
    };

    // 本函数的调用点已经过下面的显式授权门（见 main 里的“本次会被覆盖”声明）。
    // 写进前端 public/，vite dev 与 preview 都会把它当静态资源发出。
    // 这里是**第二个会被覆盖的文件**（不是数据库，但同样是静默写入），
    // 所以它不能次序地被当成“无副作用的侧事”。
    let out = pack_root()
        .join("apps")
        .join("desktop")
        .join("public")
        .join("preview.json");
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_string(&preview).map_err(std::io::Error::other)?;
    std::fs::write(&out, &body)?;

    // 同时镜像到 dist/，让「直接打开 dist/index.html」或本地静态 server 也能看到最新数据。
    // 否则 dist/ 下的 preview.json 会停留在上次 vite build 时的旧版本，UI 数量会与数据库不一致。
    // Tauri 构建本身由 vite 把 public/ 嵌进资源，dev 模式则是 vite 直接挂 public/；
    // dist/ 复制是给「脱离 vite」的本地 HTTP server 调试用。
    let dist_out = pack_root()
        .join("apps")
        .join("desktop")
        .join("dist")
        .join("preview.json");
    if let Some(dir) = dist_out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 写失败不能阻塞主流程：拷贝是个便利项，不是关键数据
    if let Err(e) = std::fs::write(&dist_out, &body) {
        eprintln!("同步到 dist/preview.json 失败（不影响入库）: {e}");
    }

    let kb = std::fs::metadata(&out).map(|m| m.len() / 1024).unwrap_or(0);
    let n = preview.items.len();
    println!("预览快照  {}  ({n} 条, {kb} KB)", out.display());
    Ok(())
}

fn main() {
    let root_dir = pack_root();
    let default_db = root_dir.join("data").join("registry.db");

    let mut args = std::env::args().skip(1);
    let db: Option<PathBuf> = args.next().map(PathBuf::from);
    let scan_root: PathBuf = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| root_dir.join("vendor"));

    // ── 无参运行 = 只打印计划，不写任何东西 ────────────────────────
    //
    // 旧版本在这里静默 fallback 到 <包根>/data/registry.db 并直接开写。
    // 「写入是本例程的用途」不构成默认写用户库的理由：无参运行是最容易
    // 被顺手敲出来的那一种，而它恰恰是最不该产生副作用的那一种。
    let Some(db) = db else {
        println!("ingest_vendor：未提供数据库路径，**未做任何写入**。");
        println!();
        println!("本例程会写库（扫描 -> 入库 -> 覆盖浏览器预览快照），所以必须显式指定。");
        println!();
        println!("  数据库  : {}   <- 会被写入", default_db.display());
        println!("  扫描根  : {}", scan_root.display());
        println!();
        println!("重新运行并显式给出路径：");
        println!("  cargo run --release -p ac-registry --example ingest_vendor --");
        println!("      \"{}\" \"{}\"", default_db.display(), scan_root.display());
        println!();
        println!("若只想验证解析器、绝不碰用户库，用只读体检：");
        println!("  cargo run --release -p ac-registry --example m2_verify_db -- \"{}\"", default_db.display());
        std::process::exit(2);
    };

    // ── 拒绝相对路径 ──────────────────────────────────────────────
    //
    // 相对路径正是那个静默默认的形态：命令行看不出来，cwd 一变含义就变。
    // 显式写入必须写到看得见的地方。
    assert!(
        db.is_absolute(),
        "数据库路径必须是**绝对路径**，收到 {}。相对路径无法让人一眼看出要改哪个库，而写错库的代价不可撤销。",
        db.display()
    );

    let before = if db.exists() { Some(sha256_of(&db)) } else { None };

    println!("数据库  : {}   <- 本次会被写入", db.display());
    println!("扫描根  : {}", scan_root.display());
    // D4：下面两个 preview.json 是本次运行的**第二、第三个被覆盖的文件**，
    // 它们不在数据库里，但同样是静默写入。就因为它们不是库，守门
    // 只盯着 Registry::open，完全看不到这两次写入；所以它们必须在这里明写出来。
    println!("会被覆盖    : apps/desktop/public/preview.json   <- 本次会被覆盖");
    println!("              : apps/desktop/dist/preview.json     <- 本次会被覆盖");
    match &before {
        Some(b) => println!("写前SHA : {}", &b[..32]),
        None => println!("写前SHA : (新建文件，本例程会创建它)"),
    }
    println!();

    let t0 = Instant::now();
    let reg = match Registry::open(&db) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("打开数据库失败: {e}");
            std::process::exit(1);
        }
    };

    // 跑两遍：第一遍建库，第二遍验证增量检测确实识别出"无变化"
    let first = match ingest(&reg, &scan_root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("入库失败: {e}");
            std::process::exit(1);
        }
    };
    let second = match ingest(&reg, &scan_root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("二次入库失败: {e}");
            std::process::exit(1);
        }
    };

    println!("── 首次 ──────────────────────────────");
    println!("解析成功  {}", first.parsed);
    println!("新增      {}", first.inserted);
    println!("更新      {}", first.updated);
    println!("未变化    {}", first.unchanged);
    println!("清除      {}", first.removed);
    println!("跳过      {}", first.skipped.len());
    println!("委派成环  {}", if first.has_cycle { "是 ⚠" } else { "否" });
    println!();

    println!("── 二次（应全部未变化）────────────────");
    println!("新增 {} / 更新 {} / 清除 {}  需重建索引: {}",
        second.inserted, second.updated, second.removed,
        if second.has_changes() { "是 ⚠ 不该发生" } else { "否 ✓" });
    println!();

    println!("── 库内统计 ──────────────────────────");
    let total: u32 = first.counts.iter().map(|c| c.count).sum();
    for c in &first.counts {
        println!("  {:<10} {}", c.kind, c.count);
    }
    println!("  {:<10} {}", "合计", total);
    println!();

    if !first.skipped.is_empty() {
        println!("── 跳过的文件（前 20）─────────────────");
        for s in first.skipped.iter().take(20) {
            let name = s.path.rsplit(['\\', '/']).next().unwrap_or(&s.path);
            let dir = s
                .path
                .rsplit(['\\', '/'])
                .nth(1)
                .unwrap_or_default();
            println!("  {dir}/{name}  —— {}", s.reason);
        }
        if first.skipped.len() > 20 {
            println!("  …另有 {} 条", first.skipped.len() - 20);
        }
        println!();
    }

    if let Err(e) = write_preview(&reg, &scan_root, &db) {
        eprintln!("写预览快照失败（不影响入库结果）: {e}");
    }

    // 明示这次写操作的影响面。写入是本例程的正当用途，但「正当」不该等于
    // 「无声」：跑完之后必须能立刻看出改了哪些字节。
    match before {
        Some(b) => {
            let a = sha256_of(&db);
            let changed = if a == b { "未变（本次扫描无增量）" } else { "已变（预期内：本例程就是写库）" };
            println!("写后SHA : {}", &a[..32]);
            println!("影响面  : {}", changed);
        }
        None => println!("影响面  : 新建了 {}", db.display()),
    }

    println!("总耗时 {:?}", t0.elapsed());
}
