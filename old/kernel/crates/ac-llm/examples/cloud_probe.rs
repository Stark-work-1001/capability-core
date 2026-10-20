//! 云端三模型的**真实调用**探针。
//!
//! 用途：验证 embedding / rerank / chat 三条路径对着真实服务能跑通。
//! 它**不参与产品构建**（examples 不进 lib），只在交付前手工跑一次。
//!
//! # 密钥来源
//!
//! 优先环境变量 `CORE_PROBE_KEY`，其次读 `--key` 参数。
//! **不允许硬编码**：这个文件会进仓库，硬编码等于把密钥提交上去。
//!
//! ```text
//! cargo run --release -p ac-llm --example cloud_probe -- --key sk-xxx
//! ```

use ac_llm::cloud::{chat, embed, rerank};

const EP: &str = "https://api.siliconflow.cn/v1";

fn key_from_args() -> String {
    let args: Vec<String> = std::env::args().collect();
    for (i, a) in args.iter().enumerate() {
        if a == "--key" {
            if let Some(v) = args.get(i + 1) {
                return v.clone();
            }
        }
        if let Some(v) = a.strip_prefix("--key=") {
            return v.to_string();
        }
    }
    std::env::var("CORE_PROBE_KEY").unwrap_or_default()
}

fn main() {
    let key = key_from_args();
    if key.trim().is_empty() {
        eprintln!("缺少密钥：请用 --key sk-xxx 或环境变量 CORE_PROBE_KEY 提供");
        std::process::exit(2);
    }

    let mut failures = 0;

    // ── 1. Embedding ────────────────────────────────────────────
    print!("[1/3] Embedding  Qwen/Qwen3-VL-Embedding-8B ... ");
    let t0 = std::time::Instant::now();
    match embed(
        EP,
        &key,
        "Qwen/Qwen3-VL-Embedding-8B",
        &[
            "能力中枢的向量检索".to_string(),
            "agent assets capability index".to_string(),
        ],
        30_000,
    ) {
        Ok(v) => {
            let dim = v.first().map(|x| x.len()).unwrap_or(0);
            let norm: f32 = v
                .first()
                .map(|x| x.iter().map(|f| f * f).sum::<f32>().sqrt())
                .unwrap_or(0.0);
            println!(
                "OK  {}ms  {} 条 × {} 维  首条范数={:.4}",
                t0.elapsed().as_millis(),
                v.len(),
                dim,
                norm
            );
            if v.len() != 2 || dim == 0 {
                println!("   !! 条数或维度不合法");
                failures += 1;
            }
        }
        Err(e) => {
            println!("FAIL  {}", e);
            failures += 1;
        }
    }

    // ── 2. Rerank ───────────────────────────────────────────────
    print!("[2/3] Rerank     Qwen/Qwen3-VL-Reranker-8B ... ");
    let t0 = std::time::Instant::now();
    match rerank(
        EP,
        &key,
        "Qwen/Qwen3-VL-Reranker-8B",
        "能力检索",
        &[
            "向量检索能力".to_string(),
            "做饭的方法".to_string(),
            "能力中枢索引".to_string(),
        ],
        3,
        30_000,
    ) {
        Ok(hits) => {
            println!("OK  {}ms  {} 条", t0.elapsed().as_millis(), hits.len());
            for h in &hits {
                println!("     #{} score={:.6}", h.index, h.score);
            }
            // 相关性排序必须把"做饭的方法"排在最后 —— 这是语义烟测
            if let Some(last) = hits.last() {
                if last.index != 1 {
                    println!("   !! 语义排序可疑：最不相关的那条应该是 index=1（做饭的方法）");
                    failures += 1;
                }
            }
        }
        Err(e) => {
            println!("FAIL  {}", e);
            failures += 1;
        }
    }

    // ── 3. Chat / Judge ─────────────────────────────────────────
    print!("[3/3] Chat       Qwen/Qwen3.8-27B ... ");
    let t0 = std::time::Instant::now();
    match chat(
        EP,
        &key,
        "Qwen/Qwen3.8-27B",
        &[(
            "user".to_string(),
            "用一个词回答：1+1 等于几？".to_string(),
        )],
        64,
        60_000,
    ) {
        Ok(text) => {
            println!("OK  {}ms  回复={:?}", t0.elapsed().as_millis(), text.trim());
        }
        Err(e) => {
            println!("FAIL  {}", e);
            failures += 1;
        }
    }

    println!();
    if failures == 0 {
        println!("全部通过：3/3");
    } else {
        println!("失败 {failures} 项");
        std::process::exit(1);
    }
}
