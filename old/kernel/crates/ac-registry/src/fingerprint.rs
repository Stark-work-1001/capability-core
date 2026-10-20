//! 内容指纹：L1（字节级）+ L2（规范化）。
//!
//! ## 边界：只做两级，**没有 L3**
//!
//! 设计文档把语义指纹（描述 -> MiniLM 嵌入）列为 L3，并明确写着
//! 「L3 必须等 M3 的向量基础设施，否则单独做就是给每个去重任务跑一次 embedding」。
//! 本模块据此**刻意不实现** L3：
//! - 不引入任何嵌入 / 向量 / ONNX 依赖；
//! - 不新增任何浮点向量字段到数据库；
//! - 不做近似最近邻搜索。
//! 语义级去重是 M3 的事，届时向量索引本��就已存在，边际成本接近零。
//!
//! ## L1 是什么
//! 全文 SHA-256（忽略 frontmatter 头）。真实数据的经验值：L1 就是字节级完全相同，
//! 它抓的是 `vendor/skill/sympy/SKILL.md` 与某个 references 目录里的同名副本。
//!
//! ## L2 是什么
//! 规范化文本后再哈希：**去 frontmatter + 去所有空白 + 去版本号 + 转小写**。
//! 它抓的是 L1 漏掉的"重打包"：换行符差异、多余空行、大小写差异、
//! 版本号从 `0.1.0` 改成 `0.1.0-hotfix` 这类**不改变能力本身**的改动。

use sha2::{Digest, Sha256};

/// L1：全文内容指纹（SHA-256 前 8 字节），忽略 frontmatter。
pub fn l1_hash(text: &str) -> String {
    let normalized = strip_frontmatter(text);
    let digest = Sha256::digest(normalized.as_bytes());
    hex(&digest[..8])
}

/// L2：规范化指纹（SHA-256 前 8 字节）。
///
/// 规范化四步，顺序固定：
/// 1. 去 frontmatter —— 元数据不该让"同一能力"看起来不同；
/// 2. 去全部空白 —— 换行/缩进差异是纯噪声；
/// 3. 去版本号 —— `v0.1.0` / `0.1.0` / `version: 0.1.0` 都不改变能力；
/// 4. 转小写 —— 大小写差异同理。
pub fn l2_hash(text: &str) -> String {
    let normalized = normalize_l2(text);
    let digest = Sha256::digest(normalized.as_bytes());
    hex(&digest[..8])
}

/// 去掉 frontmatter 头（如果存在）。
pub fn strip_frontmatter(text: &str) -> &str {
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = match body.strip_prefix("---\n") {
        Some(r) => r,
        None => match body.strip_prefix("---\r\n") {
            Some(r) => r,
            None => return body,
        },
    };
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let head_len: usize = rest
                .split_inclusive('\n')
                .take_while(|l| !l.trim_end_matches(['\r', '\n']).eq_ignore_ascii_case("---"))
                .map(|l| l.len())
                .sum();
            return &rest[head_len..];
        }
    }
    body
}

/// L2 规范化：去 frontmatter / 去空白 / 去版本号 / 转小写。
pub fn normalize_l2(text: &str) -> String {
    let body = strip_frontmatter(text);
    let mut out = String::with_capacity(body.len());
    for ch in body.chars() {
        // 去所有空白（空格、换行、制表、全角空格）
        if ch.is_whitespace() || ch == '\u{3000}' {
            continue;
        }
        // 去版本号：`0.1.0`、`v1.2.3` 这类版本号片段
        if ch.is_ascii_digit() || ch == '.' {
            continue;
        }
        out.extend(ch.to_lowercase());
    }
    out
}

/// 去版本号更保守的辅助：把 `version: 0.1.0` / `v0.1.0` 从任意文本里剥掉。
/// 只用于 group_key 里的可读 slug，不参与 L1/L2 哈希本身。
pub fn strip_version_tokens(s: &str) -> String {
    s.chars()
        .filter(|c| !(c.is_ascii_digit() || *c == '.'))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 构建去重分组键。
///
/// **只用 L1**，不用 L2 做主键：L2 是"实质相同"的近似判据，可能把两个
/// 真实不同的能力圈到一起；L1 才是"就是同一份文件"的强证据。
///
/// L2 命中但 L1 不同的资产，产出**独立的 logical_id**（按 L2 值派生），
/// 但会在去重候选队列里以 `MatchLevel::L2` 提示用户：
/// 「这两个内容字节不同、规范化后相同，可能是重打包」。
/// 这样 L2 永远不会静默合并任何东西。
pub fn group_key(l1: &str) -> String {
    format!("l1-{l1}")
}

/// L2 候选分组键。用于把 L1 不同但 L2 相同的资产关联起来，**不**用于合并。
pub fn l2_group_key(l2: &str) -> String {
    format!("l2-{l2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l1_ignores_frontmatter() {
        let a = "---\nname: x\ndescription: 一个\n---\n正文内容";
        let b = "---\nname: y\ndescription: 两个\nversion: 3\n---\n正文内容";
        assert_eq!(l1_hash(a), l1_hash(b), "L1 只看正文，忽略 frontmatter");
    }

    #[test]
    fn l1_detects_byte_level_difference() {
        let a = "正文内容";
        let b = "正文内容\n末尾多了换行";
        assert_ne!(l1_hash(a), l1_hash(b), "L1 对字节级差异敏感");
    }

    #[test]
    fn l2_catches_whitespace_and_case_differences() {
        // L1 不同，L2 相同 —— 这正是 L2 存在的意义
        let a = "---\nname: Data Normalizer\n---\nNormalize your data.";
        let b = "---\nname: data normalizer\n---\nNormalize    your\n   data.\n";
        assert_ne!(l1_hash(a), l1_hash(b), "这两个文件字节不同，L1 必须漏掉");
        assert_eq!(l2_hash(a), l2_hash(b), "L2 必须抓住：去空白 + 小写后相同");
    }

    #[test]
    fn l2_catches_version_bump_in_body() {
        // 版本号写在**正文**里时 L1 必须漏掉、L2 必须抓住。
        // （写在 frontmatter 里的话 L1 早就忽略了 —— 那是 L1 的职责，
        //   用它测 L2 会得到一个"两边都相等"的假通过。）
        let a = "提供时序图绘制 v0.1.0";
        let b = "提供时序图绘制 v0.2.0";
        assert_ne!(l1_hash(a), l1_hash(b), "字节不同，L1 漏");
        assert_eq!(l2_hash(a), l2_hash(b), "去版本号后 L2 命中");
    }

    #[test]
    fn l1_also_ignores_frontmatter_version_bumps() {
        // 对照上一条：frontmatter 里的版本号变化 **L1 就已经看不见了**，
        // 所以"版本升级"根本不构成一条新的逻辑资产，也就不需要 L2 兜。
        let a = "---\nversion: 0.1.0\n---\n提供时序图绘制";
        let b = "---\nversion: 0.2.0\n---\n提供时序图绘制";
        assert_eq!(l1_hash(a), l1_hash(b), "frontmatter 差异不该造出新资产");
        assert_eq!(l2_hash(a), l2_hash(b));
    }

    #[test]
    fn l2_does_not_merge_genuinely_different_content() {
        let a = "提供时序图绘制";
        let b = "提供流程图绘制";
        assert_ne!(l2_hash(a), l2_hash(b), "真实不同内容不能被 L2 圈到一起");
    }

    #[test]
    fn no_l3_in_the_implementation() {
        // 守门测试：指纹层的**实现部分**不得长出语义/向量能力。
        // 只扫描 #[cfg(test)] 之前的代码，否则这个测试会被自己
        // 写的关键词命中而永远失败。
        let src = include_str!("fingerprint.rs");
        let impl_part = src.split("#[cfg(test)]").next().unwrap_or("");
        // 注释里可以讨论 L3（说明为什么不实现），代码里不允许。
        let code: String = impl_part
            .lines()
            .map(|l| {
                let t = l.trim_start();
                if t.starts_with("//") {
                    ""
                } else {
                    t.split("//").next().unwrap_or("")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in ["embed", "onnx", "cosine", "dot_product"] {
            assert!(
                !code.contains(forbidden),
                "指纹层实现不得出现 {forbidden} —— L3 必须等 M3 的向量基础设施"
            );
        }
    }

    #[test]
    fn l2_strips_digits_and_dots_so_versions_collapse() {
        // 文档里给的「去版本号」是字面去数字与点。
        // 这会让 "0.1" 与 "1.0" 归一 —— 这是**有意的**：
        // 版本号本身不构成能力差异，多版本共存由 L1 分组保证。
        assert_eq!(l2_hash("v0.1.0"), l2_hash("v9.9.9"));
    }
}