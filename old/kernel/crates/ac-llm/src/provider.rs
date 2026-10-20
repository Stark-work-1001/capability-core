//! # ac-llm —— 模型接入的**接入位**，不是模型层
//!
//! ## 本轮范围（刻意克制）
//!
//! 用户原话：「能力核心自带算法驱动（检索、修复），先建多协议支持；
//! 协议测试不通过时，再接云端或本地模型来驱动故障解决。」
//!
//! 所以本轮只交付三样：
//!
//! 1. [`LlmProvider`] trait —— 厂商无关的能力抽象（判断 + 建议 + 判定）；
//! 2. [`ProviderConfig`] + [`load_registry`] —— 配置位置与读取；
//! 3. **明确的"未接入"提示** —— [`ProviderStatus::NotImplemented`] 是唯一的实现，
//!    任何 `diagnose()` 调用都会返回它，而不是悄悄走空实现假装成功。
//!
//! ## 为什么不允许"空实现"
//!
//! 一个返回 `Ok("")` 的 provider 会让上层以为"LLM 判定过了，结论是没问题"
//! ——这是最危险的一种假数据。这里让未实现的 provider **在类型上就无法被当成结论**：
//! [`ProviderStatus::NotImplemented`] 不携带任何结论字段。
//!
//! ## 配置位置
//!
//! `<data_dir>/llm-providers.json`。**文件不存在是正常状态**，
//! 表示"还没人配过"，不是错误；[`load_registry`] 返回空列表 + 明确的 path，
//! 由 UI 显示"暂无数据 + 在哪里配置"。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 配置文件名（放在数据目录下）。
pub const PROVIDER_CONFIG_FILE: &str = "llm-providers.json";

/// 一个模型 provider 的配置。**只有配置，没有实现。**
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// 稳定的 provider id，例如 "ollama" / "openai-compatible"。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 本地还是云端。UI 分组用。
    pub kind: String,
    /// 端点。**本轮只存不发**：没有任何代码会拿它去建连接。
    #[serde(default)]
    pub endpoint: String,
    /// 模型名。同上，只存不发。
    #[serde(default)]
    pub model: String,
    /// 是否启用。禁用项仍然会被列出来，只是状态不同——
    /// "有个 provider 但被关了"和"没有 provider"是两回事。
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 协议类型：决定请求体怎么拼。
    ///
    /// **通用格式的关键**：不按厂商分支，只按协议分支。目前两种覆盖绝大多数：
    /// - `openai` —— OpenAI Chat Completions / Embeddings 兼容（硅基流动、阿里百炼、
    ///   智谱、DeepSeek、vLLM、Ollama 的 /v1 端点……都走这个）
    /// - `anthropic` —— Anthropic Messages 格式
    ///
    /// 空值按 `openai` 处理：本地模型（Ollama / LM Studio / llama.cpp server）
    /// 现在几乎都提供 OpenAI 兼容端点，默认它最不容易错。
    #[serde(default)]
    pub protocol: String,

    /// API key。
    ///
    /// **本地模型通常不需要**，留空即可。
    /// **这个字段必须默认不出现在界面上**——见 `ProviderView.masked_key`：
    /// 视图层只拿到掩码，明文只在内存里，永远不回传给前端。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

fn default_true() -> bool {
    true
}

/// 配置文件根形状。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderRegistryFile {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
}

/// 一个 provider 当前的状态。**穷举，且每一档都不含结论。**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum ProviderStatus {
    /// 配置文件里没有这个 provider。
    NotConfigured,
    /// 配了但被显式关闭。
    Disabled,
    /// 配了、开着，但**本轮没有实现厂商调用**。
    ///
    /// 这是当前唯一可达的"已配置"状态。携带的 reason 会原样显示给用户，
    /// 不做任何"其实能调"的暗示。
    NotImplemented {
        reason: String,
    },
    /// 已实现真实调用。**本轮不可达**——没有任何代码路径能构造出这一档，
    /// 它只为把接口先定死：将来接厂商时唯一要新增的构造点就在这里。
    Ready {
        detail: String,
    },
}

/// provider 的完整画像，直接给 UI 用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub endpoint: String,
    pub model: String,
    pub enabled: bool,
    pub status: ProviderStatus,
    /// 协议类型（`openai` / `anthropic`）。
    pub protocol: String,
    /// **掩码后的 key**，供界面回显「有没有配」与末四位。
    ///
    /// 明文**永远不出现在这个视图里**：前端没有理由拿到它，
    /// 而一旦它进了前端，就会顺带进入 DevTools、崩溃转储与截图。
    /// 真正需要明文的只有发请求那一处，从 `ProviderConfig` 直接取。
    ///
    /// 未配置时为空串；配置了但很短时整体打码，不泄露长度之外的信息。
    #[serde(default)]
    pub masked_key: String,
    /// 是否已配置 key（本地模型通常没有，不算异常）。
    #[serde(default)]
    pub has_key: bool,
}

impl ProviderStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderStatus::NotConfigured => "not_configured",
            ProviderStatus::Disabled => "disabled",
            ProviderStatus::NotImplemented { .. } => "not_implemented",
            ProviderStatus::Ready { .. } => "ready",
        }
    }
}

/// 故障判定请求。**字段刻意做成纯数据**：
/// provider 不接触文件系统，也拿不到连接，
/// 它只能对「已经整理好的事实」做判断——这样才不会有人
/// 在"接模型"的名义下顺手改用户的文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosisRequest {
    pub diagnostic_id: String,
    pub category: String,
    pub title: String,
    /// 错误原文。
    pub detail: String,
    /// 中枢自带的判定：auto / confirm / manual。
    pub repairability: String,
}

/// 故障判定结论。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosisReply {
    pub diagnostic_id: String,
    /// 建议采取的动作（人话）。
    pub advice: String,
    /// 置信度 0.0–1.0。
    pub confidence: f32,
}

/// 模型 provider 抽象。**本轮没有任何实现类型满足它**——
/// trait 存在的意义是让"接口先定死"，而不是留一个空壳骗自己。
pub trait LlmProvider: Send + Sync {
    /// provider id。
    fn id(&self) -> &str;
    /// 当前状态。
    fn status(&self) -> ProviderStatus;
    /// 对一条故障给出建议。
    fn diagnose(&self, req: &DiagnosisRequest) -> Result<DiagnosisReply, String>;
}

/// 配置文件路径：`<data_dir>/llm-providers.json`。
pub fn provider_config_path(data_dir: &Path) -> PathBuf {
    data_dir.join(PROVIDER_CONFIG_FILE)
}

/// 读 provider 配置。
///
/// **文件不存在不是错误**——那是"还没人配过"的正常状态，返回空列表。
/// 文件存在但格式错**是错误**，且原文带回来：
/// 用户改坏了配置却只看到"暂无数据"，那是骗人。
pub fn load_registry(data_dir: &Path) -> Result<(PathBuf, Vec<ProviderConfig>), String> {
    let path = provider_config_path(data_dir);
    if !path.is_file() {
        return Ok((path, Vec::new()));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
    let parsed: ProviderRegistryFile = serde_json::from_str(&text)
        .map_err(|e| format!("{} 不是合法的 provider 配置: {e}", path.display()))?;
    Ok((path, parsed.providers))
}

/// 把 `protocol` 归一化。空值按 `openai`（本地模型的通用形态）。
///
/// 只认两种：`openai` 与 `anthropic`。别的值一律回落 `openai` 而不是报错——
/// provider 配置是手写的，一个拼错的 protocol 不该让整个侧栏打不开；
/// 但界面要能看到实际生效的是什么（视图里回的就是归一化后的值）。
pub fn normalize_protocol(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "anthropic" | "claude" => "anthropic".to_string(),
        _ => "openai".to_string(),
    }
}

/// 把 API key 掩码成可回显的形态。
///
/// 规则刻意保守：
/// - 未配置 / 空白 → 空串（界面据此显示「未配置」，尤其本地模型不需要 key）
/// - 长度 < 12 → 整体打码，不暴露长度之外的任何信息
/// - 否则保留末四位，便于用户核对「是不是我填的那把」
///
/// **前四位不显示**：有些厂商的 key 前缀固定（`sk-`），露前缀没有信息量，
/// 反而缩短了真正需要保密的中间段的相对长度。
pub fn mask_key(key: Option<&str>) -> String {
    let Some(k) = key else { return String::new() };
    let k = k.trim();
    if k.is_empty() {
        return String::new();
    }
    let n = k.chars().count();
    if n < 12 {
        return "*".repeat(n.max(1));
    }
    let tail: String = k.chars().skip(n - 4).collect();
    format!("****{tail}")
}

/// 写入（或按 id 覆盖）一条 provider 配置。
///
/// # 几个刻意的取舍
///
/// - **保留文件里其它字段**：只在 `providers` 数组里按 id 替换/追加，
///   不重写整个文件为「只有本节知道的形状」。用户手写过的额外键不会丢。
/// - **原子写**：先写 `.tmp` 再 `rename`。直接截断原文件再写，一旦中途
///   失败（磁盘满 / 进程被杀），用户就丢失了全部 provider 配置。
/// - **id 为空直接拒绝**：空 id 会让下一次读取产生一个无法定位的条目。
/// - `api_key` 传 `None` 表示「**不改动现有 key**」，传空串表示「清空」。
///   这个区别很重要：界面上的编辑表单默认不回填明文（它拿不到明文），
///   若把 `None` 当成「清空」，用户改个模型名就会把 key 抹掉。
pub fn save_provider(data_dir: &Path, incoming: ProviderConfig) -> Result<PathBuf, String> {
    let _guard=config_update_lock().lock().map_err(|_|"模型配置写入锁不可用")?;
    if incoming.id.trim().is_empty() {
        return Err("provider id 不能为空".to_string());
    }
    let path = provider_config_path(data_dir);
    let mut file = if path.is_file() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
        serde_json::from_str::<ProviderRegistryFile>(&text)
            .map_err(|e| format!("{} 不是合法的 provider 配置: {e}", path.display()))?
    } else {
        ProviderRegistryFile::default()
    };

    // 传 None 时沿用旧 key；传 Some("") 时清空。
    let merged_key = match &incoming.api_key {
        Some(k) if k.trim().is_empty() => None,
        Some(k) => Some(k.clone()),
        None => file
            .providers
            .iter()
            .find(|p| p.id == incoming.id)
            .and_then(|p| p.api_key.clone()),
    };

    let mut next = incoming;
    next.api_key = merged_key;
    next.protocol = normalize_protocol(&next.protocol);

    match file.providers.iter_mut().find(|p| p.id == next.id) {
        Some(slot) => *slot = next,
        None => file.providers.push(next),
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败: {e}"))?;
    }
    let mut root=if path.is_file(){serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?}else{serde_json::json!({})};
    root["providers"]=serde_json::to_value(&file.providers).map_err(|e|e.to_string())?;
    atomic_config_write(&path,&root)?;
    Ok(path)
}

pub(crate) fn config_update_lock()->&'static std::sync::Mutex<()> {
    static LOCK:std::sync::OnceLock<std::sync::Mutex<()>>=std::sync::OnceLock::new();
    LOCK.get_or_init(||std::sync::Mutex::new(()))
}
pub(crate) fn atomic_config_write(path:&Path,value:&serde_json::Value)->Result<(),String>{
    use std::io::Write;
    let tmp=path.with_extension(format!("json.{}.tmp",std::process::id()));
    let text=serde_json::to_vec_pretty(value).map_err(|e|e.to_string())?;
    let mut file=std::fs::File::create(&tmp).map_err(|e|e.to_string())?;
    file.write_all(&text).and_then(|_|file.sync_all()).map_err(|e|e.to_string())?;
    drop(file);
    std::fs::rename(&tmp,path).map_err(|e|format!("替换配置失败，原配置已保留: {e}"))
}

/// 把配置渲染成给 UI 的画像。**每一档状态都自带说明**，
/// 不让前端自己猜"not_implemented 到底能不能用"。
pub fn views(configs: &[ProviderConfig]) -> Vec<ProviderView> {
    if configs.is_empty() {
        return Vec::new();
    }
    configs
        .iter()
        .map(|c| {
            let status = if !c.enabled {
                ProviderStatus::Disabled
            } else {
                ProviderStatus::NotImplemented {
                    reason: format!(
                        "provider {} 已在 {} 配置，但本轮只交付接入位，未实现厂商调用",
                        c.id,
                        PROVIDER_CONFIG_FILE
                    ),
                }
            };
            let has_key = c
                .api_key
                .as_deref()
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false);
            ProviderView {
                id: c.id.clone(),
                name: c.name.clone(),
                kind: c.kind.clone(),
                endpoint: c.endpoint.clone(),
                model: c.model.clone(),
                enabled: c.enabled,
                status,
                protocol: normalize_protocol(&c.protocol),
                // 明文 key 到此为止：视图层只带掩码出去。
                masked_key: mask_key(c.api_key.as_deref()),
                has_key,
            }
        })
        .collect()
}

/// 已实现真实调用的 provider 数量。**本轮恒为 0**。
///
/// 只数 [`ProviderStatus::Ready`]：`Disabled` 与 `NotImplemented` 都不算，
/// 把"配了但没实现"数成"已实现"会让侧栏上的"未接入"提示消失。
pub fn implemented_count(views: &[ProviderView]) -> u32 {
    views
        .iter()
        .filter(|v| matches!(v.status, ProviderStatus::Ready { .. }))
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let mut p = std::env::current_exe().expect("current_exe");
        p.pop();
        p.pop();
        p.join(format!("ac-llm-{tag}"))
    }

    #[test]
    fn missing_config_file_is_not_an_error() {
        let d = scratch("missing");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (path, cfgs) = load_registry(&d).unwrap();
        assert!(cfgs.is_empty(), "没配过就该是空列表，不能编一条出来");
        assert_eq!(path, provider_config_path(&d));
        assert!(views(&cfgs).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn configured_provider_reports_not_implemented() {
        let cfgs = vec![ProviderConfig {
            id: "ollama".into(),
            name: "本地 Ollama".into(),
            kind: "local".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            model: "qwen".into(),
            enabled: true,
            // 本地 Ollama 走 OpenAI 兼容端点，且通常不需要 key。
            protocol: String::new(),
            api_key: None,
        }];
        let v = views(&cfgs);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].status.as_str(), "not_implemented");
        // 变异敏感：把 implemented_count 改成返回 views.len() 会红。
        assert_eq!(implemented_count(&v), 0);
    }

    #[test]
    fn disabled_provider_is_disabled_not_not_implemented() {
        let cfgs = vec![ProviderConfig {
            id: "x".into(),
            name: "X".into(),
            kind: "cloud".into(),
            endpoint: String::new(),
            model: String::new(),
            enabled: false,
            protocol: String::new(),
            api_key: None,
        }];
        let v = views(&cfgs);
        assert_eq!(v[0].status.as_str(), "disabled");
        assert_eq!(implemented_count(&v), 0);
    }

    #[test]
    fn broken_config_reports_original_error() {
        let d = scratch("broken");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(provider_config_path(&d), b"{ not json").unwrap();
        let err = load_registry(&d).unwrap_err();
        // 必须是"配置坏了"，不能退化成"暂无数据"。
        assert!(err.contains("不是合法的 provider 配置"), "实际: {err}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
