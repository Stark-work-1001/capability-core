//! CapabilityCore $name
//! Tauri 前后端通信桥
//!
//! M0 阶段：骨架占位。M1 起逐个填充。

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert!(!super::CRATE_NAME.is_empty());
    }
}
