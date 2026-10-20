//! 资产作者与「作者出处」判定。
//!
//! # 用户原话与它要的东西
//!
//! 「建立相应依赖来表明这些能力的作者」——关键词是**依赖**：作者不能是
//! 一个凭空推断出来的字符串，它必须带着「这句话是从哪来的」一起落库。
//! 所以这里判定的结果不是 `author: String`，而是 `(author, author_source)`：
//!
//! | author_source | 证据 | 可信度 |
//! |---|---|---|
//! | `manifest`    | plugin.json 的 `author` 字段（作者自己声明） | 最高 |
//! | `frontmatter` | SKILL.md 的 `author:` 键（作者自己声明） | 高 |
//! | `package`     | 包路径 `vendor/<kind>/<pkg>/…` 里的 `<pkg>` | 只表明「哪个包」 |
//! | `''` 空串     | 判不出来 | —— |
//!
//! # 为什么不用占位值
//!
//! 空串就是空串，**绝不写 `unknown` / `anonymous`**：占位值会被当成
//! 一个真实的作者名展示出来，而「不知道」和「叫 unknown 的人」在界面上
//! 长得一样，这正是本项目反复强调不能出现的那类谎报。
//!
//! # 为什么判定必须是一个函数
//!
//! 迁移回填（老库没有新列）、解析入库（新扫描）、以及测试断言三处必须是
//! 同一套规则。散成三段 SQL/if-else 之后，同一份资产在库里和界面上的
//! 作者迟早会不一样——那是最难排查的一类故障（见 `origin` 模块的同款教训）。
//!
//! # 包路径推断为什么只认 vendor / assets
//!
//! 真实库里的 506 条 source_path 全部长这样：
//!
//! - `D:\…\capability-core\vendor\skill\matlab\SKILL.md` → 包 `matlab`
//! - `D:\…\capability-core\vendor\plugin\obsidian-suite\.qoder-plugin\plugin.json` → 包 `obsidian-suite`
//!
//! 即 `<root>/<vendor|assets>/<kind>/<pkg>/…`。别的目录（比如 `assets/`
//! 之外的任意深路径）**不推断**：从 `/tmp/x/SKILL.md` 里编一个作者出来，
//! 比不显示作者有害得多。

use ac_core::CapabilityManifest;

/// 作者来自 manifest（plugin.json 的 author 字段）。
pub const SOURCE_MANIFEST: &str = "manifest";
/// 作者来自 SKILL.md 的 frontmatter。
pub const SOURCE_FRONTMATTER: &str = "frontmatter";
/// 作者从包路径推断（只表明是哪个包，不是人）。
pub const SOURCE_PACKAGE: &str = "package";
/// 判不出来：空串，不是占位值。
pub const SOURCE_NONE: &str = "";

/// 一次作者判定的完整结果。两个字段对应 assets 表的两列。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorProfile {
    /// 作者名；判不出来是**空串**（不是 `unknown`）。
    pub author: String,
    /// 出处：manifest / frontmatter / package / 空串（见本模块文档的表格）。
    pub source: String,
}

impl AuthorProfile {
    pub fn is_empty(&self) -> bool {
        self.author.is_empty()
    }
}

/// 只看 manifest 里作者自己声明的那一份。
///
/// `provenance.author_source` 决定算 `manifest` 还是 `frontmatter`：
/// 同一个「自声明」在两条解析路径上出处不同，界面要按档标注
/// （作者（自声明）/ 作者（frontmatter））。
pub fn declared_author(m: &CapabilityManifest) -> AuthorProfile {
    let author = m
        .provenance
        .author
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    if author.is_empty() {
        return AuthorProfile::default();
    }
    let source = match m.provenance.author_source.as_deref() {
        Some(SOURCE_FRONTMATTER) => SOURCE_FRONTMATTER,
        _ => SOURCE_MANIFEST,
    };
    AuthorProfile {
        author: author.to_string(),
        source: source.to_string(),
    }
}

/// 完整判定：**自声明优先，包路径兜底**。
///
/// 顺序不是可调的：作者自己写在清单里的名字永远比从目录名猜出来的准。
/// 兜底只在没有任何自声明时发生，且出处如实记成 `package`。
pub fn infer_author(m: &CapabilityManifest, source_path: &str) -> AuthorProfile {
    let declared = declared_author(m);
    if !declared.is_empty() {
        return declared;
    }
    match package_of(source_path) {
        Some(pkg) => AuthorProfile {
            author: pkg,
            source: SOURCE_PACKAGE.to_string(),
        },
        None => AuthorProfile::default(),
    }
}

/// 从 `<root>/<vendor|assets>/<kind>/<pkg>/…` 里取出 `<pkg>`。
///
/// 分隔符与大小写都归一化（真实库是 Windows 反斜杠；测试里常写正斜杠），
/// 与 `origin` 模块的处理保持一致——同一份资产不能因为路径写法不同
/// 得到两个作者。
pub fn package_of(source_path: &str) -> Option<String> {
    let norm = source_path.replace('\\', "/");
    let segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    let idx = segs.iter().position(|s| {
        let l = s.to_ascii_lowercase();
        l == "vendor" || l == "assets"
    })?;
    // 坐标固定：root 后第一段是 kind，第二段才是包名。越界直接判无。
    let pkg = *segs.get(idx + 2)?;
    if !plausible_package(pkg) {
        return None;
    }
    Some(pkg.to_string())
}

/// 这一段像不像一个包名（而不是版本目录 / 隐藏目录 / 分隔符）。
fn plausible_package(seg: &str) -> bool {
    let s = seg.trim();
    if s.len() < 2 {
        return false;
    }
    // 隐藏目录（.qoder-plugin / .plugin / .tmp）是标记，不是包名。
    if s.starts_with('.') {
        return false;
    }
    let l = s.to_ascii_lowercase();
    if l == "vendor" || l == "assets" || l == "root" {
        return false;
    }
    // 纯版本号目录（0.0.5 / 1.2.3 / 2）不是包名。
    // 判据：至少有一个字母。真实包名全是字母开头（含中文）。
    s.chars().any(|c| c.is_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::{AssetKind, CapabilityManifest};

    fn manifest_with_author(author: Option<&str>, source: Option<&str>) -> CapabilityManifest {
        let mut m = CapabilityManifest::new("plugin::x", AssetKind::Plugin, "x");
        m.provenance.author = author.map(|s| s.to_string());
        m.provenance.author_source = source.map(|s| s.to_string());
        m
    }

    #[test]
    fn declared_manifest_author_wins_over_package() {
        // 作者自己声明的名字永远比从目录名猜的准。
        let m = manifest_with_author(Some("Max Zheng"), Some(SOURCE_MANIFEST));
        let p = infer_author(&m, "vendor/skill/qgraphflow/0.0.5/.qoder-plugin/plugin.json");
        assert_eq!(p.author, "Max Zheng");
        assert_eq!(p.source, SOURCE_MANIFEST);
    }

    #[test]
    fn frontmatter_author_is_reported_as_its_own_source() {
        // 同一个「自声明」在两条解析路径上出处不同，界面按档标注。
        let m = manifest_with_author(Some("MathWorks"), Some(SOURCE_FRONTMATTER));
        let p = infer_author(&m, "vendor/skill/matlab/SKILL.md");
        assert_eq!(p.author, "MathWorks");
        assert_eq!(p.source, SOURCE_FRONTMATTER);
    }

    #[test]
    fn package_is_the_fallback_and_says_so() {
        let m = manifest_with_author(None, None);
        let p = infer_author(&m, "vendor/plugin/obsidian-suite/.zcode-plugin/plugin.json");
        assert_eq!(p.author, "obsidian-suite");
        assert_eq!(p.source, SOURCE_PACKAGE, "兜底必须自报是包推断");
    }

    #[test]
    fn empty_author_source_is_empty_not_unknown() {
        // 铁律：判不出来是空串。写 'unknown' 会让「不知道」和
        // 「一个叫 unknown 的作者」在界面上长得一样。
        let m = manifest_with_author(None, None);
        let p = infer_author(&m, "/tmp/whatever/SKILL.md");
        assert_eq!(p.author, "");
        assert_eq!(p.source, "");
        assert!(p.is_empty());
    }

    #[test]
    fn backslash_and_forward_slash_agree() {
        let m = manifest_with_author(None, None);
        let a = infer_author(&m, "vendor/skill/matlab/SKILL.md");
        let b = infer_author(&m, "..\\vendor\\skill\\matlab\\SKILL.md");
        assert_eq!(a, b);
        assert_eq!(a.author, "matlab");
    }

    #[test]
    fn version_directory_is_not_a_package_name() {
        // vendor/skill/<pkg>/<version>/… 里 pkg 在版本号前面；
        // 若路径少了 pkg 一段，宁可判不出来也不要把 "0.0.5" 当作者。
        assert_eq!(package_of("vendor/skill/0.0.5/SKILL.md"), None);
        assert_eq!(package_of("vendor/skill/0.0.5"), None);
    }

    #[test]
    fn hidden_and_short_segments_are_rejected() {
        assert_eq!(package_of("vendor/skill/.qoder-plugin/plugin.json"), None);
        assert_eq!(package_of("vendor/skill/x/SKILL.md"), None, "单字符不算包名");
        assert_eq!(package_of("vendor/skill/ab/SKILL.md").as_deref(), Some("ab"));
    }

    #[test]
    fn assets_paths_use_the_same_coordinate() {
        // assets/<kind>/<id>/… 与 vendor/<kind>/<pkg>/… 同一个坐标。
        assert_eq!(package_of("assets/mcp/obsidian-suite/mcp.json").as_deref(), Some("obsidian-suite"));
        assert_eq!(package_of("assets/skill/my-skill/SKILL.md").as_deref(), Some("my-skill"));
    }

    #[test]
    fn paths_without_vendor_or_assets_segment_are_not_guessed() {
        // 真实场景：用户把某份 skill 直接放在任意目录里扫描。
        assert_eq!(package_of("D:/scratch/some-skill/SKILL.md"), None);
        assert_eq!(package_of("some-skill/SKILL.md"), None);
        assert_eq!(package_of(""), None);
    }

    #[test]
    fn vendor_must_be_a_whole_segment() {
        // 回归：按字符串包含判会把 vendorized-notes/ 误判成厂商包。
        assert_eq!(package_of("vendorized-notes/a/SKILL.md"), None);
    }
}
