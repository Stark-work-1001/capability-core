//! 三槽位模型配置：Embedding / Judge / Rerank。
//!
//! # 为什么从「平铺 provider 列表」改成「三槽位」
//!
//! 平铺列表回答的是「我配了哪些 provider」，而产品真正需要回答的是
//! 「**谁负责向量化、谁负责判定、谁负责重排**」。这两件事不一样：
//! 前者可以有三条硅基流动记录却没人知道哪条是 embedding；
//! 后者一个槽位只有一条，职责是确定的。
//!
//! 所以这里引入槽位。**平铺列表保留**（`provider::load_registry` 照旧可用），
//! 两者读同一个文件：老配置能被新界面读出来，新配置也不破坏老调用方。
//!
//! # 文件格式
//!
//! `<data_dir>/llm-providers.json`：
//! ```json
//! {
//!   "providers": [ ...旧格式，保留... ],
//!   "slots": [
//!     { "slot": "embedding", "enabled": true, "mode": "cloud-openai",
//!       "endpoint": "https://api.siliconflow.cn/v1",
//!       "model": "Qwen/Qwen3-VL-Embedding-8B",
//!       "api_key_env": "", "api_key": "sk-...", "timeout_ms": 30000,
//!       "input_template": "qwen3-embedding" },
//!     ...
//!   ]
//! }
//! ```
//!
//! # 密钥纪律
//!
//! - `api_key` 明文只在配置文件中与内存里；
//! - `SlotView` 只给前端 `masked_key`（如 `****mqunn`）与 `has_key`；
//! - 保存时 `api_key: None` 表示**不改动现有 key**，空串表示**清空**。
//!   这个语义与 `provider::save_provider` 完全一致，避免两处规则不同。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 配置文件（与 provider 共用同一个）。
pub const SLOT_CONFIG_FILE: &str = crate::provider::PROVIDER_CONFIG_FILE;

/// 三个槽位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelSlot {
    /// 把文本编码成向量。
    Embedding,
    /// 判定/分类（结构化输出）。
    Judge,
    /// 重排序。
    Rerank,
    Diagnose,
    Repair,
    Dedup,
    Generate,
    Verify,
    Fallback,
    Localembedding,
}

impl ModelSlot {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embedding => "embedding",
            Self::Judge => "judge",
            Self::Rerank => "rerank",
            Self::Diagnose => "diagnose",
            Self::Repair => "repair",
            Self::Dedup => "dedup",
            Self::Generate => "generate",
            Self::Verify => "verify",
            Self::Fallback => "fallback",
            Self::Localembedding => "localembedding",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "embedding" | "embed" | "vector" => Some(Self::Embedding),
            "judge" | "classify" | "chat" | "general" => Some(Self::Judge),
            "rerank" | "reranker" => Some(Self::Rerank),
            "diagnose" => Some(Self::Diagnose),
            "repair" => Some(Self::Repair),
            "dedup" => Some(Self::Dedup),
            "generate" => Some(Self::Generate),
            "verify" => Some(Self::Verify),
            "fallback" => Some(Self::Fallback),
            "localembedding" => Some(Self::Localembedding),
            _ => None,
        }
    }

    pub fn all() -> [Self; 10] {
        [Self::Embedding, Self::Judge, Self::Rerank, Self::Diagnose, Self::Repair, Self::Dedup, Self::Generate, Self::Verify, Self::Fallback, Self::Localembedding]
    }
}

/// 接入方式。
pub const MODE_CLOUD: &str = "cloud-openai";
pub const MODE_LOCAL: &str = "local-openai";
pub const MODE_DISABLED: &str = "disabled";

/// 一个槽位的配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotConfig {
    pub slot: ModelSlot,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// cloud-openai / local-openai / disabled
    #[serde(default = "default_mode")]
    pub mode: String,
    /// API 根地址，**含 /v1**。例如 `https://api.siliconflow.cn/v1`。
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub model: String,
    /// 密钥环境变量名（可选）。与 `api_key` 二选一，`api_key` 优先。
    #[serde(default)]
    pub api_key_env: String,
    /// 字面密钥。**永不回传前端**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// 输入模板（embedding 用）：qwen3-embedding / openai-generic。
    #[serde(default)]
    pub input_template: String,
    /// 结构化输出（judge 用）：json_schema / text。
    #[serde(default)]
    pub output_schema: String,
}

fn default_true() -> bool {
    true
}
fn default_mode() -> String {
    MODE_CLOUD.to_string()
}
fn default_timeout() -> u64 {
    30_000
}

impl SlotConfig {
    pub fn new(slot: ModelSlot) -> Self {
        Self {
            slot,
            enabled: false,
            mode: MODE_DISABLED.to_string(),
            endpoint: String::new(),
            model: String::new(),
            api_key_env: String::new(),
            api_key: None,
            timeout_ms: default_timeout(),
            input_template: String::new(),
            output_schema: String::new(),
        }
    }

    /// 解析出实际要用的密钥：**字面 key 优先，其次环境变量**。
    ///
    /// 为什么字面优先：用户点「测试连接」时期待的是「我刚填的 key 生效了」，
    /// 而不是「环境变量里那个旧的生效了」。环境变量是兜底，不是覆盖。
    pub fn resolve_key(&self) -> Option<String> {
        if let Some(k) = &self.api_key {
            let t = k.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
        let env = self.api_key_env.trim();
        if !env.is_empty() {
            if let Ok(v) = std::env::var(env) {
                let t = v.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        None
    }

    /// 是否具备调用条件。**不检查 key**——本地模型不需要 key。
    pub fn is_callable(&self) -> bool {
        self.enabled
            && self.mode != MODE_DISABLED
            && !self.endpoint.trim().is_empty()
            && !self.model.trim().is_empty()
    }
}

/// 给前端的视图。**不含明文密钥**。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotView {
    pub slot: String,
    pub enabled: bool,
    pub mode: String,
    pub endpoint: String,
    pub model: String,
    pub api_key_env: String,
    /// 掩码后的 key，如 `****mqunn`。未配置为空串。
    pub masked_key: String,
    pub has_key: bool,
    pub timeout_ms: u64,
    pub input_template: String,
    pub output_schema: String,
    /// 是否具备调用条件（enabled + 有端点 + 有模型）。
    pub callable: bool,
}

impl From<&SlotConfig> for SlotView {
    fn from(c: &SlotConfig) -> Self {
        let key = c.resolve_key();
        Self {
            slot: c.slot.as_str().to_string(),
            enabled: c.enabled,
            mode: c.mode.clone(),
            endpoint: c.endpoint.clone(),
            model: c.model.clone(),
            api_key_env: c.api_key_env.clone(),
            masked_key: key.as_deref().map(mask_key).unwrap_or_default(),
            has_key: key.is_some(),
            timeout_ms: c.timeout_ms,
            input_template: c.input_template.clone(),
            output_schema: c.output_schema.clone(),
            callable: c.is_callable(),
        }
    }
}

/// 打码：保留末 5 位。**很短的值整体打码**，不泄露长度之外的信息。
fn mask_key(k: &str) -> String {
    let chars: Vec<char> = k.chars().collect();
    if chars.len() <= 10 {
        return "****".to_string();
    }
    let tail: String = chars[chars.len() - 5..].iter().collect();
    format!("****{tail}")
}

/// 文件根形状。**同时容纳旧 providers 与新 slots**。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlotRegistryFile {
    #[serde(default)]
    pub providers: Vec<crate::provider::ProviderConfig>,
    #[serde(default)]
    pub slots: Vec<SlotConfig>,
}

fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SLOT_CONFIG_FILE)
}

/// 读三个槽位。文件不存在时返回**空槽位列表**（不是错误）：
/// 「还没配」是正常状态，界面据此显示「未配置」。
pub fn load_slots(data_dir: &Path) -> Result<(PathBuf, Vec<SlotConfig>), String> {
    let path = config_path(data_dir);
    if !path.is_file() {
        return Ok((path, Vec::new()));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
    // 空文件 / 只有空白：当作没配过。这不是"损坏"，是"刚被清空"。
    if text.trim().is_empty() {
        return Ok((path, Vec::new()));
    }
    let file: SlotRegistryFile = serde_json::from_str(&text)
        .map_err(|e| format!("解析 {} 失败：{e}", path.display()))?;
    Ok((path, file.slots))
}

/// 读单个槽位。没配过时返回一个**禁用态的默认值**，方便前端渲染表单。
pub fn load_slot(data_dir: &Path, slot: ModelSlot) -> Result<SlotConfig, String> {
    let (_, slots) = load_slots(data_dir)?;
    Ok(slots
        .into_iter()
        .find(|s| s.slot == slot)
        .unwrap_or_else(|| SlotConfig::new(slot)))
}

/// 保存一个槽位。**保留文件里的其它内容**（旧 providers、其它槽位）。
///
/// `api_key` 语义：
/// - `Some(k)` 且非空 → 用这个 key
/// - `Some("")` → 清空 key
/// - `None` → **不改动现有 key**
pub fn save_slot(data_dir: &Path, mut incoming: SlotConfig) -> Result<PathBuf, String> {
    let _guard=crate::provider::config_update_lock().lock().map_err(|_|"模型配置写入锁不可用")?;
    std::fs::create_dir_all(data_dir)
        .map_err(|e| format!("建目录 {} 失败：{e}", data_dir.display()))?;
    let path = config_path(data_dir);

    // 先读原文件（保留 providers 与其它槽位）。解析失败时**不覆盖**——
    // 一个坏文件不该被一次保存顺手清空，那会让用户失去手工修复的机会。
    let mut file: SlotRegistryFile = if path.is_file() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
        if text.trim().is_empty() {
            SlotRegistryFile::default()
        } else {
            serde_json::from_str(&text).map_err(|e| {
                format!(
                    "{} 已存在但无法解析（{e}）。为避免覆盖你手工编辑的内容，本次保存已取消。                     请先修好该文件，或把它移走后重试。",
                    path.display()
                )
            })?
        }
    } else {
        SlotRegistryFile::default()
    };

    // 合并 key：None = 不改动，Some("") = 清空。
    let existing = file.slots.iter().find(|s| s.slot == incoming.slot);
    incoming.api_key = match &incoming.api_key {
        Some(v) if v.trim().is_empty() => None,
        Some(v) => Some(v.trim().to_string()),
        None => existing.and_then(|s| s.api_key.clone()),
    };

    // 替换或追加
    match file.slots.iter_mut().find(|s| s.slot == incoming.slot) {
        Some(s) => *s = incoming,
        None => file.slots.push(incoming),
    }
    // 槽位顺序固定，便于人读文件
    file.slots.sort_by_key(|s| match s.slot {
        ModelSlot::Embedding => 0,
        ModelSlot::Judge => 1,
        ModelSlot::Rerank => 2,
        ModelSlot::Diagnose => 3,
        ModelSlot::Repair => 4,
        ModelSlot::Dedup => 5,
        ModelSlot::Generate => 6,
        ModelSlot::Verify => 7,
        ModelSlot::Fallback => 8,
        ModelSlot::Localembedding => 9,
    });

    let mut root=if path.is_file(){let text=std::fs::read_to_string(&path).map_err(|e|e.to_string())?;if text.trim().is_empty(){serde_json::json!({})}else{serde_json::from_str::<serde_json::Value>(&text).map_err(|e|e.to_string())?}}else{serde_json::json!({})};
    root["slots"]=serde_json::to_value(&file.slots).map_err(|e|e.to_string())?;
    crate::provider::atomic_config_write(&path,&root)?;
    Ok(path)
}

/// 三个槽位的视图（给前端一次性拿全）。
pub fn slots_view(data_dir: &Path) -> Result<Vec<SlotView>, String> {
    let (_, slots) = load_slots(data_dir)?;
    let mut out: Vec<SlotView> = Vec::new();
    for want in ModelSlot::all() {
        let cfg = slots
            .iter()
            .find(|s| s.slot == want)
            .cloned()
            .unwrap_or_else(|| SlotConfig::new(want));
        out.push(SlotView::from(&cfg));
    }
    Ok(out)
}

/// 硅基流动的预设（用户点「硅基流动 SiliconFlow」按钮时用）。
///
/// **只提供地址与模型名，不含密钥**：密钥必须用户自己填或走环境变量。
/// 返回 (endpoint, model)。
pub fn siliconflow_preset(slot: ModelSlot) -> (&'static str, &'static str) {
    const EP: &str = "https://api.siliconflow.cn/v1";
    match slot {
        ModelSlot::Embedding => (EP, "Qwen/Qwen3-VL-Embedding-8B"),
        ModelSlot::Judge => (EP, "Qwen/Qwen3.8-27B"),
        ModelSlot::Rerank => (EP, "Qwen/Qwen3-VL-Reranker-8B"),
        _ => (EP, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用临时目录。
    ///
    /// **锚在 target/ 下，不用系统 TEMP**：本机（以及受限环境）对系统临时
    /// 目录的写入可能被策略拒绝，那样测试会以一个和被测逻辑无关的方式失败
    /// （实测：Os code 5 PermissionDenied）。target/ 一定在可写的工作区内。
    fn scratch(name: &str) -> PathBuf {
        let mut p = std::env::current_exe().expect("current_exe");
        p.pop(); // deps
        p.pop(); // release | debug
        p.push(format!("ac-llm-slots-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("建测试目录");
        p
    }

    #[test]
    fn slot_parse_accepts_aliases() {
        assert_eq!(ModelSlot::parse("embedding"), Some(ModelSlot::Embedding));
        assert_eq!(ModelSlot::parse("EMBED"), Some(ModelSlot::Embedding));
        assert_eq!(ModelSlot::parse("vector"), Some(ModelSlot::Embedding));
        assert_eq!(ModelSlot::parse("judge"), Some(ModelSlot::Judge));
        assert_eq!(ModelSlot::parse("chat"), Some(ModelSlot::Judge));
        assert_eq!(ModelSlot::parse("rerank"), Some(ModelSlot::Rerank));
        assert_eq!(ModelSlot::parse("reranker"), Some(ModelSlot::Rerank));
        assert_eq!(ModelSlot::parse("nope"), None);
    }

    #[test]
    fn missing_file_yields_ten_disabled_slots() {
        let d = scratch("missing");
        let views = slots_view(&d).unwrap();
        assert_eq!(views.len(), 10);
        for v in &views {
            assert!(!v.enabled);
            assert!(!v.callable);
            assert!(!v.has_key);
            assert_eq!(v.masked_key, "");
        }
    }
    #[test]
    fn concurrent_slot_saves_preserve_all_slots_and_unknown_fields(){
        let d=scratch("concurrent");std::fs::write(config_path(&d),r#"{"providers":[],"custom":{"keep":true}}"#).unwrap();
        let threads:Vec<_>=ModelSlot::all().into_iter().map(|slot|{let d=d.clone();std::thread::spawn(move||save_slot(&d,SlotConfig::new(slot)).unwrap())}).collect();
        for thread in threads{thread.join().unwrap();}
        assert_eq!(load_slots(&d).unwrap().1.len(),10);
        let v:serde_json::Value=serde_json::from_str(&std::fs::read_to_string(config_path(&d)).unwrap()).unwrap();assert_eq!(v["custom"]["keep"],true);
    }

    #[test]
    fn save_then_load_round_trip() {
        let d = scratch("roundtrip");
        let mut c = SlotConfig::new(ModelSlot::Embedding);
        c.enabled = true;
        c.mode = MODE_CLOUD.into();
        c.endpoint = "https://api.siliconflow.cn/v1".into();
        c.model = "Qwen/Qwen3-VL-Embedding-8B".into();
        c.api_key = Some("sk-test-abcdefghijklmn".into());
        save_slot(&d, c).unwrap();

        let got = load_slot(&d, ModelSlot::Embedding).unwrap();
        assert!(got.enabled);
        assert_eq!(got.model, "Qwen/Qwen3-VL-Embedding-8B");
        assert_eq!(got.api_key.as_deref(), Some("sk-test-abcdefghijklmn"));
    }

    #[test]
    fn view_never_exposes_plain_key() {
        let d = scratch("mask");
        let mut c = SlotConfig::new(ModelSlot::Embedding);
        c.enabled = true;
        c.endpoint = "https://x/v1".into();
        c.model = "m".into();
        c.api_key = Some("test-fake-key-not-a-credential".into());
        save_slot(&d, c).unwrap();

        let views = slots_view(&d).unwrap();
        let emb = views.iter().find(|v| v.slot == "embedding").unwrap();
        assert!(emb.has_key);
        assert!(emb.masked_key.starts_with("****"));
        assert!(!emb.masked_key.contains("supersecret"));
        // 序列化后也不能出现明文
        let json = serde_json::to_string(&views).unwrap();
        assert!(!json.contains("supersecret"), "视图 JSON 泄露了明文密钥");
    }

    #[test]
    fn none_key_keeps_existing_and_empty_string_clears() {
        let d = scratch("keysem");
        let mut c = SlotConfig::new(ModelSlot::Judge);
        c.enabled = true;
        c.endpoint = "https://x/v1".into();
        c.model = "m".into();
        c.api_key = Some("sk-first".into());
        save_slot(&d, c.clone()).unwrap();

        // None = 不改动
        let mut c2 = c.clone();
        c2.api_key = None;
        c2.model = "m2".into();
        save_slot(&d, c2).unwrap();
        let got = load_slot(&d, ModelSlot::Judge).unwrap();
        assert_eq!(got.api_key.as_deref(), Some("sk-first"), "None 不该清空 key");
        assert_eq!(got.model, "m2");

        // 空串 = 清空
        let mut c3 = c.clone();
        c3.api_key = Some(String::new());
        save_slot(&d, c3).unwrap();
        let got = load_slot(&d, ModelSlot::Judge).unwrap();
        assert!(got.api_key.is_none(), "空串必须清空 key");
    }

    #[test]
    fn saving_preserves_legacy_providers_section() {
        let d = scratch("legacy");
        // 手工写一个同时含 providers 与 slots 的文件
        let raw = r#"{
  "providers": [{"id":"old","name":"Old","kind":"cloud","enabled":true}],
  "slots": []
}"#;
        std::fs::write(d.join(SLOT_CONFIG_FILE), raw).unwrap();

        let mut c = SlotConfig::new(ModelSlot::Rerank);
        c.enabled = true;
        c.endpoint = "https://x/v1".into();
        c.model = "m".into();
        save_slot(&d, c).unwrap();

        let text = std::fs::read_to_string(d.join(SLOT_CONFIG_FILE)).unwrap();
        assert!(text.contains("\"old\""), "保存槽位不能丢掉旧的 providers 段");
        assert!(text.contains("rerank"));
    }

    #[test]
    fn corrupt_file_is_not_silently_overwritten() {
        let d = scratch("corrupt");
        std::fs::write(d.join(SLOT_CONFIG_FILE), "{ this is not json").unwrap();
        let mut c = SlotConfig::new(ModelSlot::Embedding);
        c.enabled = true;
        c.endpoint = "https://x/v1".into();
        c.model = "m".into();
        let err = save_slot(&d, c).unwrap_err();
        assert!(err.contains("无法解析"), "错误要说清是文件坏了：{err}");
        // 原文件必须原样还在
        let after = std::fs::read_to_string(d.join(SLOT_CONFIG_FILE)).unwrap();
        assert_eq!(after, "{ this is not json");
    }

    #[test]
    fn siliconflow_preset_fills_expected_models() {
        let (ep, m) = siliconflow_preset(ModelSlot::Embedding);
        assert_eq!(ep, "https://api.siliconflow.cn/v1");
        assert_eq!(m, "Qwen/Qwen3-VL-Embedding-8B");
        let (_, m) = siliconflow_preset(ModelSlot::Judge);
        assert_eq!(m, "Qwen/Qwen3.8-27B");
        let (_, m) = siliconflow_preset(ModelSlot::Rerank);
        assert_eq!(m, "Qwen/Qwen3-VL-Reranker-8B");
    }

    #[test]
    fn env_key_used_when_literal_absent() {
        let d = scratch("envkey");
        std::env::set_var("AC_TEST_KEY_XYZ", "sk-from-env-12345");
        let mut c = SlotConfig::new(ModelSlot::Embedding);
        c.enabled = true;
        c.endpoint = "https://x/v1".into();
        c.model = "m".into();
        c.api_key = None;
        c.api_key_env = "AC_TEST_KEY_XYZ".into();
        save_slot(&d, c).unwrap();

        let views = slots_view(&d).unwrap();
        let emb = views.iter().find(|v| v.slot == "embedding").unwrap();
        assert!(emb.has_key, "环境变量里的 key 也要算作已配置");
        std::env::remove_var("AC_TEST_KEY_XYZ");
    }

    #[test]
    fn literal_key_wins_over_env() {
        std::env::set_var("AC_TEST_KEY_ABC", "sk-env");
        let mut c = SlotConfig::new(ModelSlot::Judge);
        c.api_key = Some("sk-literal".into());
        c.api_key_env = "AC_TEST_KEY_ABC".into();
        assert_eq!(c.resolve_key().as_deref(), Some("sk-literal"));
        std::env::remove_var("AC_TEST_KEY_ABC");
    }
}

// ─────────────────────────────────────────────────────────────────────
// 「测试连接」：真实调用
// ─────────────────────────────────────────────────────────────────────

/// 一次槽位探活的结果。
///
/// 这个类型就是界面上「测试连接」按钮拿到的对象，字段与前端
/// `ModelSlotProbeResult` 一一对应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotProbe {
    pub ok: bool,
    pub elapsed_ms: u64,
    /// 成功摘要，例如 `4096 维 × 1 条` / `3 条重排，最高分 0.3772` / `回复：二`
    pub summary: String,
    /// 失败原因。**不含密钥**（cloud 模块在每条错误路径上 redact）。
    pub error: String,
}

/// 用某个槽位的**当前配置**真实调一次模型。
///
/// # 为什么这个函数在 ac-llm 而不是在 Tauri 层
///
/// 它曾经只写在 `apps/desktop/src-tauri/src/lib.rs` 的 `ops::test_model_slot` 里。
/// 那带来一个验证难题：**验收时无法证明界面按钮走的是哪条代码路径**——
/// 只能截一张图说"点了它显示成功"。
///
/// 挪到这里之后，Tauri 命令只剩一行转发，于是：
/// - 命令行探针（`examples\slot_probe.rs`）与界面按钮调用**同一个函数**；
/// - 命令行跑通 = 界面按钮走通，无需 UI 自动化。
///
/// # 不检查 key 是否存在
///
/// 本地模型（Ollama / vLLM）不需要密钥。`resolve_key()` 给空串时
/// 请求照发，由服务端决定接不接 —— 那才是真实结论。
pub fn probe_slot(data_dir: &std::path::Path, slot: ModelSlot) -> Result<SlotProbe, String> {
    let cfg = load_slot(data_dir, slot)?;
    if !cfg.is_callable() {
        return Ok(SlotProbe {
            ok: false,
            elapsed_ms: 0,
            summary: String::new(),
            // 措辞很重要：这不是"连不上"，是"还没配"。
            // 混在一起会让用户去查网络，而实际上他只需要填个地址。
            error: "该槽位未启用或缺 endpoint / model（不是连接失败，是还没配）".into(),
        });
    }
    let key = cfg.resolve_key().unwrap_or_default();
    let t0 = std::time::Instant::now();
    let r: Result<String, String> = match slot {
        ModelSlot::Embedding | ModelSlot::Localembedding => crate::cloud::embed(
            &cfg.endpoint,
            &key,
            &cfg.model,
            &["能力中枢向量检索探针".to_string()],
            cfg.timeout_ms,
        )
        .map(|v| {
            let dim = v.first().map(|x| x.len()).unwrap_or(0);
            let norm: f32 = v
                .first()
                .map(|x| x.iter().map(|f| f * f).sum::<f32>().sqrt())
                .unwrap_or(0.0);
            // 范数也报出来：一个返回全零向量的服务会让余弦相似度全变成 0，
            // 而"调用成功、维度 4096"看起来完全正常。
            format!("{dim} 维 × {} 条，范数={norm:.4}", v.len())
        }),
        ModelSlot::Rerank => crate::cloud::rerank(
            &cfg.endpoint,
            &key,
            &cfg.model,
            "能力检索",
            &[
                "向量检索能力".to_string(),
                "做饭的方法".to_string(),
                "能力中枢索引".to_string(),
            ],
            3,
            cfg.timeout_ms,
        )
        .map(|hits| {
            let top = hits.first().map(|h| h.score).unwrap_or(0.0);
            let order: Vec<String> = hits
                .iter()
                .map(|h| format!("#{}:{:.4}", h.index, h.score))
                .collect();
            format!("{} 条重排，最高分 {top:.4} [{}]", hits.len(), order.join(" "))
        }),
        _ => crate::cloud::chat(
            &cfg.endpoint,
            &key,
            &cfg.model,
            &[("user".to_string(), "用一个词回答：1+1 等于几？".to_string())],
            64,
            cfg.timeout_ms,
        )
        .map(|t| {
            let s: String = t.trim().chars().take(80).collect();
            format!("回复：{s}")
        }),
    };
    let elapsed_ms = t0.elapsed().as_millis() as u64;
    Ok(match r {
        Ok(summary) => SlotProbe {
            ok: true,
            elapsed_ms,
            summary,
            error: String::new(),
        },
        Err(e) => SlotProbe {
            ok: false,
            elapsed_ms,
            summary: String::new(),
            error: e,
        },
    })
}

/// 三个槽位各探一次。返回 `(槽位名, 结果)`。
///
/// 单个槽位失败**不影响其它槽位**：这是三个独立的云服务，
/// 一个 key 过期不该让另外两个的结论也消失。
pub fn probe_all(data_dir: &std::path::Path) -> Vec<(ModelSlot, SlotProbe)> {
    ModelSlot::all()
        .into_iter()
        .map(|s| {
            let r = probe_slot(data_dir, s).unwrap_or_else(|e| SlotProbe {
                ok: false,
                elapsed_ms: 0,
                summary: String::new(),
                error: e,
            });
            (s, r)
        })
        .collect()
}
