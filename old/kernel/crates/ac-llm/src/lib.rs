//! # ac-llm
//!
//! 模型接入管理：本地 Ollama / 云端 API / OpenAI 兼容。
//!
//! ## 本轮做到哪一步（M3 第一批产品化）
//!
//! 只做**接入位**，不做模型层：
//!
//! - `provider::LlmProvider` —— 厂商无关的能力抽象（trait 先定死）；
//! - `provider::ProviderConfig` + `load_registry` —— 配置位置与读取；
//! - `provider::ProviderStatus::NotImplemented` —— **明确的"未接入"提示**。
//!
//! 任何 `diagnose()` 调用都拿不到"空实现假装成功"的结论：
//! 未实现的 provider 只能返回 `NotImplemented`，那一档不携带任何结论。
//!
//! ## 配置位置
//!
//! `<data_dir>/llm-providers.json`。文件不存在是**正常状态**（还没人配过），
//! 不是错误；UI 会显示"暂无数据 + 去哪儿配"。
//!
//! ## 为什么不做本地算法
//!
//! 本地算法（结构化过滤 + BM25）不在本 crate：它在 `ac-retrieval`，
//! 由检索层与维修站直接调用。本 crate 只负责"接一个模型进来"，
//! 两者不重叠，避免同一个能力有两份实现。

pub mod cloud;
pub mod broker;
pub mod provider;
pub mod slots;

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert!(!super::CRATE_NAME.is_empty());
    }
}
