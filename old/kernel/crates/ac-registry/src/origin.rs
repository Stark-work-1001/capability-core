//! 来源（origin）与注册方式（registration）判定。
//!
//! # 这为什么是一个模块，而不是几行 SQL
//!
//! 用户明确要求「标明来源 / 类型」两个维度，而 2026-10-18 的审计实测：
//! **这两个维度在库里根本不存在**。能判它们的证据其实一直在磁盘上，只是
//! 没有任何地方把它变成字段：
//!
//! | 维度 | 证据（真实资产里确实存在的形态） |
//! |---|---|
//! | 来源 | `source_path` 落在 `vendor/` 下 = 厂商pack；落在 `assets/` 下 = 中枢自己纳管的 |
//! | 注册方式 | MCP 启动命令是裸命令名（`uvx`）/ 还是盘符绝对路径（`D:/…/python.exe`）/ 还是 http URL |
//!
//! 判定必须是**一个函数**，不能是散在几处的 SQL：迁移回填（老库没有新列）、
//! 解析入库（新扫描）、以及测试断言必须是同一套规则。三处各写一遍，迟早
//! 给出三个不同答案，而"同一份数据在库里和界面上说法不一样"是最难排查的
//! 一类故障。
//!
//! # 判定纪律
//!
//! - **有证据才认**。没有绝对路径 / 没有 URL 的，一律 `embedded`，
//!   绝不因为"看着像外部依赖"就猜 `mounted`。
//! - `vendor` 是按**路径段**判的，不是按字符串包含：`vendor/` 必须是一整段，
//!   否则 `vendorized-notes/` 这类目录名会被误判。
//! - 判不出来就退到最保守的类别，并在文档里写清楚为什么。

use ac_core::CapabilityManifest;

use crate::agents::infer_ecosystem;

/// 资产来源。三段互斥，覆盖全部资产。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginKind {
    /// 厂商随包带来的（`vendor/` 下）。内容不是本项目写的。
    Vendor,
    /// 中枢自己纳管的实体（`assets/` 下，由 install_mcp / install_skill 装配）。
    SelfBuilt,
    /// 本仓库/本机长出来的资产：既不是厂商pack，也不是中枢纳管。
    /// 这是**兜底类别**，不是"猜一个"——剩下所有情况都是它。
    Nurtured,
}

impl OriginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OriginKind::Vendor => "vendor",
            OriginKind::SelfBuilt => "self_built",
            OriginKind::Nurtured => "nurtured",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "vendor" => Some(OriginKind::Vendor),
            "self_built" => Some(OriginKind::SelfBuilt),
            "nurtured" => Some(OriginKind::Nurtured),
            _ => None,
        }
    }
}

/// 注册方式：这个能力**怎么被接进来的**。
///
/// 与 `AssetKind` 的区别要一直记住：`AssetKind` 说的是"资产长什么样"
/// （skill / plugin / mcp），本枚举说的是"它靠什么活着"。同一个 skill
/// 可以是 embedded（全部文件都在包内），也可以是 mounted（入口指向包外）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationKind {
    /// 自包含：所有运行期引用都在本包内（或只是裸命令名，由宿主 PATH 解析）。
    Embedded,
    /// 挂载：运行期引用里出现**包外的绝对路径**（盘符 / UNC / 根斜杠）。
    Mounted,
    /// 远程：运行期引用是 http(s) 端点，能力在别的机器上。
    Remote,
}

impl RegistrationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RegistrationKind::Embedded => "embedded",
            RegistrationKind::Mounted => "mounted",
            RegistrationKind::Remote => "remote",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "embedded" => Some(RegistrationKind::Embedded),
            "mounted" => Some(RegistrationKind::Mounted),
            "remote" => Some(RegistrationKind::Remote),
            _ => None,
        }
    }
}

/// 一次判定的完整结果。四个字段对应 assets 表的四列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginProfile {
    pub origin_kind: String,
    pub registration_kind: String,
    /// 推断出来的宿主生态；判不出来是**空串**，不是 "unassigned" 占位。
    /// 空串在 SQL 里好筛（`= ''`），而占位值会被当成真结论。
    pub origin_agent: String,
    /// 是否自包含（见 [is_self_contained] 的独立判据）。
    pub self_contained: bool,
}

/// 从扫描期留下的路径与运行期引用算出四列。
pub fn classify(source_path: &str, refs: &[String]) -> OriginProfile {
    let origin = origin_of(source_path);
    let reg = registration_of(refs);
    let eco = infer_ecosystem(&scan_relative(source_path)).ecosystem;
    let origin_agent = if eco == crate::agents::ECOSYSTEM_UNASSIGNED {
        String::new()
    } else {
        eco
    };
    OriginProfile {
        origin_kind: origin.as_str().to_string(),
        registration_kind: reg.as_str().to_string(),
        origin_agent,
        self_contained: is_self_contained(refs),
    }
}

/// 从 manifest 直接判定。解析入库走这条。
pub fn classify_manifest(m: &CapabilityManifest) -> OriginProfile {
    let source_path = m.provenance.source_path.as_deref().unwrap_or("");
    let refs = refs_of_manifest(m);
    classify(source_path, &refs)
}

/// 抽出 manifest 的**全部运行期引用**。
///
/// 两条来源，缺一不可：
///
/// 1. `provenance.runtime_refs`：解析器新写的结构化字段（本轮新增）。
/// 2. 老 manifest 的 `provides[].summary` 文本。真实库里 506 条 manifest 是
///    旧解析器写的，没有第 1 条；而 `migrate()` 只拿得到数据库、拿不到文件，
///    不可能重新解析。所以必须能从旧文本里**原样取回**这些引用，否则迁移
///    回填出来的四列全是低估——"挂载了外部 venv 的 MCP 显示成自包含"是
///    比没有这一列更糟的谎报。
///
/// 文本提取只认解析器自己写死的那三个前缀（见 parse.rs），不做模糊匹配。
pub fn refs_of_manifest(m: &CapabilityManifest) -> Vec<String> {
    if !m.provenance.runtime_refs.is_empty() {
        return m.provenance.runtime_refs.clone();
    }
    let mut out: Vec<String> = Vec::new();
    for p in &m.provides {
        if let Some(rest) = p.summary.strip_prefix("stdio server，启动命令 ") {
            push_ref(&mut out, rest);
        } else if let Some(rest) = p.summary.strip_prefix("HTTP server，地址 ") {
            push_ref(&mut out, rest);
        } else if let Some(rest) = p.summary.strip_prefix("声明位置 ") {
            let loc = rest.split('（').next().unwrap_or(rest);
            push_ref(&mut out, loc);
        }
    }
    // plugin 声明式引用另外落在关系上：`…::comp@<路径>`。
    for r in &m.relations {
        if let Some((_, path)) = r.target_id.rsplit_once('@') {
            push_ref(&mut out, path);
        }
    }
    out
}

fn push_ref(out: &mut Vec<String>, raw: &str) {
    let v = raw.trim().trim_end_matches(['。', '，', ',', ')', '）']);
    if !v.is_empty() && !out.iter().any(|x| x == v) {
        out.push(v.to_string());
    }
}

/// 把路径归一化成「相对扫描根」的形态：`..\vendor\…` / `..\assets\…`。
///
/// # 为什么必须归一化（真实踩到过）
///
/// `agents::infer_ecosystem` 的包名坐标是**固定位置** `segs[3]`，
/// 它按 `..\vendor\<kind>\<pkg>\…` 这种形态设计。同一个 vendor 目录，
/// 传相对路径得到 `pkg`，传绝对路径得到 `vendor`——**同一份资产、两种结论**。
///
/// 实测后果：老库 source_path 是 `..\vendor\…`（历史扫描根是相对的），
/// 迁移回填判出 80 个资产有宿主；换绝对扫描根重新入库后只剩 35 个。
/// 同一个字段在两条写入路径上给出不同答案，正是本项目反复强调不能出现的那种故障。
///
/// 归一化后两条路径结论一致，且**不改变** infer_ecosystem 自己的语义与测试。
fn scan_relative(path: &str) -> String {
    let norm = path.replace('\\', "/");
    let segs: Vec<&str> = norm.split('/').collect();
    let Some(idx) = segs.iter().position(|s| {
        let l = s.to_ascii_lowercase();
        l == "vendor" || l == "assets"
    }) else {
        return path.to_string();
    };
    // 前导 `..` 让 vendor 落在 segs[1]，与 infer_ecosystem 的坐标对齐。
    let mut out = String::from("..");
    for s in &segs[idx..] {
        out.push('\\');
        out.push_str(s);
    }
    out
}

/// 按路径段判来源。
///
/// 分隔符与大小写都归一化：真实库里的路径是 `..\vendor\skill\…` 这种
/// Windows 反斜杠形式，而测试里常写正斜杠；只认一种会让一半数据落错桶。
fn origin_of(source_path: &str) -> OriginKind {
    let norm = source_path.replace('\\', "/").to_ascii_lowercase();
    let segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    // vendor 必须在**第一个有效段之后**——路径以 vendor/ 开头同样算厂商包。
    if segs.iter().any(|s| *s == "vendor") {
        return OriginKind::Vendor;
    }
    if segs.iter().any(|s| *s == "assets") {
        return OriginKind::SelfBuilt;
    }
    OriginKind::Nurtured
}

/// 按引用判注册方式：remote > mounted > embedded。
///
/// 顺序不是随意的：一个既指向本地 venv、又连远端端点的 MCP，
/// 说它"自包含"是错的，说它"只是挂载"也不完整——**远程优先**。
fn registration_of(refs: &[String]) -> RegistrationKind {
    if refs.iter().any(|r| is_remote_url(r)) {
        return RegistrationKind::Remote;
    }
    if refs.iter().any(|r| is_external_abs_path(r)) {
        return RegistrationKind::Mounted;
    }
    RegistrationKind::Embedded
}

/// 自包含的**独立判据**：所有引用要么是本包内相对路径，要么是裸命令名 /
/// 环境变量占位，要么干脆没有引用。
///
/// 与 [registration_of] 在现有数据上会得到同一结果，这是**预期的**：
/// 两条判据看的是同一批证据。它单独存在是因为它们回答的是两个问题——
/// "它是怎么接进来的"（枚举，给筛选器用）与"删掉包外的东西它还能不能跑"
/// （布尔，给交付验收用）。将来若有第三条证据（比如符号链接指向包外），
/// 只会影响后者，不必动前者。
fn is_self_contained(refs: &[String]) -> bool {
    !refs
        .iter()
        .any(|r| is_remote_url(r) || is_external_abs_path(r))
}

/// http(s) 端点。
fn is_remote_url(s: &str) -> bool {
    let l = s.trim().to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// 包外绝对路径：盘符（`D:/` / `D:\`）、UNC（`\\\\server`）、POSIX 根（`/x`）。
///
/// 刻意**不**把 `./skills` / `skills` 这类相对路径算成外部：相对路径的基准
/// 是本资产所在目录，删掉包外的东西它照样能跑——这正是"自包含"的定义。
/// 真实资产里 20 条 `requires …@./skills` 全是这种形态，把它们算成挂载
/// 会把 mounted 从 5 条虚报成 25 条。
fn is_external_abs_path(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    let b = t.as_bytes();
    // 盘符：<letter>: 后面跟分隔符（避免把 "C:foo" 之外的怪串算进来）
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return true;
    }
    // UNC
    if t.starts_with("\\\\") || t.starts_with("//") {
        return true;
    }
    // POSIX 根：单个 / 开头，但排除 "./"（相对路径的写法）
    if t.starts_with('/') && !t.starts_with("./") {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn vendor_segment_means_vendor_origin() {
        let p = classify("..\\vendor\\skill\\a\\SKILL.md", &[]);
        assert_eq!(p.origin_kind, "vendor");
        // 反斜杠与正斜杠必须得到同一结论
        let q = classify("../vendor/skill/a/SKILL.md", &[]);
        assert_eq!(q.origin_kind, "vendor");
    }

    #[test]
    fn vendor_requires_a_whole_path_segment() {
        // 回归：按字符串包含判会把 vendorized-notes/ 误判成厂商包。
        let p = classify("..\\vendorized-notes\\a\\SKILL.md", &[]);
        assert_eq!(p.origin_kind, "nurtured");
    }

    #[test]
    fn assets_segment_means_self_built() {
        let p = classify("assets/mcp/obsidian-suite/mcp.json", &[]);
        assert_eq!(p.origin_kind, "self_built");
    }

    #[test]
    fn bare_command_is_embedded_not_mounted() {
        let p = classify("..\\vendor\\mcp\\x\\.mcp.json", &refs(&["uvx", "npx"]));
        assert_eq!(p.registration_kind, "embedded");
        assert!(p.self_contained);
    }

    #[test]
    fn drive_letter_path_is_mounted() {
        let p = classify(
            "..\\vendor\\mcp\\x\\.mcp.json",
            &refs(&["D:/.my-agent/.plugin/obsidian-agent/mcp/.venv/Scripts/python.exe"]),
        );
        assert_eq!(p.registration_kind, "mounted");
        assert!(!p.self_contained);
    }

    #[test]
    fn http_endpoint_is_remote_and_wins_over_mounted() {
        let p = classify(
            "..\\vendor\\skill\\x\\.mcp.json",
            &refs(&["D:/local/python.exe", "https://mcp.figma.com/mcp"]),
        );
        assert_eq!(p.registration_kind, "remote", "远程优先于挂载");
        assert!(!p.self_contained);
    }

    #[test]
    fn relative_declared_path_is_not_a_mount() {
        // 真实库 20 条 requires …@./skills 全是相对路径。
        let p = classify("..\\vendor\\plugin\\x\\plugin.json", &refs(&["./skills", "./.mcp.json"]));
        assert_eq!(p.registration_kind, "embedded");
        assert!(p.self_contained);
    }

    #[test]
    fn ecosystem_inference_is_independent_of_scan_root_form() {
        // 回归：同一份资产，相对路径与绝对路径必须判出同一个宿主生态。
        // 真实踩到过：老库是 `..\vendor\…`（80 个有宿主），换绝对扫描根后
        // 只剩 35 个——同一个字段两条写入路径给出不同答案。
        let rel = "..\\vendor\\skill\\zion-nocode\\2.7.5\\.qoder-plugin\\plugin.json";
        let abs = "D:\\work\\capability-core\\vendor\\skill\\zion-nocode\\2.7.5\\.qoder-plugin\\plugin.json";
        assert_eq!(
            classify(rel, &[]).origin_agent,
            classify(abs, &[]).origin_agent,
            "扫描根形态不能影响 origin_agent"
        );
        assert_eq!(classify(abs, &[]).origin_agent, "qoder");
    }

    #[test]
    fn package_hint_still_works_for_absolute_paths() {
        // 绝对路径也要能命中包名关键词（segs[3] 坐标归一化后才成立）。
        let abs = "D:\\work\\capability-core\\vendor\\plugin\\obsidian-suite\\x.json";
        assert_eq!(classify(abs, &[]).origin_agent, "obsidian");
    }

    #[test]
    fn unassigned_ecosystem_becomes_empty_not_a_placeholder() {
        let p = classify("..\\vendor\\skill\\product-design\\0.5.10\\SKILL.md", &[]);
        assert_eq!(p.origin_agent, "", "判不出来必须是空串，不能写占位值");
    }

    #[test]
    fn ecosystem_is_recorded_when_path_declares_a_host() {
        let p = classify("..\\vendor\\skill\\zion-nocode\\2.7.5\\.qoder-plugin\\plugin.json", &[]);
        assert_eq!(p.origin_agent, "qoder");
    }

    #[test]
    fn legacy_manifest_summaries_are_read_back_verbatim() {
        // 迁移回填只能拿到 manifest；这三条前缀是旧解析器写死的格式。
        let mut m = ac_core::CapabilityManifest::new("mcp::x", ac_core::AssetKind::Mcp, "x");
        m.provenance.source_path = Some("..\\vendor\\mcp\\x\\.mcp.json".into());
        m.provides.push(ac_core::ProvidedCapability {
            capability_id: "mcp::x::s".into(),
            name: "s".into(),
            summary: "stdio server，启动命令 D:/venv/Scripts/python.exe".into(),
            input_schema: None,
            tags: vec!["tool".into()],
        });
        let p = classify_manifest(&m);
        assert_eq!(p.registration_kind, "mounted");
    }

    #[test]
    fn runtime_refs_take_precedence_over_summary_text() {
        let mut m = ac_core::CapabilityManifest::new("mcp::y", ac_core::AssetKind::Mcp, "y");
        m.provenance.source_path = Some("..\\vendor\\mcp\\y\\.mcp.json".into());
        m.provenance.runtime_refs = refs(&["uvx"]);
        m.provides.push(ac_core::ProvidedCapability {
            capability_id: "mcp::y::s".into(),
            name: "s".into(),
            summary: "stdio server，启动命令 D:/stale/path".into(),
            input_schema: None,
            tags: vec![],
        });
        let p = classify_manifest(&m);
        assert_eq!(
            p.registration_kind, "embedded",
            "结构化字段存在时不再去解析人话"
        );
    }
}
