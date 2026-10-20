//! 云端模型调用（OpenAI 兼容协议）。
//!
//! # 为什么单独一个文件
//!
//! `provider.rs` 只做「配置的形状与读写」，**不含任何网络调用**——
//! 那是刻意的：配置只要能读能写，界面就不会因为模型服务挂了而打不开。
//! 真实调用在这个文件里，且只有三种：embedding / rerank / chat。
//!
//! # 为什么三家都用同一个路径
//!
//! 硅基流动、阿里百炼、智谱、DeepSeek、vLLM、Ollama 的 `/v1` 端点
//! 都实现 OpenAI 兼容协议。所以这里**按协议分支，不按厂商分支**：
//! 加一个新厂商只需要改配置，不需要改代码。
//!
//! # 密钥纪律
//!
//! 明文 key 只在内存与配置文件中。**这个文件的任何日志与错误信息里
//! 都不许出现它** —— 错误会被写进界面、写进日志、写进用户截图。
//!
//! # 超时
//!
//! 用 ureq 的 `timeout_connect` + 每次请求的 `timeout_overall`。
//! 两者都要：只给 connect 超时挡不住「连上但永远不回包」，
//! 只给 overall 超时挡不住 DNS/connect 卡住把预算耗光。

use serde::{Deserialize, Serialize};

/// 一次调用的结果摘要（给界面「测试连接」按钮用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    /// 是否成功。
    pub ok: bool,
    /// 耗时（毫秒）。
    pub elapsed_ms: u64,
    /// 成功时的摘要（如「4096 维」「3 条重排」「12 token」）。
    pub summary: String,
    /// 失败时的原因（不含密钥）。
    pub error: String,
}

/// 重排结果的一条。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankHit {
    /// 文档在输入数组里的下标。
    pub index: usize,
    /// 相关性分数。**只在本批内可比**，跨查询不可比。
    pub score: f32,
}

/// 构造一个 ureq agent。**每次调用都新建**：云端可能换端点/换超时，
/// 复用 agent 会让「改了配置没生效」变成一个需要重启才能发现的坑。
fn agent(timeout_ms: u64) -> ureq::Agent {
    // ureq 3.x 的超时是**分阶段**的：global 是总预算，connect 单独钳一个更小的值。
    //
    // 为什么两个都要：
    // - 只给 global：一个能连上但 DNS 很慢的目标会把预算全花在建连上，
    //   后面的读就没时间了 —— 但那其实是"没连上"，错误信息会张冠李戴。
    // - 只给 connect：连上后对方不吐字节，请求会永久挂住。
    let global = std::time::Duration::from_millis(timeout_ms);
    let connect = std::time::Duration::from_millis(timeout_ms.min(10_000));
    ureq::Agent::config_builder()
        .timeout_global(Some(global))
        .timeout_connect(Some(connect))
        .build()
        .into()
}

/// 把密钥从任何字符串里抹掉。
///
/// **每条错误路径都要过一遍这个函数**。看似多余，但一次 `format!("{e}")`
/// 就足以把 key 带进用户截图——ureq 的错误可能包含完整 URL，而某些
/// 服务商把 key 放 query 里（我们没有，但不能假设上游不会）。
pub fn redact(text: &str, api_key: &str) -> String {
    if api_key.is_empty() {
        return text.to_string();
    }
    text.replace(api_key, "****")
}

/// 调 embedding。返回每条文本的向量。
pub fn embed(
    endpoint: &str,
    api_key: &str,
    model: &str,
    texts: &[String],
    timeout_ms: u64,
) -> Result<Vec<Vec<f32>>, String> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!("{}/embeddings", endpoint.trim_end_matches('/'));
    let body = serde_json::json!({ "model": model, "input": texts }).to_string();

    let resp = agent(timeout_ms)
        .post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(body.as_str())
        .map_err(|e| redact(&format!("请求 {url} 失败：{e}"), api_key))?;

    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| redact(&format!("读取响应失败：{e}"), api_key))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("响应不是 JSON：{e}（前 200 字：{}）", redact(&snippet(&text),api_key)))?;

    // OpenAI 兼容错误体：{error:{message,type,code}}
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        return Err(redact(&format!("服务端报错：{msg}"), api_key));
    }

    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| format!("响应缺少 data 数组（前 200 字：{}）", redact(&snippet(&text),api_key)))?;
    if data.len() != texts.len() {
        return Err(format!(
            "返回条数不符：请求 {} 条，返回 {} 条",
            texts.len(),
            data.len()
        ));
    }

    let mut out: Vec<Vec<f32>> = vec![Vec::new(); data.len()];
    let mut seen_index=std::collections::BTreeSet::new();
    let mut seen_dim: Option<usize> = None;
    for (i, item) in data.iter().enumerate() {
        let arr = item
            .get("embedding")
            .and_then(|e| e.as_array())
            .ok_or_else(|| format!("第 {i} 条缺少 embedding 数组"))?;
        if arr.is_empty() {
            return Err(format!("第 {i} 条向量为空"));
        }
        match seen_dim {
            None => seen_dim = Some(arr.len()),
            Some(d) if d != arr.len() => {
                return Err(format!("第 {i} 条维度 {} 与前面的 {d} 不一致", arr.len()))
            }
            Some(_) => {}
        }
        let mut vec: Vec<f32> = Vec::with_capacity(arr.len());
        for x in arr {
            let f = x
                .as_f64()
                .ok_or_else(|| format!("第 {i} 条含非数值元素"))? as f32;
            if !f.is_finite() {
                return Err(format!("第 {i} 条含 NaN/Inf"));
            }
            vec.push(f);
        }
        let index=item.get("index").and_then(|v|v.as_u64()).unwrap_or(i as u64) as usize;
        if index>=out.len() || !seen_index.insert(index){return Err("向量响应索引越界或重复".into());}
        out[index]=vec;
    }
    Ok(out)
}

/// 调 rerank。返回按分数降序的命中。
pub fn rerank(
    endpoint: &str,
    api_key: &str,
    model: &str,
    query: &str,
    documents: &[String],
    top_n: usize,
    timeout_ms: u64,
) -> Result<Vec<RerankHit>, String> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!("{}/rerank", endpoint.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "query": query,
        "documents": documents,
        "top_n": top_n.min(documents.len()),
    })
    .to_string();

    let resp = agent(timeout_ms)
        .post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(body.as_str())
        .map_err(|e| redact(&format!("请求 {url} 失败：{e}"), api_key))?;

    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| redact(&format!("读取响应失败：{e}"), api_key))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("响应不是 JSON：{e}（前 200 字：{}）", redact(&snippet(&text),api_key)))?;

    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        return Err(redact(&format!("服务端报错：{msg}"), api_key));
    }

    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| format!("响应缺少 results 数组（前 200 字：{}）", redact(&snippet(&text),api_key)))?;

    let mut out: Vec<RerankHit> = Vec::with_capacity(results.len());
    for item in results {
        let index = item
            .get("index")
            .and_then(|i| i.as_u64())
            .ok_or("results 条目缺少 index")? as usize;
        if index >= documents.len() {
            return Err(format!("index={index} 越界（文档共 {} 条）", documents.len()));
        }
        let score = item
            .get("relevance_score")
            .and_then(|s| s.as_f64())
            .ok_or("results 条目缺少 relevance_score")? as f32;
        if !score.is_finite() {
            return Err("relevance_score 含 NaN/Inf".into());
        }
        out.push(RerankHit { index, score });
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(out)
}

/// 调 chat（Judge / 通用模型）。
pub fn chat(
    endpoint: &str,
    api_key: &str,
    model: &str,
    messages: &[(String, String)],
    max_tokens: u32,
    timeout_ms: u64,
) -> Result<String, String> {
    let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));
    let msgs: Vec<serde_json::Value> = messages
        .iter()
        .map(|(role, content)| serde_json::json!({ "role": role, "content": content }))
        .collect();
    let body = serde_json::json!({
        "model": model,
        "messages": msgs,
        "max_tokens": max_tokens,
    })
    .to_string();

    let resp = agent(timeout_ms)
        .post(&url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(body.as_str())
        .map_err(|e| redact(&format!("请求 {url} 失败：{e}"), api_key))?;

    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| redact(&format!("读取响应失败：{e}"), api_key))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("响应不是 JSON：{e}（前 200 字：{}）", redact(&snippet(&text),api_key)))?;

    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        return Err(redact(&format!("服务端报错：{msg}"), api_key));
    }

    v.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("响应里找不到 choices[0].message.content（前 200 字：{}）", snippet(&text)))
}

/// 取前 N 个字符做错误上下文。**按字符而不是字节切**——
/// 中文响应按字节切会切出半个字，进而让整个错误信息变成乱码。
fn snippet(s: &str) -> String {
    s.chars().take(200).collect()
}
