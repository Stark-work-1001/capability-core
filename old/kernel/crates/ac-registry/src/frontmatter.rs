//! YAML frontmatter 最小解析器。
//!
//! 只覆盖 `SKILL.md` 实际会出现的语法子集，**刻意不引入 serde_yaml**：
//! 该 crate 已停止维护，会在 `clippy -D warnings` 门禁下产生噪音，
//! 而 SKILL.md 的 frontmatter 语法是受约束的，写死解析器反而更可控。
//!
//! 支持：
//! - `key: value`
//! - `key: [a, b, c]`（行内数组）
//! - `key:\n  - a\n  - b`（块数组）
//! - `key: |` / `key: >`（**块标量**，含 `-` / `+` chomping 与缩进）
//! - `# comment` 与空行
//! - 引号包裹的标量（`'...'` / `"..."`）
//!
//! 不支持（遇到时退回字符串，不报错）：
//! - 锚点、复杂流式语法、多行流式集合、嵌套数组
//!
//! # 缩进是唯一的结构判据
//!
//! 早先的实现「见到 `key:` 就当顶层键」，真实资产上塌了两次（见
//! [parse] 的文档）：`>` 被当成 description 的字面量，`chain.schema.name`
//! 覆盖掉顶层 `name`。两条根因是同一个：**不看缩进就没有结构**。

use std::collections::BTreeMap;

/// frontmatter 的解析结果。
///
/// 有意使用 `BTreeMap` 而非 `HashMap`：键序稳定，便于生成确定性的 content_hash。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub fields: BTreeMap<String, YamlValue>,
    /// **明确点名**的嵌套一级键：`section -> key -> value`。
    ///
    /// # 为什么这个表存在，而顶层 fields 仍然只收顶层键
    ///
    /// 本解析器最硬的一条教训是「嵌套键绝不能提升成顶层键」——
    /// 真实资产里的 `chain.schema.name: string` 曾把 6 条资产的展示名
    /// 变成字面量 "string"。所以顶层 fields 的纪律一个字都不改。
    ///
    /// 但有些字段**官方规范就放在嵌套里**：Agent Skills 上游把作者写成
    ///
    /// ```text
    /// metadata:
    ///   author: MathWorks
    ///   version: "1.1"
    /// ```
    ///
    /// 实测本机 460 份 SKILL.md 里有 8 份是这种写法（另有 1 份写成顶层
    /// `author:`）。一律读不到的话，这 8 份的作者会永远退化成包名——
    /// 而作者自己**确实写了**。所以这里提供一个**只能按 (section, key)
    /// 精确取**的旁路：调用方必须写出完整路径，不可能"顺手"拿到一个
    /// 顶层键，也就不可能重演那次展示名事故。
    pub nested: BTreeMap<String, BTreeMap<String, YamlValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamlValue {
    Str(String),
    Bool(bool),
    /// 行内数组或块数组
    List(Vec<String>),
}

impl Frontmatter {
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.fields.get(key) {
            Some(YamlValue::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// 按 **(section, key)** 精确读一级嵌套值。
    ///
    /// 刻意要求两个参数：只给 key 的 API 会让调用方把嵌套值当顶层值用，
    /// 那正是本文件反复防的那种错误。
    pub fn get_nested_str(&self, section: &str, key: &str) -> Option<&str> {
        match self.nested.get(section).and_then(|m| m.get(key)) {
            Some(YamlValue::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.fields.get(key) {
            Some(YamlValue::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    /// 取列表；标量按单元素列表处理（`allowed-tools: Bash Read` 与 `allowed-tools: [Bash, Read]` 等价）
    pub fn get_list(&self, key: &str) -> Vec<String> {
        match self.fields.get(key) {
            Some(YamlValue::List(v)) => v.clone(),
            Some(YamlValue::Str(s)) => {
                if s.is_empty() {
                    Vec::new()
                } else {
                    s.split_whitespace().map(|x| x.to_string()).collect()
                }
            }
            _ => Vec::new(),
        }
    }
}

/// 从一段文本中提取 frontmatter 与正文。
///
/// 若文档不以 `---` 开头则返回 `(None, 原文)`。
pub fn split_document(text: &str) -> (Option<&str>, &str) {
    // 允许 BOM
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = match body.strip_prefix("---\n") {
        Some(r) => r,
        None => match body.strip_prefix("---\r\n") {
            Some(r) => r,
            None => return (None, body),
        },
    };
    // 结束分隔符：单独一行的 ---
    for (idx, line) in rest.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let head_len: usize = rest
                .split_inclusive('\n')
                .take(idx)
                .map(|l| l.len())
                .sum();
            let head = &rest[..head_len];
            let tail = &rest[head_len + line.len()..];
            return (Some(head), tail);
        }
    }
    // 没有结束分隔符：视为无 frontmatter，避免吞掉整篇文档
    (None, body)
}

/// 解析 frontmatter 文本。
///
/// # 判据是缩进（本文件最大的一条教训）
///
/// 真实 SKILL.md 里两种写法都无法回避：
///
/// 1. **块标量**。description 常写成 "description: >" 换行后缩进两格写正文。
///    早先只认单行 key: value，于是把 ">" 本身当成了值 —— 库里 50 条
///    description 变成 ">" / ">-" / "|" 字面量。
/// 2. **嵌套映射**。chain: 下面有 schema:，再下面有 "name: string"。
///    不看缩进的话，这条**嵌套**的 name 会覆盖掉顶层真正的 name，
///    6 条资产的展示名因此成了字面量 "string"。
///
/// 所以：与首条键同缩进的才是顶层键；更深缩进一律当作上一个键的续行或
/// 嵌套内容，**绝不写入顶层字段**。而且同一个顶层键重复出现时只保留第一次
/// （见 insert_once）—— 重复键在 YAML 里本身就是坏数据，先到的才是作者意图。
pub fn parse(text: &str) -> Frontmatter {
    let lines: Vec<&str> = text.lines().collect();

    // 顶层缩进 = 第一条键行的缩进。SKILL.md 正常是 0，但整份被缩进过的
    // 文件也真实存在；写死 0 会让它一条键都读不出来。
    let base_indent = lines
        .iter()
        .map(|l| l.trim_end())
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .find_map(|l| {
            let body = l.trim_start();
            if !body.starts_with("- ") && body.contains(':') {
                Some(l.len() - body.len())
            } else {
                None
            }
        })
        .unwrap_or(0);

    let mut fm = Frontmatter::default();
    // 已读到键、还没读到值的顶层键（后面可能跟块数组）
    let mut pending_key: Option<String> = None;
    let mut list_buf: Vec<String> = Vec::new();
    // 该块数组第一项的缩进：更深的是数组项的嵌套内容，不能当同级项收进来
    let mut item_indent: Option<usize> = None;
    // 当前正在读哪个顶层 section 的一级嵌套键（见 Frontmatter::nested）。
    // **只在上一个顶层键是"有标量值"时才置位**：`key:`（会跟块数组或块标量）
    // 与 `key: |` 都不算 section，否则会把块标量正文当成嵌套键。
    let mut nesting_section: Option<String> = None;
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i].trim_end();
        i += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len() - line.trim_start().len();
        let body = &line[indent..];

        // 块数组项
        if let Some(item) = body.strip_prefix("- ") {
            if pending_key.is_some() {
                let at_item_level = match item_indent {
                    None => {
                        item_indent = Some(indent);
                        true
                    }
                    Some(want) => indent == want,
                };
                if at_item_level {
                    list_buf.push(unquote(strip_trailing_comment(item.trim())));
                }
            }
            continue;
        }

        // 非顶层缩进：续行或嵌套映射。既不能写进顶层字段，也意味着
        // 上一个顶层键的块数组到此结束。
        if indent != base_indent {
            // section 的两种来法：
            //   ① 上一个顶层键是 `key: value` → 它就是 section；
            //   ② 上一个顶层键是 `key:` 而后面的缩进行**不是 `- ` 项**
            //      → 它是嵌套映射的 section，不是块数组。
            // 两条都只把值放进 `nested`，**绝不提升成顶层字段**
            // （见 Frontmatter::nested 的注释：那次展示名事故就是这么来的）。
            if nesting_section.is_none() {
                if let Some(prev) = pending_key.take() {
                    // 这个顶层键后面跟的缩进行不是 `- ` 项 → 它是**嵌套映射**
                    // （metadata / chain / …），把它记成 section。
                    // `key:` + `- ` 项那条分支在上面 continue 掉了，
                    // 所以走到这里的一定不是块数组。
                    nesting_section = Some(prev.clone());
                    // 旧行为：块数组为空时落一个空串，保持不变。
                    take_block_array(&mut fm, prev, &mut list_buf);
                    item_indent = None;
                }
            }
            if let Some(section) = nesting_section.clone() {
                // 只收**一级**嵌套（缩进比 section 深，且不超过它 + 4），
                // 且本行必须是 `key: scalar`（不是 `- ` 项、不是块标量头、
                // 不是行内数组）。
                if indent > base_indent
                    && indent <= base_indent + 4
                    && !body.starts_with("- ")
                {
                    if let Some((nk, nv)) = body.split_once(':') {
                        let nk = nk.trim();
                        let nv = strip_trailing_comment(nv.trim());
                        if !nk.is_empty()
                            && !nv.is_empty()
                            && block_indicator(nv).is_none()
                            && !nv.starts_with('[')
                        {
                            fm.nested
                                .entry(section)
                                .or_default()
                                .insert(nk.to_string(), YamlValue::Str(unquote(nv)));
                        }
                    }
                }
            }
            continue;
        }

        let Some((k, v)) = body.split_once(':') else {
            continue;
        };
        let key = k.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let val = strip_trailing_comment(v.trim());

        // 收尾上一个块的列表
        if let Some(prev) = pending_key.take() {
            take_block_array(&mut fm, prev, &mut list_buf);
            item_indent = None;
        }

        if let Some((style, chomp)) = block_indicator(val) {
            // 块标量：吃掉所有比表头更深的行
            nesting_section = None;
            let (scalar, next) = read_block_scalar(&lines, i, indent, style, chomp);
            i = next;
            insert_once(&mut fm, key, YamlValue::Str(scalar));
        } else if val.is_empty() {
            // 可能是块数组开始
            nesting_section = None;
            pending_key = Some(key);
            list_buf.clear();
            item_indent = None;
        } else if let Some(inner) = val.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            // 行内数组：不是 section（它的缩进里不会再有东西）
            nesting_section = None;
            let items: Vec<String> = inner
                .split(',')
                .map(|s| unquote(s.trim()))
                .filter(|s| !s.is_empty())
                .collect();
            insert_once(&mut fm, key, YamlValue::List(items));
        } else {
            let unq = unquote(val);
            let value = if unq == "true" || unq == "yes" {
                YamlValue::Bool(true)
            } else if unq == "false" || unq == "no" {
                YamlValue::Bool(false)
            } else {
                YamlValue::Str(unq)
            };
            // 顶层键 + 标量值 = 可能是嵌套 section（metadata / chain / …）。
            // 只看结构，不看键名：硬编码 "metadata" 会让别家的写法读不到。
            nesting_section = Some(key.clone());
            insert_once(&mut fm, key, value);
        }
    }

    if let Some(prev) = pending_key.take() {
        take_block_array(&mut fm, prev, &mut list_buf);
    }

    fm
}

/// 写入一个字段：**已有键不覆盖**。
///
/// 这是缺陷 B 的第二道闸。第一道是缩进判定（挡住嵌套键），这一道挡重复键：
/// 同一个顶层键出现两次时保留第一次。两个都做，是因为「嵌套键被当顶层键」
/// 和「重复键后写覆盖先写」是两种不同的错法，只堵一种还会从另一种漏回来。
fn insert_once(fm: &mut Frontmatter, key: String, value: YamlValue) {
    fm.fields.entry(key).or_insert(value);
}

/// 收尾一个「键后无值」的块：有项就是块数组，没项就是空串。
fn take_block_array(fm: &mut Frontmatter, key: String, buf: &mut Vec<String>) {
    let value = if buf.is_empty() {
        YamlValue::Str(String::new())
    } else {
        YamlValue::List(std::mem::take(buf))
    };
    insert_once(fm, key, value);
}

/// 块标量的头指示符："|" 或 ">"，可带 chomping（"-" / "+"）与显式缩进数字。
///
/// 返回 (style, chomp)。">" 后面还有别的字符就不是块标量表头，
/// 退回普通标量（YAML 里那是坏数据，但坏数据不该让整篇解析失败）。
fn block_indicator(val: &str) -> Option<(char, char)> {
    let mut chars = val.chars();
    let style = match chars.next()? {
        '|' => '|',
        '>' => '>',
        _ => return None,
    };
    // 显式缩进指示符（|2）不参与判定：本解析器按「块内最小缩进」算，
    // 与显式值在真实文件里从未冲突；但格式必须合法，否则不是表头。
    let mut chomp = ' ';
    for c in chars {
        match c {
            '-' | '+' => chomp = c,
            '0'..='9' => {}
            _ => return None,
        }
    }
    Some((style, chomp))
}

/// 读一个块标量（"key: |" / "key: >"）的内容。
///
/// 结束判据是**缩进**而不是空行：真实的描述里块内就有空行（分段落成组），
/// 把空行当结束会在第一段就截断，剩下的段落又会被当成新的键。
///
/// chomping 语义按 YAML：
/// - "-"（strip）不带尾随换行；
/// - "+"（keep）保留块尾空行；
/// - 缺省（clip）恰好一个尾随换行。
fn read_block_scalar(
    lines: &[&str],
    start: usize,
    header_indent: usize,
    style: char,
    chomp: char,
) -> (String, usize) {
    let mut raw: Vec<&str> = Vec::new();
    let mut min_indent = usize::MAX;
    let mut i = start;
    while i < lines.len() {
        let line = lines[i].trim_end();
        if line.trim().is_empty() {
            // 空行先照收：它可能只是块内的段落间隔，也可能就是块的结尾。
            raw.push(line);
            i += 1;
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent <= header_indent {
            break;
        }
        min_indent = min_indent.min(indent);
        raw.push(line);
        i += 1;
    }

    // 尾部空行先摘掉，chomping 指示符决定要不要补回来
    let mut trailing_blank = 0usize;
    while raw.last().is_some_and(|l| l.trim().is_empty()) {
        raw.pop();
        trailing_blank += 1;
    }
    if raw.is_empty() {
        return (String::new(), i);
    }

    let content_indent = if min_indent == usize::MAX {
        header_indent + 2
    } else {
        min_indent
    };
    let body: Vec<String> = raw
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                l[content_indent.min(l.len())..].trim_end().to_string()
            }
        })
        .collect();

    let mut text = if style == '>' {
        fold_lines(&body)
    } else {
        body.join("\n")
    };
    match chomp {
        '-' => {}
        '+' => {
            for _ in 0..=trailing_blank {
                text.push('\n');
            }
        }
        _ => text.push('\n'),
    }
    (text, i)
}

/// ">" 的折叠语义：相邻非空行用**一个空格**连接，空行保留为换行。
///
/// 已知简化（明写，不藏）：比块基准缩进更深的行本应在折叠时保留换行，
/// 这里一律按空格连接。真实资产里没有依赖该细节的内容；而它是「排版」层面的
/// 差异，不会再把 ">" 本身写进 description。
fn fold_lines(lines: &[String]) -> String {
    let mut out = String::new();
    let mut prev_blank = true;
    for (idx, l) in lines.iter().enumerate() {
        if l.is_empty() {
            out.push('\n');
            prev_blank = true;
            continue;
        }
        if idx > 0 && !prev_blank {
            out.push(' ');
        }
        out.push_str(l);
        prev_blank = false;
    }
    out
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

/// 剥掉行尾注释：`a  # 注释` -> `a`。
///
/// 只有前面是空白（或位于串首）的 `#` 才算注释，且不切开引号内部，
/// 否则 `url: "http://x#y"` 这类值会被截断。
fn strip_trailing_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if b == q {
                    // 双引号内 \" 是转义，闭合引号要跳过它
                    let escaped = q == b'"' && i > 0 && bytes[i - 1] == b'\\';
                    if !escaped {
                        quote = None;
                    }
                }
            }
            None => {
                if b == b'"' || b == b'\'' {
                    quote = Some(b);
                } else if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
                    return s[..i].trim_end();
                }
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"---
name: release-checklist
description: 执行发布前的验证清单
license: MIT
allowed-tools:
  - Bash
  - Read
tags: [ci, release]
disable-model-invocation: false
---
# 正文开始

这里是正文。
"#;

    #[test]
    fn splits_frontmatter_and_body() {
        let (head, body) = split_document(SAMPLE);
        let head = head.expect("应当识别出 frontmatter");
        assert!(body.starts_with("# 正文开始"));
        let fm = parse(head);
        assert_eq!(fm.get_str("name"), Some("release-checklist"));
        assert_eq!(fm.get_str("description"), Some("执行发布前的验证清单"));
        assert_eq!(fm.get_bool("disable-model-invocation"), Some(false));
    }

    #[test]
    fn reads_one_level_nested_scalars_without_promoting_them() {
        // 上游 Agent Skills 规范把作者写在 metadata.author 下。
        // nested 表要能精确读到它。
        let text = "name: x\nmetadata:\n  author: MathWorks\n  version: \"1.1\"\ndescription: d\n";
        let fm = parse(text);
        assert_eq!(fm.get_nested_str("metadata", "author"), Some("MathWorks"));
        assert_eq!(fm.get_nested_str("metadata", "version"), Some("1.1"));
        // **顶层绝不能出现 author**：这正是那次"展示名变 string"事故的形态。
        assert_eq!(fm.get_str("author"), None, "嵌套值不得提升成顶层键");
        assert_eq!(fm.get_str("name"), Some("x"));
        assert_eq!(fm.get_str("description"), Some("d"));
    }

    #[test]
    fn nested_value_does_not_shadow_a_real_top_level_key() {
        // 回归（真实事故）：chain.schema.name 曾经覆盖顶层 name，
        // 6 条资产的展示名变成字面量 "string"。
        let text = "name: 真名\nchain:\n  schema:\n    name: string\n";
        let fm = parse(text);
        assert_eq!(fm.get_str("name"), Some("真名"), "顶层 name 不能被嵌套 name 覆盖");
        // 深层（两级）不进 nested：只收一级。
        assert_eq!(fm.get_nested_str("chain", "schema"), None);
    }

    #[test]
    fn nested_list_items_are_not_collected_as_nested_keys() {
        let text = "allowed-tools:\n  - Bash\n  - Read\n";
        let fm = parse(text);
        assert_eq!(fm.get_list("allowed-tools"), vec!["Bash", "Read"]);
        assert!(fm.nested.is_empty(), "块数组项不是嵌套键");
    }

    #[test]
    fn nested_block_scalar_is_not_collected() {
        // description: > 的正文里常出现 "x: y" 形态，不能被当成嵌套键。
        let text = "description: >\n  author: 不该收到这里\n  name: 也不该\n";
        let fm = parse(text);
        assert_eq!(fm.get_nested_str("description", "author"), None);
        assert!(fm.get_str("description").unwrap().contains("author:"));
    }

    #[test]
    fn parses_block_array() {
        let (head, _) = split_document(SAMPLE);
        let fm = parse(head.unwrap());
        assert_eq!(fm.get_list("allowed-tools"), vec!["Bash", "Read"]);
    }

    #[test]
    fn parses_inline_array() {
        let (head, _) = split_document(SAMPLE);
        let fm = parse(head.unwrap());
        assert_eq!(fm.get_list("tags"), vec!["ci", "release"]);
    }

    #[test]
    fn space_delimited_scalar_becomes_list() {
        let fm = Frontmatter {
            fields: [("k".into(), YamlValue::Str("Bash Read Write".into()))]
                .into_iter()
                .collect(),
            nested: Default::default(),
        };
        assert_eq!(fm.get_list("k"), vec!["Bash", "Read", "Write"]);
    }

    #[test]
    fn no_frontmatter_returns_none() {
        let (head, body) = split_document("# 直接是正文");
        assert!(head.is_none());
        assert_eq!(body, "# 直接是正文");
    }

    #[test]
    fn unterminated_frontmatter_is_ignored() {
        // 没有结束分隔符时，不能把整篇当 frontmatter 吞掉
        let (head, _) = split_document("---\nname: x\n没有结束");
        assert!(head.is_none());
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let t = "# 注释\n\nname: a  # 行尾注释\nversion: 2\n";
        let fm = parse(t);
        assert_eq!(fm.get_str("name"), Some("a"));
        assert_eq!(fm.get_str("version"), Some("2"));
    }

    #[test]
    fn handles_crlf_line_endings() {
        let (head, _) = split_document("---\r\nname: crlf\r\n---\r\n正文");
        assert!(head.is_some());
        assert_eq!(parse(head.unwrap()).get_str("name"), Some("crlf"));
    }

    #[test]
    fn field_order_is_stable() {
        let a = parse("z: 1\na: 2\nm: 3");
        let b = parse("a: 2\nm: 3\nz: 1");
        assert_eq!(
            a.fields.keys().collect::<Vec<_>>(),
            b.fields.keys().collect::<Vec<_>>()
        );
    }

    // ── 缺陷 B：块标量与嵌套键（2026-10-18）────────────────────────

    #[test]
    fn folded_block_scalar_becomes_real_text() {
        // 回归：真实库里 35 条 description = ">" 字面量。
        let t = [
            "name: 视觉情绪板",
            "description: >",
            "  视觉情绪板（Mood Board / Design System 起点）。基于 /用户旅程 + /用户故事 上游",
            "  产出，推导 3-4 套色彩方向不同的设计方案。",
            "",
            "  触发关键词：视觉情绪板、情绪板。",
            "allowed-tools:",
            "  - Read",
        ]
        .join("\n");
        let fm = parse(&t);
        let d = fm.get_str("description").expect("必须解析出 description");
        assert!(!d.starts_with('>'), "description 不能还是块标量指示符: {d:?}");
        assert!(d.contains("Mood Board"), "正文必须被读进来: {d:?}");
        assert!(d.contains("触发关键词"), "空行之后的后半段也必须读进来: {d:?}");
        // 折叠：相邻行用一个空格连接，段落之间是换行
        assert!(d.contains("上游 产出"), "折叠应把折行接成一个空格: {d:?}");
        assert!(!d.contains("\n\n\n"), "不应残留连续空行: {d:?}");
        // 块之后的顶层键必须照常解析
        assert_eq!(fm.get_list("allowed-tools"), vec!["Read"]);
    }

    #[test]
    fn literal_block_scalar_keeps_line_breaks() {
        let t = [
            "description: |",
            "  第一行",
            "  第二行",
            "other: x",
        ]
        .join("\n");
        let fm = parse(&t);
        let d = fm.get_str("description").expect("descriptor");
        assert_eq!(d.trim_end(), "第一行\n第二行");
        assert_eq!(fm.get_str("other"), Some("x"));
    }

    #[test]
    fn chomping_indicators_are_honoured() {
        let clip = parse("d: >\n  a\n  b\n");
        assert_eq!(clip.get_str("d"), Some("a b\n"), "clip 缺省恰好一个换行");

        let strip = parse("d: >-\n  a\n  b\n");
        assert_eq!(strip.get_str("d"), Some("a b"), "strip 不带尾随换行");

        let literal_strip = parse("d: |-\n  a\n  b\n");
        assert_eq!(literal_strip.get_str("d"), Some("a\nb"));
    }

    #[test]
    fn nested_keys_never_override_top_level_keys() {
        // 回归：真实库里 6 条 name = "string"，来自 chain.schema.name。
        let t = [
            "name: 视觉情绪板",
            "chain:",
            "  protocol_version: \"1.0\"",
            "  schema:",
            "    skill: string",
            "    generated_at: string",
            "    project_name: string",
            "    name: string",
            "description: 真正的描述",
        ]
        .join("\n");
        let fm = parse(&t);
        assert_eq!(fm.get_str("name"), Some("视觉情绪板"), "嵌套 name 绝不能覆盖顶层 name");
        assert_eq!(fm.get_str("description"), Some("真正的描述"));
        assert!(!fm.fields.contains_key("skill"), "嵌套键不该进顶层字段表");
    }

    #[test]
    fn duplicate_top_level_key_keeps_the_first() {
        // 「已有键不覆盖」保护：重复键保留第一次出现的值。
        let fm = parse("name: 第一个\ndescription: d\nname: 第二个");
        assert_eq!(fm.get_str("name"), Some("第一个"));
    }

    #[test]
    fn block_scalar_inside_nested_map_does_not_leak_to_top_level() {
        // 嵌套映射里的块标量同样不能进顶层字段。
        let t = [
            "chain:",
            "  notes: >",
            "    这是嵌套里的块标量",
            "name: top",
        ]
        .join("\n");
        let fm = parse(&t);
        assert_eq!(fm.get_str("name"), Some("top"));
        assert!(!fm.fields.contains_key("notes"));
    }

    #[test]
    fn block_scalar_stops_at_next_top_level_key() {
        let t = [
            "description: >",
            "  一段",
            "name: x",
            "version: 2",
        ]
        .join("\n");
        let fm = parse(&t);
        assert_eq!(fm.get_str("name"), Some("x"));
        assert_eq!(fm.get_str("version"), Some("2"));
        assert_eq!(fm.get_str("description").map(|s| s.trim()), Some("一段"));
    }
}
