//! 测试期共用的临时目录工具。
//!
//! # 为什么不直接用 `std::env::temp_dir()`
//!
//! 本机的执行环境给子进程加了**路径白名单**：只有工作区
//! （`C:\external-assets\capability-core`）内允许创建目录，往
//! `%TEMP%` / `%USERPROFILE%` / 盘根建目录一律返回
//! `PermissionDenied (os error 5)`。注意这个限制只拦"创建"，
//! 读取是正常的，所以它不会以任何显眼的方式暴露出来——
//! 表现为测试莫名其妙地 ACCESS_DENIED，而同样的路径用
//! PowerShell / cmd / python 建却又是成功的，非常容易误判成
//! 代码 bug 或并行竞态。
//!
//! 因此单元测试的落盘位置必须锚在 `target/` 里，它一定在
//! 工作区内，且随 `cargo clean` 一起消失。
//!
//! [`Scratch`] 还顺带解决了并行问题：`cargo test` 是多线程跑的，
//! 多个测试共用同一个目录名会互相删/建，必须每次给唯一名字。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

/// 落盘根目录。
///
/// 集成测试能拿到 `CARGO_TARGET_TMPDIR`；单元测试拿不到，
/// 于是从测试二进制的位置反推 `<target>/release/ac-test-tmp`。
pub fn scratch_root() -> PathBuf {
    if let Some(d) = std::env::var_os("CARGO_TARGET_TMPDIR") {
        return PathBuf::from(d);
    }
    let mut p = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    p.pop(); // .../deps
    p.pop(); // .../release | .../debug
    p.join("ac-test-tmp")
}

/// 一次性临时目录，Drop 时自动删除。
pub struct Scratch {
    path: PathBuf,
    root: PathBuf,
}

impl Scratch {
    /// 创建一个干净的临时目录。`tag` 只为方便人肉辨认，唯一性由计数器保证。
    pub fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = scratch_root();
        let _ = std::fs::create_dir_all(&root);
        let path = root.join(format!("{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|e| panic!("造测试目录失败 {}: {e}", path.display()));
        Self { path, root }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 造子目录；失败时把完整路径打出来，否则光看
    /// "拒绝访问" 无从判断是哪个目录。
    pub fn mkdir(&self, rel: &str) -> PathBuf {
        let p = self.path.join(rel);
        if let Err(e) = std::fs::create_dir_all(&p) {
            panic!("create_dir_all 失败: {} -> {e}", p.display());
        }
        p
    }

    /// 写文件。
    pub fn write(&self, rel: &str, body: &str) -> PathBuf {
        if let Some(parent) = Path::new(rel).parent().filter(|p| !p.as_os_str().is_empty()) {
            self.mkdir(&parent.to_string_lossy());
        }
        let p = self.path.join(rel);
        if let Err(e) = std::fs::write(&p, body) {
            panic!("write 失败: {} -> {e}", p.display());
        }
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        // 最后一棒顺手把根目录也收了，保持 target/ 干净
        if self.path.parent() == Some(self.root.as_path()) {
            let _ = std::fs::remove_dir(&self.root);
        }
    }
}
