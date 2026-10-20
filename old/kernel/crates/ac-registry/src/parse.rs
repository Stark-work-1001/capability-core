//! 三种资产格式的解析器。
//!
//! - `SKILL.md`（Agent Skills 开放标准）
//! - `plugin.json`（Claude Code 插件清单）
//! - `.mcp.json`（MCP server 配置）
//!
//! 三者统一产出 `CapabilityManifest`，这是 capability-core 的核心抽象。

use crate::frontmatter;
use crate::infer::{infer, InferenceInput};
use ac_core::{
    AssetKind, CapabilityManifest, ProvidedCapability, Provenance, RelationKind, SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use std::path::Path;

/// 解析结果。
#[derive(Debug)]
pub enum ParseOutcome {
    Ok(Box<CapabilityManifest>),
    /// 资产存在但无法解析，记录原因并跳过，**不中断扫描**
    Skipped { reason: String },
}

/// 内容指纹（SHA-256 前 16 字节十六进制），用于增量索引。
pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex(&digest[..8])
}
pub fn sha256_digest(bytes:&[u8])->String {hex(&Sha256::digest(bytes))}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 资产 id 的作用域。
///
/// 早先只用"清单文件所在目录名"当 id，在真实资产上立刻塌了：
/// `.qoder-plugin` / `.claude-plugin` / `.codex-plugin` 这些是**清单目录名**
/// 而不是插件标识，22 个互不相干的插件全叫 `.qoder-plugin`，于是互相覆盖——
/// 磁盘上 508 个清单文件只入库 427 个，78 个资产被静默吞掉，且不报错。
/// 改用「相对扫描根的目录路径」后 id 天然唯一、可读、且不随挂载点变化。
#[derive(Debug, Clone, Copy, Default)]
pub struct IdScope<'a> {
    /// 相对扫描根的目录路径，分隔符不限
    pub rel_dir: Option<&'a str>,
    /// 同目录下有多份同类清单时的判别后缀（短哈希）。
    ///
    /// 例如 `mcp-local/` 下同时存在 `.mcp.json` 与 `mcp.json`，
    /// 光看目录无法区分。扫描器在发现冲突时才给这个后缀，
    /// 所以绝大多数资产的 id 仍然是干净可读的。
    pub tag: Option<&'a str>,
}

impl<'a> IdScope<'a> {
    pub fn new(rel_dir: &'a str) -> Self {
        Self {
            rel_dir: Some(rel_dir),
            tag: None,
        }
    }

    pub fn tagged(rel_dir: &'a str, tag: &'a str) -> Self {
        Self {
            rel_dir: Some(rel_dir),
            tag: Some(tag),
        }
    }

    /// 计算资产 id 后缀：优先相对路径，退回所在目录名。
    pub fn slug(&self, path: &Path) -> String {
        let base = match self.rel_dir {
            Some(rel) if !rel.trim_matches(['/', '\\']).is_empty() => slugify(rel),
            _ => path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .map(slugify)
                .unwrap_or_default(),
        };
        let base = if base.is_empty() {
            "unnamed".to_string()
        } else {
            base
        };
        match self.tag.filter(|t| !t.is_empty()) {
            Some(t) => format!("{base}-{t}"),
            None => base,
        }
    }
}

/// 把路径压成 id 友好的短横线串：保留 Unicode 字母数字、去前导点、其余转 `-`。
///
/// **必须保留非 ASCII**：真实资产里大量目录是中文
/// （`skills/产品指标复盘/SKILL.md`）。早先只留 ASCII，中文段被整个
/// 丢掉，7 个不同技能共用同一个 id。SQLite / JSON / Tauri 都能直接吃
/// UTF-8，没有理由为了"看着像 slug"而丢信息。
///
/// ```text
/// .qoder-plugin                              -> qoder-plugin
/// plugin/knowledge-process-suite/.qoder-plugin
///     -> plugin-knowledge-process-suite-qoder-plugin
/// skills/产品指标复盘                        -> skills-产品指标复盘
/// ```
pub fn slugify(raw: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for seg in raw.split(['/', '\\']) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let mut s = String::with_capacity(seg.len());
        let mut prev_dash = false;
        for c in seg.chars() {
            if c == '.' && s.is_empty() {
                continue; // 吃掉前导的点，隐藏目录不该参与 id
            }
            if c.is_alphanumeric() || c == '_' || c == '-' {
                s.extend(c.to_lowercase());
                prev_dash = false;
            } else if !prev_dash && !s.is_empty() {
                s.push('-');
                prev_dash = true;
            }
        }
        let s = s.trim_matches('-').to_string();
        if !s.is_empty() {
            out.push(s);
        }
    }
    out.join("-")
}

/// 按文件类型分派。
pub fn parse_asset(path: &Path, kind_hint: Option<AssetKind>, scope: IdScope<'_>) -> ParseOutcome {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if ["openapi.json","agent-card.json","ghost.json","organization.json"].contains(&name) {
        return match std::fs::read_to_string(path).map_err(|e|e.to_string()).and_then(|s|crate::formats::parse(path,&s,scope)) {
            Ok(m)=>ParseOutcome::Ok(Box::new(m)), Err(reason)=>ParseOutcome::Skipped{reason}
        };
    }
    let kind = kind_hint.unwrap_or(match name {
        "SKILL.md" => AssetKind::Skill,
        "plugin.json" => AssetKind::Plugin,
        ".mcp.json" | "mcp.json" | "mcp-servers.json" => AssetKind::Mcp,
        _ => return ParseOutcome::Skipped { reason: format!("未知文件类型: {name}") },
    });

    match std::fs::read_to_string(path) {
        Ok(text) => {
            let bytes = text.as_bytes();
            let m = match kind {
                AssetKind::Skill => parse_skill_md(path, &text, scope),
                AssetKind::Plugin => parse_plugin_json(path, &text, scope),
                AssetKind::Mcp => parse_mcp_json(path, &text, scope),
                other => {
                    return ParseOutcome::Skipped {
                        reason: format!("该格式暂不支持: {other}"),
                    }
                }
            };
            let mut m = m;
            m.provenance.content_hash = Some(content_hash(bytes));
            // M1.5 L2：规范化指纹。**不参与自动合并**，只用于生成
            // 「L1 不同但 L2 相同」的待确认候选，交给用户裁决。
            m.provenance.normalized_hash = Some(crate::fingerprint::l2_hash(&text));
            m.provenance.source_path = Some(path.to_string_lossy().to_string());
            m.provenance.imported_at = Some(now_rfc3339());
            ParseOutcome::Ok(Box::new(m))
        }
        Err(e) => ParseOutcome::Skipped {
            reason: format!("读取失败 {path:?}: {e}"),
        },
    }
}

/// 抽取**作者自己写的**标签，与机器标签区分开。
///
/// M1 审计发现 `capabilities.tags` 只写不读，补全链路时必须先回答
/// "筛的到底是什么"。实测（506 资产 / 460 个 SKILL.md）：
/// - SKILL.md 的 frontmatter **没有** `tags:` 键，但有 `category` /
///   `capabilities` / `tools`；
/// - plugin.json 有 36 份，其中 **10 份**带 `tags:`、15 份带 `keywords:`。
///
/// 所以作者标签的来源就定为这三处，全部转成 `author:<值>` 前缀，
/// 与 `bundle:*` / `tool` 这类机器标签在库里天然可区分。
/// 前缀是必须的：`skill` 既可能是作者的分类词，也可能是 `bundle:skills`
/// 剥掉前缀后的样子，不加前缀就没法按类别筛。
pub fn extract_author_tags(fm: &frontmatter::Frontmatter) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let v = raw.trim().trim_matches(['"', '\'', ' ']);
        if v.is_empty() {
            return;
        }
        let tag = format!("author:{v}");
        if !out.contains(&tag) {
            out.push(tag);
        }
    };

    for key in ["tags", "category", "keywords", "capabilities"] {
        for item in fm.get_list(key) {
            // `capabilities: [a, b]` 可能是逗号串
            for part in item.split(',') {
                push(part);
            }
        }
        if let Some(s) = fm.get_str(key) {
            for part in s.split(',') {
                push(part);
            }
        }
    }
    out
}

/// 记一条运行期引用（去重、去空）。顺序保持源文件里的出现顺序，
/// 便于出问题时一眼对回原文。
fn push_ref(out: &mut Vec<String>, raw: &str) {
    let v = raw.trim();
    if !v.is_empty() && !out.iter().any(|x| x == v) {
        out.push(v.to_string());
    }
}

fn now_rfc3339() -> String {
    // 不引入 chrono：扫描性能敏感，且只需一个可排序的时间戳。
    // Unix 秒足够做增量排序，展示时再格式化。
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into())
}

// ── SKILL.md ────────────────────────────────────────────────

pub fn parse_skill_md(path: &Path, text: &str, scope: IdScope<'_>) -> CapabilityManifest {
    let (head, body) = frontmatter::split_document(text);
    let fm = head.map(frontmatter::parse).unwrap_or_default();

    let dir_name = scope.slug(path);

    // 规范要求 name 与目录名一致；不一致时以 frontmatter 为准但保留目录名线索
    let name = fm
        .get_str("name")
        .map(|s| s.to_string())
        .unwrap_or_else(|| dir_name.clone());
    let description = fm.get_str("description").unwrap_or("").to_string();

    // 版本号：SKILL.md 的 frontmatter 里 460 个中有 45 个写了 `version:`
    // （形如 `version: "1.2.0"`）。早先一律写死 "1"，于是 M1.5 要证明的
    // 「多版本共存」根本无从展示——两个版本在库里长得一模一样。
    // 这里如实取用作者声明的版本号；没声明的退回 "1"（沿用旧行为）。
    let version = fm
        .get_str("version")
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "1".into());

    // v8：作者自声明。两处都认，**不猜**：
    //   1. 顶层 `author:`（真实语料 1 份：render-deploy）
    //   2. 上游规范写法 `metadata.author`（真实语料 8 份：4 个 MathWorks 等）
    // 读不到就 None——空着就是空着，不写占位串。
    let declared_author = fm
        .get_str("author")
        .or_else(|| fm.get_nested_str("metadata", "author"))
        .or_else(|| fm.get_nested_str("metadata", "skill-author"))
        .map(|s| s.trim().trim_matches(['"', '\''].as_slice()).to_string())
        .filter(|s| !s.is_empty());

    // 名字与出处必须**同生同死**：没有名字时出处也留 None。
    // 只写出处不写名字会让 `author` 模块把一份空声明当成一档来源。
    let (author, author_source) = match declared_author {
        Some(a) => (
            Some(a),
            Some(crate::author::SOURCE_FRONTMATTER.to_string()),
        ),
        None => (None, None),
    };

    let mut m = CapabilityManifest {
        schema_version: SCHEMA_VERSION.into(),
        id: format!("skill::{dir_name}"),
        kind: AssetKind::Skill,
        name,
        version,
        description: description.clone(),
        identity: Default::default(),
        provides: Vec::new(),
        relations: Vec::new(),
        provenance: Provenance {
            source_format: Some("skill_md".into()),
            author,
            author_source,
            ..Default::default()
        },
    };

    // Skill 提供「能力」而非「工具」：把正文摘要作为能力声明
    let summary = summarize_body(body, 400);
    // 标签 = 作者声明的可用工具 + 作者自己写的分类词。
    // 两者都保留：工具名（Read/Write/Bash）是 M1 唯一的标签来源，
    // 分类词才是用户真正想按之筛选的东西（见 store.rs 的 TAG_SEMANTICS）。
    let mut skill_tags = fm.get_list("allowed-tools");
    for t in extract_author_tags(&fm) {
        if !skill_tags.contains(&t) {
            skill_tags.push(t);
        }
    }

    m.provides.push(ProvidedCapability {
        capability_id: format!("skill::{dir_name}::run"),
        name: "run".into(),
        summary: if description.is_empty() { summary } else { description.clone() },
        input_schema: None,
        tags: skill_tags,
    });

    // allowed-tools 是作者声明的"能用哪些工具"，是身份推断的第一手依据。
    // 注意它不能当 tags 传——tags 在 infer 里语义是"行为标注"，
    // 混进去会让 allowed-tools 里恰好叫 read 的工具被当成"声明只读"。
    let allowed = fm.get_list("allowed-tools");
    m.identity = infer(&InferenceInput {
        description: &m.description,
        tool_names: &allowed,
        tags: &[],
        has_hooks: false,
        open_world: false,
        name: &m.name,
    });

    m
}

fn summarize_body(body: &str, max: usize) -> String {
    let t: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if t.chars().count() <= max {
        t
    } else {
        t.chars().take(max).collect::<String>() + "…"
    }
}

// ── plugin.json ─────────────────────────────────────────────

pub fn parse_plugin_json(path: &Path, text: &str, scope: IdScope<'_>) -> CapabilityManifest {
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => serde_json::Value::Null,
    };

    let dir = scope.slug(path);

    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or(&dir)
        .to_string();
    let description = v
        .get("description")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let version = v
        .get("version")
        .and_then(|x| x.as_str())
        .unwrap_or("0")
        .to_string();

    // v8：作者自声明。真实语料里两种写法都存在（36 份 plugin.json 有 26 份带 author）：
    //   "author": "Zion"                      —— 字符串（1 份）
    //   "author": {"name": "Max Zheng", ...}   —— 对象（25 份）
    // 对象里还有 url / email，这里只取 name：界面上那一行显示的是人，不是名片。
    // name 缺失时**不退回 url**——URL 不是作者名，混进来会让属性栏显示一条链接。
    let declared_author: Option<String> = match v.get("author") {
        Some(serde_json::Value::String(s)) => Some(s.trim().to_string()),
        Some(serde_json::Value::Object(o)) => o
            .get("name")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string()),
        _ => None,
    }
    .filter(|s| !s.is_empty());

    let (author, author_source) = match declared_author {
        Some(a) => (
            Some(a),
            Some(crate::author::SOURCE_MANIFEST.to_string()),
        ),
        None => (None, None),
    };

    let mut m = CapabilityManifest {
        schema_version: SCHEMA_VERSION.into(),
        id: format!("plugin::{dir}"),
        kind: AssetKind::Plugin,
        name,
        version,
        description: description.clone(),
        identity: Default::default(),
        provides: Vec::new(),
        relations: Vec::new(),
        provenance: Provenance {
            source_format: Some("plugin_json".into()),
            author,
            author_source,
            ..Default::default()
        },
    };

    // 插件携带的五类组件，全部转成能力声明
    let mut tool_names: Vec<String> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    // v7：声明式组件路径（`"skills": "skills"`）是「自包含 / 挂载」的关键证据。
    let mut runtime_refs: Vec<String> = Vec::new();

    // 作者自己写的分类词：plugin.json 的 tags / keywords / category。
    // 加 `author:` 前缀与机器标签区分（见 store.rs 的 TAG_SEMANTICS）。
    let author_tags: Vec<String> = ["tags", "keywords", "category"]
        .iter()
        .filter_map(|k| v.get(*k).and_then(|x| x.as_str()))
        .flat_map(|s| s.split([',', ' ']).map(|x| x.trim().to_string()))
        .filter(|s| !s.is_empty())
        .map(|s| format!("author:{s}"))
        .collect();
    let mut author_tags = author_tags;
    author_tags.sort();
    author_tags.dedup();

    for comp in ["commands", "skills", "agents", "hooks", "mcpServers"] {
        let Some(raw) = v.get(comp) else { continue };

        // 三种写法都要认，真实资产里都出现过：
        //   1. 数组      "commands": [{...}]
        //   2. 对象      "hooks": {"PostToolUse": [...]}（Claude 原生）
        //   3. 字符串    "skills": "skills"（指向同级的组件目录）
        // 宽容一点比漏掉整个组件强——漏了会直接影响身份推断与检索。
        let items: Vec<(&str, &serde_json::Value)> = match raw {
            serde_json::Value::Array(a) => a.iter().map(|x| ("", x)).collect(),
            serde_json::Value::Object(o) => o.iter().map(|(k, x)| (k.as_str(), x)).collect(),
            serde_json::Value::String(dir) => {
                // 声明式引用：内容在磁盘上的另一个目录里。
                // 这里只如实记下声明位置并建立关系，展开内容由 M2 的
                // 组件解析器负责——不在解析阶段做 IO 是刻意的边界。
                let loc = dir.as_str();
                push_ref(&mut runtime_refs, loc);
                m.provides.push(ProvidedCapability {
                    capability_id: format!("plugin::{dir}::{comp}"),
                    name: comp.to_string(),
                    summary: format!("声明位置 {loc}（组件内容待展开）"),
                    input_schema: None,
                    tags: vec![format!("bundle:{comp}"), "declared-path".into()],
                });
                m.relations.push(ac_core::AssetRelation::new(
                    RelationKind::Requires,
                    format!("plugin::{dir}::{comp}@{loc}"),
                ));
                if comp == "hooks" {
                    tags.push("hooks".into());
                }
                continue;
            }
            _ => continue,
        };
        if items.is_empty() {
            continue;
        }

        // 结构标签 bundle:* + 作者标签（plugin.json 的 tags/keywords 是真有的：
        // 36 份里 10 份带 tags、15 份带 keywords）
        let mut comp_tags = vec![format!("bundle:{comp}")];
        for t in &author_tags {
            if !comp_tags.contains(t) {
                comp_tags.push(t.clone());
            }
        }
        m.provides.push(ProvidedCapability {
            capability_id: format!("plugin::{dir}::{comp}"),
            name: comp.to_string(),
            summary: format!("插件携带 {} 项 {}", items.len(), comp),
            input_schema: None,
            tags: comp_tags,
        });

        for (key, it) in &items {
            if comp != "mcpServers" {
                continue;
            }
            // 名字优先取子项的 name，数组写法没给就退回对象的键
            let child = it
                .get("name")
                .and_then(|x| x.as_str())
                .or(if key.is_empty() { None } else { Some(*key) });
            let Some(child) = child else { continue };
            tool_names.push(child.to_string());
            // 扩展关系：插件 -> 其携带的 MCP server
            m.relations.push(ac_core::AssetRelation::new(
                RelationKind::Extends,
                format!("mcp::{child}"),
            ));
        }

        if comp == "hooks" {
            tags.push("hooks".into());
        }
    }

    // 组件写在同级目录、清单里不内联的插件，provides 天然为空，
    // 于是它的作者标签无处安放 —— 标签筛选就看不见这个插件。
    // 这里补一条"包本身"的能力专门承载作者标签，让标签跟着插件走。
    if m.provides.is_empty() && !author_tags.is_empty() {
        m.provides.push(ProvidedCapability {
            capability_id: format!("plugin::{dir}::package"),
            name: "package".into(),
            summary: "插件包本体（组件由 M2 展开）".into(),
            input_schema: None,
            tags: author_tags.clone(),
        });
    }

    m.provenance.runtime_refs = runtime_refs;

    m.identity = infer(&InferenceInput {
        description: &m.description,
        tool_names: &tool_names,
        tags: &tags,
        has_hooks: tags.iter().any(|t| t == "hooks"),
        open_world: false,
        name: &m.name,
    });

    m
}

// ── .mcp.json ───────────────────────────────────────────────

/// 往上找一个不像版本号的目录名，作为展示用标题。
fn display_dir_name(path: &Path) -> String {
    let mut cur = path.parent();
    let mut first: Option<String> = None;
    while let Some(d) = cur {
        let Some(name) = d.file_name().and_then(|s| s.to_str()) else {
            break;
        };
        if name.is_empty() {
            break;
        }
        let owned = name.to_string();
        if first.is_none() {
            first = Some(owned.clone());
        }
        if !name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return owned;
        }
        cur = d.parent();
    }
    first.unwrap_or_else(|| "root".to_string())
}

pub fn parse_mcp_json(path: &Path, text: &str, scope: IdScope<'_>) -> CapabilityManifest {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);

    // 支持两种形态：{"mcpServers": {...}} 与 {...} 直接是 server 表
    let servers = v
        .get("mcpServers")
        .and_then(|x| x.as_object())
        .or_else(|| v.get("servers").and_then(|x|x.as_object()))
        .or_else(|| v.as_object())
        .cloned()
        .unwrap_or_default();

    let file_key = scope.slug(path);

    // 展示名：清单常被放在版本目录里（`presentations/0.1.1/.mcp.json`），
    // 直接取所在目录会显示成 "0.1.1"，往上找一个不像版本号的目录名。
    let display = display_dir_name(path);

    let mut m = CapabilityManifest {
        schema_version: SCHEMA_VERSION.into(),
        id: format!("mcp::{file_key}"),
        kind: AssetKind::Mcp,
        name: display,
        version: "1".into(),
        description: format!("MCP server 配置，包含 {} 个 server", servers.len()),
        identity: Default::default(),
        provides: Vec::new(),
        relations: Vec::new(),
        provenance: Provenance {
            source_format: Some("mcp_json".into()),
            ..Default::default()
        },
    };

    let mut tool_names: Vec<String> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    // v7：把「这个 server 靠什么活着」的原始证据存下来。
    // 判定逻辑不在这里（见 origin 模块）——解析器只负责**原样记**，
    // 谁来判、判成什么，是另一件事。两者混在一起就再也没法单独验证判据。
    let mut runtime_refs: Vec<String> = Vec::new();

    for (srv_name, cfg) in &servers {
        m.provides.push(ProvidedCapability {
            capability_id: format!("mcp::{file_key}::{srv_name}"),
            name: srv_name.clone(),
            summary: cfg
                .get("command")
                .and_then(|x| x.as_str())
                .map(|c| format!("stdio server，启动命令 {c}"))
                .or_else(|| {
                    cfg.get("url")
                        .and_then(|x| x.as_str())
                        .map(|u| format!("HTTP server，地址 {u}"))
                })
                .unwrap_or_else(|| "MCP server".into()),
            input_schema: None,
            tags: vec!["tool".into()],
        });

        // 常见命令名可作为权限线索
        if let Some(cmd) = cfg.get("command").and_then(|x| x.as_str()) {
            tool_names.push(cmd.to_string());
            push_ref(&mut runtime_refs, cmd);
            // args 里也藏着路径：`command: "python"` + `args: ["D:/venv/x.py"]`
            // 与 `command: "D:/venv/python.exe"` 是同一件事，只看 command 会漏。
            if let Some(args) = cfg.get("args").and_then(|x| x.as_array()) {
                for a in args {
                    if let Some(s) = a.as_str() {
                        push_ref(&mut runtime_refs, s);
                    }
                }
            }
            if cmd.contains("git") || cmd.contains("docker") {
                tags.push("write".into());
            }
        }
        if let Some(url) = cfg.get("url").and_then(|x| x.as_str()) {
            push_ref(&mut runtime_refs, url);
            tags.push("open_world".into());
        }
    }
    m.provenance.runtime_refs = runtime_refs;

    m.identity = infer(&InferenceInput {
        description: &m.description,
        tool_names: &tool_names,
        tags: &tags,
        has_hooks: false,
        open_world: tags.iter().any(|t| t == "open_world"),
        name: &m.name,
    });

    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;

    // ── v8：作者自声明抽取 ──────────────────────────────────────

    #[test]
    fn skill_frontmatter_author_is_captured_with_its_source() {
        let text = "---\nname: matlab\nauthor: MathWorks\ndescription: x\n---\n正文";
        let m = parse_skill_md(Path::new("/x/matlab/SKILL.md"), text, IdScope::default());
        assert_eq!(m.provenance.author.as_deref(), Some("MathWorks"));
        assert_eq!(
            m.provenance.author_source.as_deref(),
            Some(crate::author::SOURCE_FRONTMATTER),
            "出处必须记成 frontmatter，否则界面没法按档标注"
        );
    }

    #[test]
    fn skill_without_author_leaves_both_fields_empty() {
        // 真实语料 460 个 SKILL.md 只有 4 个写了 author。其余的必须是
        // None（不是占位串），否则「不知道」和「有个叫 unknown 的作者」分不开。
        let text = "---\nname: x\ndescription: y\n---\n正文";
        let m = parse_skill_md(Path::new("/x/x/SKILL.md"), text, IdScope::default());
        assert!(m.provenance.author.is_none());
        assert!(m.provenance.author_source.is_none());
    }

    #[test]
    fn blank_author_is_not_a_declaration() {
        // 作者写了 `author:` 但值是空 / 引号：不算声明。
        for text in [
            "---\nname: x\nauthor:\n---\n正文",
            "---\nname: x\nauthor: \"\"\n---\n正文",
        ] {
            let m = parse_skill_md(Path::new("/x/x/SKILL.md"), text, IdScope::default());
            assert!(
                m.provenance.author.is_none(),
                "空 author 不该算声明: {text:?} -> {:?}",
                m.provenance.author
            );
            assert!(m.provenance.author_source.is_none(), "名字没有出处也不该有");
        }
    }

    #[test]
    fn plugin_author_object_form_uses_name_only() {
        // 真实语料 26 份带 author 的 plugin.json 里 25 份是对象。
        let text = r#"{"name":"qflow","description":"d","author":{"name":"Max Zheng","url":"https://x"}}"#;
        let m = parse_plugin_json(Path::new("/x/qflow/plugin.json"), text, IdScope::default());
        assert_eq!(m.provenance.author.as_deref(), Some("Max Zheng"));
        assert_eq!(
            m.provenance.author_source.as_deref(),
            Some(crate::author::SOURCE_MANIFEST)
        );
    }

    #[test]
    fn plugin_author_string_form_is_captured() {
        // 真实语料里 1 份是字符串写法（zion-nocode 的 .kimi-plugin）。
        let text = r#"{"name":"zion","description":"d","author":"Zion"}"#;
        let m = parse_plugin_json(Path::new("/x/zion/plugin.json"), text, IdScope::default());
        assert_eq!(m.provenance.author.as_deref(), Some("Zion"));
        assert_eq!(
            m.provenance.author_source.as_deref(),
            Some(crate::author::SOURCE_MANIFEST)
        );
    }

    #[test]
    fn plugin_author_object_without_name_is_not_guessed_from_url() {
        // url 不是作者名。混进来会让属性栏把一条链接显示成作者。
        let text = r#"{"name":"x","description":"d","author":{"url":"https://example.com"}}"#;
        let m = parse_plugin_json(Path::new("/x/x/plugin.json"), text, IdScope::default());
        assert!(m.provenance.author.is_none(), "不得从 url 编一个作者");
        assert!(m.provenance.author_source.is_none());
    }

    #[test]
    fn mcp_json_has_no_author_concept() {
        // .mcp.json 是 server 配置，没有作者字段。绝不能凭空造一个。
        let text = r#"{"mcpServers":{"s":{"command":"uvx"}}}"#;
        let m = parse_mcp_json(Path::new("/x/.mcp.json"), text, IdScope::default());
        assert!(m.provenance.author.is_none());
        assert!(m.provenance.author_source.is_none());
    }

    #[test]
    fn parses_skill_md() {
        let text = "---\nname: my-skill\ndescription: 做某事\nallowed-tools: [Bash, Read]\n---\n正文内容";
        let m = parse_skill_md(Path::new("/x/my-skill/SKILL.md"), text, IdScope::default());
        assert_eq!(m.kind, AssetKind::Skill);
        assert_eq!(m.name, "my-skill");
        assert_eq!(m.id, "skill::my-skill");
        assert_eq!(m.provides[0].capability_id, "skill::my-skill::run");
        // allowed-tools 里的 Bash 查表即 Write（能执行命令），
        // Read 只读不抵消——整体取权限上界
        assert!(m.identity.authority >= ac_core::Authority::Write);
        assert!(m.identity.tool_posture >= ac_core::ToolPosture::Writer);
    }

    #[test]
    fn skill_with_only_read_tools_stays_readonly() {
        let text = "---\nname: ro\ndescription: 只读技能\nallowed-tools: [Read, Grep, Glob]\n---\nx";
        let m = parse_skill_md(Path::new("/x/ro/SKILL.md"), text, IdScope::default());
        assert_eq!(m.identity.authority, ac_core::Authority::Observe);
        assert_eq!(m.identity.tool_posture, ac_core::ToolPosture::Reader);
    }

    #[test]
    fn skill_without_allowed_tools_stays_minimal() {
        // 没有声明就不猜，宁可给最低权限
        let text = "---\nname: bare\ndescription: 什么也没声明\n---\nx";
        let m = parse_skill_md(Path::new("/x/bare/SKILL.md"), text, IdScope::default());
        assert_eq!(m.identity.authority, ac_core::Authority::Read);
        assert_eq!(m.identity.tool_posture, ac_core::ToolPosture::Pure);
    }

    #[test]
    fn parses_plugin_json_with_bundles() {
        let text = r#"{
          "name": "suite-x", "version": "2.1", "description": "组合包",
          "commands": [{"name":"a"}],
          "mcpServers": [{"name":"gh"}],
          "hooks": {"PostToolUse": []}
        }"#;
        let m = parse_plugin_json(Path::new("/p/suite-x/plugin.json"), text, IdScope::default());
        assert_eq!(m.kind, AssetKind::Plugin);
        assert_eq!(m.version, "2.1");
        let names: Vec<&str> = m.provides.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"commands"));
        assert!(names.contains(&"mcpServers"));
        // hooks 存在 -> 姿态至少 Writer
        assert!(m.identity.tool_posture >= ac_core::ToolPosture::Writer);
        assert!(m.relations.iter().any(|r| r.target_id == "mcp::gh"));
    }

    #[test]
    fn parses_mcp_json_both_shapes() {
        let a = parse_mcp_json(
            Path::new("/m/gromacs/.mcp.json"),
            r#"{"mcpServers":{"gd":{"command":"uvx","args":[]}}}"#,
            IdScope::default(),
        );
        assert_eq!(a.provides.len(), 1);

        let b = parse_mcp_json(
            Path::new("/m/direct/.mcp.json"),
            r#"{"srv1":{"url":"https://x/mcp"}}"#,
            IdScope::default(),
        );
        assert_eq!(b.provides.len(), 1);
        assert!(b.identity.open_world);
    }

    #[test]
    fn malformed_json_does_not_panic() {
        let m = parse_plugin_json(Path::new("/p/x/plugin.json"), "{ not json", IdScope::default());
        assert_eq!(m.name, "x");
        assert!(m.provides.is_empty());
    }

    #[test]
    fn plugin_component_declared_as_directory_path() {
        // 回归：厂商格式里 `"skills": "skills"` 是**字符串路径**不是数组。
        // 只认数组/对象时，这些插件全是 0 能力，在界面上等于不存在。
        let text = r#"{
          "name": "qgraphflow", "version": "0.0.5", "description": "流程图技能",
          "skills": "skills"
        }"#;
        let m = parse_plugin_json(Path::new("/p/.qoder-plugin/plugin.json"), text, IdScope::default());
        assert_eq!(m.provides.len(), 1);
        assert_eq!(m.provides[0].name, "skills");
        assert!(m.provides[0].summary.contains("skills"), "{}", m.provides[0].summary);
        assert!(m.provides[0].tags.iter().any(|t| t == "declared-path"));
        // 如实记下引用位置，供后续展开组件用
        assert!(
            m.relations.iter().any(|r| r.target_id.ends_with("@skills")),
            "{:?}",
            m.relations
        );
    }

    #[test]
    fn plugin_component_string_and_array_can_coexist() {
        let text = r#"{
          "name": "mix", "description": "混搭",
          "commands": [{"name": "a"}],
          "skills": "skills"
        }"#;
        let m = parse_plugin_json(Path::new("/p/mix/plugin.json"), text, IdScope::default());
        let names: Vec<&str> = m.provides.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"commands"), "{names:?}");
        assert!(names.contains(&"skills"), "{names:?}");
    }

    #[test]
    fn mcp_display_name_skips_version_directory() {
        // 清单常在 `presentations/0.1.1/.mcp.json` 这种版本目录里，
        // 直接用所在目录会显示成 "0.1.1"
        let m = parse_mcp_json(
            Path::new("a/skill/presentations/0.1.1/.mcp.json"),
            r#"{"mcpServers":{"gd":{}}}"#,
            IdScope::new("skill/presentations/0.1.1"),
        );
        assert_eq!(m.name, "presentations");
        assert_eq!(m.id, "mcp::skill-presentations-0-1-1");

        // 没有版本目录时用最近一层
        let n = parse_mcp_json(
            Path::new("a/skill/gromacs/.mcp.json"),
            r#"{"mcpServers":{"gd":{}}}"#,
            IdScope::new("skill/gromacs"),
        );
        assert_eq!(n.name, "gromacs");
    }

    #[test]
    fn slugify_keeps_cjk_segments() {
        // 回归：早先只保留 ASCII，中文段被整个丢掉，
        // `skills/产品指标复盘` 和 `skills/竞品分析` 压成同一个空结果，
        // 7 个不同技能共用一个 id。
        assert_eq!(slugify("skills/产品指标复盘"), "skills-产品指标复盘");
        assert_ne!(
            slugify("skills/产品指标复盘"),
            slugify("skills/竞品分析")
        );
    }

    #[test]
    fn tag_disambiguates_same_dir_multiple_manifests() {
        let a = parse_mcp_json(
            Path::new("x/mcp-local/.mcp.json"),
            r#"{"mcpServers":{"gd":{}}}"#,
            IdScope::tagged("plugin/suite/mcp-local", "aaaaaa"),
        );
        let b = parse_mcp_json(
            Path::new("x/mcp-local/mcp.json"),
            r#"{"mcpServers":{"gd":{}}}"#,
            IdScope::tagged("plugin/suite/mcp-local", "bbbbbb"),
        );
        assert_ne!(a.id, b.id);
        assert!(a.id.starts_with("mcp::plugin-suite-mcp-local-aaaaaa"));
        assert!(b.id.starts_with("mcp::plugin-suite-mcp-local-bbbbbb"));
    }

    #[test]
    fn slugify_normalizes_hidden_dirs_and_separators() {
        assert_eq!(slugify(".qoder-plugin"), "qoder-plugin");
        assert_eq!(slugify("plugin/knowledge-process-suite/.qoder-plugin"),
                   "plugin-knowledge-process-suite-qoder-plugin");
        assert_eq!(slugify(r"skill\matlab"), "skill-matlab");
        assert_eq!(slugify("My Skill (v2)"), "my-skill-v2");
        assert_eq!(slugify("  "), "");
    }

    #[test]
    fn scope_makes_ids_unique_across_same_named_dirs() {
        // 回归：真实资产里 22 个不同插件的清单都在 `.qoder-plugin/` 下，
        // 曾经只按叶目录名取 id，于是它们互相覆盖、508 个资产只剩 427 个。
        let a = parse_plugin_json(
            Path::new("x/.qoder-plugin/plugin.json"),
            r#"{"name":"alpha"}"#,
            IdScope::new("plugin/knowledge-process-suite/.qoder-plugin"),
        );
        let b = parse_plugin_json(
            Path::new("x/.qoder-plugin/plugin.json"),
            r#"{"name":"beta"}"#,
            IdScope::new("plugin/obsidian-suite/.qoder-plugin"),
        );
        assert_ne!(a.id, b.id);
        assert_eq!(a.id, "plugin::plugin-knowledge-process-suite-qoder-plugin");
        assert_eq!(b.id, "plugin::plugin-obsidian-suite-qoder-plugin");
    }

    #[test]
    fn slug_falls_back_to_dirname_without_scope() {
        let m = parse_skill_md(
            Path::new("/x/my-skill/SKILL.md"),
            "---\nname: my-skill\n---\nx",
            IdScope::default(),
        );
        assert_eq!(m.id, "skill::my-skill");
    }

    #[test]
    fn content_hash_is_stable_and_short() {
        let a = content_hash(b"hello");
        let b = content_hash(b"hello");
        assert_eq!(a, b);
        assert_ne!(a, content_hash(b"world"));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn dispatch_by_filename() {
        // 必须落真实文件：parse_asset 读不到文件会返回 Skipped，
        // 那样断言 Ok 只是在测"文件存在"，测不到分派逻辑。
        let s = Scratch::new("dispatch");
        let skill = s.write("SKILL.md", "---\nname: dispatched\ndescription: d\n---\nx");
        let readme = s.write("README.md", "x");

        assert!(matches!(
            parse_asset(&skill, None, IdScope::default()),
            ParseOutcome::Ok(_)
        ));
        assert!(matches!(
            parse_asset(&readme, None, IdScope::default()),
            ParseOutcome::Skipped { .. }
        ));
    }

    #[test]
    fn body_summary_strips_headings_and_truncates() {
        let body = "# Title\n\n第一行内容\n\n第二行内容";
        let s = summarize_body(body, 400);
        assert!(!s.contains("Title"));
        assert!(s.contains("第一行内容"));

        let long = "字".repeat(500);
        let t = summarize_body(&long, 10);
        assert_eq!(t.chars().count(), 11); // 10 + 省略号
    }
}
