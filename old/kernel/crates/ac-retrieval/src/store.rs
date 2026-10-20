//! 结构化存储抽象（trait 定义侧）。
//!
//! # 依赖方向：本文件是「谁定义 trait」这件事的答案
//!
//! 设计文档「### 2. 模块划分与解耦纪律」与硬性约束 #2 规定：
//!
//!   ac-retrieval 不直接依赖 ac-registry —— 二者通过 trait 通信。
//!   耦合点清单：registry → retrieval | AssetStore trait | retrieval 定义 trait，registry 实现
//!
//! 所以方向是 **ac-retrieval 定义 AssetStore，ac-registry 依赖 ac-retrieval 并实现它**。
//! 本文件因此定义 trait 本身，以及 trait 签名里用到的**所有**数据形状
//! （AssetFilter / AssetRow）。这些形状必须活在 ac-retrieval（或 ac-core）里，
//! **绝不能**引用 ac-registry 的类型——否则 trait 的定义方反而被实现方绑住了，
//! 依赖方向就反了。
//!
//! 这一点由 Cargo.toml 里没有 ac-registry 依赖 + 本文件的守门测试守住。

use ac_core::Authority;

/// 一条供检索层使用的资产行。
///
/// 刻意做成**检索层自己的最小视图**，而不是直接复用 ac_registry::AssetSummary：
/// 后者是 UI 列表专用形状（含 authority_label 之类展示字段）。让 trait 返回
/// UI 类型会把 ac-registry 拖成 trait 的必要依赖，依赖方向就反了。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetRow {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub authority: String,
    /// 标签全集（去重后）。用于结果展示，不用于过滤（过滤下推给 store）。
    pub tags: Vec<String>,
    /// 可检索的纯文本（name + description + capability 摘要拼接而成）。
    pub text: String,
    /// 所属逻辑资产（M1.5）。内容相同的多份在检索层按逻辑资产看待。
    pub logical_id: Option<String>,
    /// 同一逻辑资产的磁盘位置数。
    pub location_count: u32,
}

impl AssetRow {
    /// 便捷构造：给测试与降级兜底用。
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        name: impl Into<String>,
        authority: Authority,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            name: name.into(),
            description: String::new(),
            authority: authority.as_str().to_string(),
            tags: Vec::new(),
            text: String::new(),
            logical_id: None,
            location_count: 1,
        }
    }
}

/// 结构化过滤条件。
///
/// **语义全部照搬 ac-registry 里已在 M1 证明过的那一套**（见该 crate 的
/// TAG_SEMANTICS 与 query_assets）：
///
/// - kind：等值；
/// - tag：**单值 + OR 语义**（任一能力带该标签即命中），SQL 用
///   EXISTS + json_each 而非 JOIN（否则一条多能力资产会重复出现）；
/// - min_authority：按**枚举序**（read < observe < write < destructive）比较，
///   不是字典序；
/// - text：对可检索纯文本做包含匹配（内部转义 LIKE 通配符）。
///
/// 检索层只**描述**条件，不做 SQL 文本拼装——那正是 ac-registry 的职责。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetFilter {
    /// 按资产类型过滤（skill / plugin / mcp / …）。
    pub kind: Option<String>,
    /// 最低权限要求：只返回 authority 不低于该值的资产。
    pub min_authority: Option<Authority>,
    /// 按标签过滤：任一能力带该标签即命中。
    pub tag: Option<String>,
    /// 对可检索文本做包含匹配。
    pub text: Option<String>,
}

impl AssetFilter {
    /// 全量（无过滤）。
    pub fn all() -> Self {
        Self::default()
    }

    /// 该过滤是否会**收窄**结果。用于健康检查与自检展示。
    pub fn is_empty(&self) -> bool {
        self.kind.is_none()
            && self.min_authority.is_none()
            && self.tag.is_none()
            && self.text.is_none()
    }
}


// ─────────────────────────────────────────────────────────────
// M2 第二刀：全文索引输入源 + 候选集取行
// ─────────────────────────────────────────────────────────────
//
// 下面三组方法都是为「BM25 召回 → 结构化过滤 → 精排」这条链路补的，缺一不可：
//
// - `index_source()`：索引器唯一的输入。**只读**，不写库、不改任何解析逻辑。
// - `fingerprint()`：一次快照的内容指纹，用来判「索引是否已过期」。
// - `try_query_in()`：把 BM25 召回的 id 集合换回**存储层自己的**结构化行。
//
// 为什么最后一条必须回到 store 去取，而不是让索引直接吐出可展示的行：
// 索引是**派生物**，可能过期。返回给用户的内容必须来自存储层这一份真相，
// 否则索引一过期，结果就会是「旧数据 + 报告说成功」——
// 正是 M2 第一刀验收里抓到的那个谎报 bug 的同一种形状。

/// 索引器的一条输入文档。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexEntry {
    /// 资产 id。与 `AssetRow::id` 同值，是索引与存储层之间的连接键。
    pub id: String,
    /// **变更信号**：内容指纹 + 解析器修订号拼成的字符串。
    ///
    /// 索引增量更新的**唯一**判据：`sig` 相同就认为「这条没变，一个字都不用重写」。
    /// 它刻意复用注册表自己已经在用的那对信号（`content_hash` + `parser_rev`），
    /// 于是「解析逻辑改了」与「文件内容改了」走的是同一条增量闸门，不会各说各话。
    ///
    /// 代价（明写在这里，不藏）：若有人改了 `search_blob` 的拼接方式却没 bump
    /// `PARSER_REV`，索引会以为没变而漏更新。这与 M1.5 已记录的硬规则是同一条——
    /// 索引没有引入新的失效模式，只是复用了已有的那条。
    pub sig: String,
    /// 真正进倒排的文本（对注册表实现而言就是 `search_blob`）。
    pub text: String,
}

/// 一次索引同步拿到的全部输入。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexSnapshot {
    pub entries: Vec<IndexEntry>,
}

impl IndexSnapshot {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// 条目按 id 排序后的内容指纹。**顺序无关**：同一批资产换个顺序，值不变。
    ///
    /// 索引构建完成时算一次存下来；下次挂载时重算一次比对——
    /// 不一致就说明「索引比库旧」。这条判据是 O(n) 的，所以只在**挂载时**算，
    /// 绝不放在每次查询的路径上（与 M2「P95 < 50ms」直接冲突）。
    pub fn fingerprint(&self) -> String {
        let mut pairs: Vec<String> =
            self.entries.iter().map(|e| format!("{}\x1f{}", e.id, e.sig)).collect();
        pairs.sort();
        let joined = pairs.join("\n");
        let mut h = <sha2::Sha256 as sha2::Digest>::new();
        sha2::Digest::update(&mut h, joined.as_bytes());
        format!("{:x}", sha2::Digest::finalize(h))
    }
}
/// 检索层访问资产存储所需的**全部**能力。
///
/// 这就是耦合点清单里那一条 AssetStore。语义刻意收得很窄：
/// 只要求「按结构化条件取一批资产」和「取总量」，
/// 全是同步、纯数据访问——具体后端（SQLite / 内存 / 远程）
/// 由实现方自己解决，检索层不关心。
///
/// 这也正是硬性约束里「检索算法要能脱离 SQLite 单独测试」的落点：
/// 拿一个内存 impl 就能单测检索逻辑，不碰任何数据库。
pub trait AssetStore {
    /// 按结构化过滤条件取资产行，按后端既定的稳定顺序返回。
    ///
    /// **这是有损视图**：签名里没有错误通道，后端一旦失败就只能退化成空列表，
    /// 而静默空列表会被上层读成「没有资产匹配你的过滤条件」——本项目最不能犯的错。
    /// 凡是分得清「空是因为没有、还是因为库坏了」的调用方，必须用
    /// [`AssetStore::try_query`]；流水线用的也正是它。
    fn query(&self, filter: &AssetFilter, limit: u32) -> Vec<AssetRow>;

    /// 结构化基线查询的**可失败**版本：语义与 [`AssetStore::query`] 相同，
    /// 但后端失败时如实返回 `Err`，绝不伪装成「成功且一条不匹配」。
    ///
    /// # 为什么 trait 上有两个 query
    ///
    /// 「永远不会失败」这条承诺属于**检索层**（`Searcher::search` 不返回
    /// `Result`），它**不覆盖后端本身**：SQLite 被锁住、文件损坏、路径不存在，
    /// 都是真实存在的失败。把它吞掉再报「查询成功」，等于谎报基线交付。
    ///
    /// 所以失败留在**这一层**：检索层拿到 `Err` 后把它记进
    /// `DegradationReport`（`Stage::Structured` = `Failed`），查询本身照样返回
    /// 结果——「错误不冒泡」与「状态不说谎」同时成立。
    ///
    /// # 默认实现为什么是 `Ok(self.query(...))`
    ///
    /// 它只对**查询确实不会失败**的后端成立（内存后端）。保留默认实现是为了让
    /// 既有实现方无需改动即可编译；但**任何真正会失败的后端（任何 SQLite 实现）
    /// 都必须覆写本方法**，否则 `unwrap_or_default()` 式的静默吞错原样回来。
    fn try_query(&self, filter: &AssetFilter, limit: u32) -> Result<Vec<AssetRow>, String> {
        Ok(self.query(filter, limit))
    }


    /// 取出全文索引所需的全部条目（`id` + 变更信号 + 可检索文本）。
    ///
    /// **默认实现返回 `Err`**，不是返回空快照：
    /// 「这个后端不支持索引」与「支持但当前库里一条都没有」必须可区分，
    /// 否则一个未实现的 `index_source` 会让索引看起来「同步成功、0 条文档」，
    /// 然后全文检索理直气壮地返回空结果并报告就绪——又是一次谎报成功。
    fn index_source(&self) -> Result<IndexSnapshot, String> {
        Err("该存储后端未实现 index_source，无法建立全文索引".into())
    }

    /// 按 BM25 召回的 id 集合取回**结构化行**，并叠加其余结构化维度
    /// （kind / tag / min_authority）。返回顺序**由 `ids` 决定**。
    ///
    /// 这就是链路里的「结构化过滤」一步：全文层只负责**召回与打分**，
    /// 过滤与字段回填一律回到存储层，保证过滤语义与桌面端列表是同一段逻辑，
    /// 也保证返回的行永远是存储层此刻的真相，而不是索引里的旧副本。
    ///
    /// # 默认实现为什么可行、什么时候会慢
    ///
    /// 默认实现是「先按 filter 取一批，再按 ids 过滤重排」——语义正确，
    /// 但对 SQLite 这是一次**全量**扫描。因此真正会失败的/真正大的后端
    /// 应当覆写它（注册表实现走的是 `id IN (...)` + 主键）。
    ///
    /// # 契约：`filter.text` 由调用方清空
    ///
    /// 本方法只管结构化维度。文本匹配是召回层的事，调用方**必须**先把
    /// `filter.text` 置空再传进来；默认实现照做，实现方不必重复判断。
    fn try_query_in(
        &self,
        filter: &AssetFilter,
        ids: &[String],
        limit: u32,
    ) -> Result<Vec<AssetRow>, String> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let want: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        let mut scan = filter.clone();
        scan.text = None;
        let rows = self.try_query(&scan, ids.len().max(limit as usize) as u32)?;
        let mut picked: Vec<AssetRow> = rows
            .into_iter()
            .filter(|r| want.contains(r.id.as_str()))
            .collect();
        // 按 ids 的给定顺序重排：调用方靠顺序表达「谁更相关」。
        picked.sort_by_key(|r| ids.iter().position(|i| *i == r.id).unwrap_or(usize::MAX));
        picked.truncate(limit as usize);
        Ok(picked)
    }
    /// 满足条件的总条数（不受 limit 影响）。
    ///
    /// 同样是**有损视图**：查询失败时返回 0（见上）。要区分「真的是 0 条」与
    /// 「数不出来」，先用 [`AssetStore::is_healthy`]。
    fn count(&self, filter: &AssetFilter) -> u32;

    /// 存储是否可用。健康检查用；结构化基线路径为真时，
    /// 检索永远能返回结果。
    fn is_healthy(&self) -> bool {
        true
    }
}

/// 任意 store 的共享引用自动也是 store。
///
/// 常见用法是把 `&Registry`（或任何后端的 &T）直接交给检索引擎，
/// 免去调用方为了借用而克隆整个后端。
impl<T: AssetStore + ?Sized> AssetStore for &T {
    fn query(&self, filter: &AssetFilter, limit: u32) -> Vec<AssetRow> {
        (**self).query(filter, limit)
    }
    /// **必须转发**：若不转发，这里会落回默认实现 `Ok(self.query(..))`，
    /// 而 `query` 是有损视图——于是 `&Registry` 又会静默吞掉 SQL 错误，
    /// 流水线拿到的仍是「成功的空结果」。这正是本轮修掉的那个 bug。
    fn try_query(&self, filter: &AssetFilter, limit: u32) -> Result<Vec<AssetRow>, String> {
        (**self).try_query(filter, limit)
    }
    fn count(&self, filter: &AssetFilter) -> u32 {
        (**self).count(filter)
    }
    fn is_healthy(&self) -> bool {
        (**self).is_healthy()
    }
    /// **同样必须转发**：不转发的话 `&Registry` 会落回 trait 的默认实现，
    /// 而那个默认实现是**返回 Err**（见上）。于是最常见的装配写法
    /// （`RetrievalEngine::structured_only(&registry)`）全文索引永远建不起来，
    /// 且故障原因会是一条指向「后端未实现」的假话。
    fn index_source(&self) -> Result<IndexSnapshot, String> {
        (**self).index_source()
    }
    fn try_query_in(
        &self,
        filter: &AssetFilter,
        ids: &[String],
        limit: u32,
    ) -> Result<Vec<AssetRow>, String> {
        (**self).try_query_in(filter, ids, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_row_new_sets_core_fields() {
        let r = AssetRow::new("a::b", "skill", "名字", Authority::Write);
        assert_eq!(r.id, "a::b");
        assert_eq!(r.kind, "skill");
        assert_eq!(r.authority, "write");
    }

    #[test]
    fn empty_filter_is_reported_empty() {
        assert!(AssetFilter::all().is_empty());
        let mut f = AssetFilter::all();
        f.kind = Some("skill".into());
        assert!(!f.is_empty());
    }

    fn entry(id: &str, sig: &str) -> IndexEntry {
        IndexEntry {
            id: id.into(),
            sig: sig.into(),
            text: format!("{id} 的可检索文本"),
        }
    }

    #[test]
    fn fingerprint_is_order_independent_but_content_sensitive() {
        // 顺序无关：同一批资产换个返回顺序，指纹必须一样——否则每次扫描
        // 都会因为「顺序碰巧不同」而被判成索引过期，退化成全量重建。
        let a = IndexSnapshot {
            entries: vec![entry("s::a", "h1|4"), entry("s::b", "h2|4")],
        };
        let b = IndexSnapshot {
            entries: vec![entry("s::b", "h2|4"), entry("s::a", "h1|4")],
        };
        assert_eq!(a.fingerprint(), b.fingerprint());

        // 内容敏感：改 sig（=解析器修订号或文件内容变了）必须变。
        // 变异敏感：把 format!("{id} {sig}") 写成只取 id，本条立刻红。
        let c = IndexSnapshot {
            entries: vec![entry("s::a", "h1|4"), entry("s::b", "h2|5")],
        };
        assert_ne!(a.fingerprint(), c.fingerprint());

        // 增删也必须变。
        let d = IndexSnapshot {
            entries: vec![entry("s::a", "h1|4")],
        };
        assert_ne!(a.fingerprint(), d.fingerprint());
        assert_ne!(d.fingerprint(), IndexSnapshot::default().fingerprint());
    }

    #[test]
    fn index_source_defaults_to_err_not_to_an_empty_snapshot() {
        // 一个什么都实现了、唯独没实现 index_source 的后端：
        // 拿到 Err（「不支持」）而不是 Ok(空)（「支持但库里没东西」）。
        // 后者会让索引「同步成功、0 条」，然后全文检索返回空并报告就绪。
        struct Bare;
        impl AssetStore for Bare {
            fn query(&self, _f: &AssetFilter, _l: u32) -> Vec<AssetRow> {
                Vec::new()
            }
            fn count(&self, _f: &AssetFilter) -> u32 {
                0
            }
        }
        let e = Bare.index_source().unwrap_err();
        assert!(
            e.contains("index_source"),
            "错误原因必须指向「未实现」而不是「库里没有」，实际：{e}"
        );
    }

    #[test]
    fn try_query_in_default_impl_filters_by_ids_and_keeps_given_order() {
        let rows = vec![
            AssetRow::new("s::a", "skill", "甲", Authority::Read),
            AssetRow::new("s::b", "skill", "乙", Authority::Read),
            AssetRow::new("s::c", "skill", "丙", Authority::Read),
        ];
        let store = crate::memstore::InMemoryStore::new(rows);
        // 调用方给的顺序即「相关性顺序」，必须原样保留。
        let ids = vec!["s::c".to_string(), "s::a".to_string()];
        let got = store.try_query_in(&AssetFilter::all(), &ids, 10).unwrap();
        assert_eq!(
            got.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["s::c", "s::a"]
        );

        // limit 必须真的截断。
        let got = store.try_query_in(&AssetFilter::all(), &ids, 1).unwrap();
        assert_eq!(got.len(), 1);

        // 空 ids 不得退化成「全表返回」。
        assert!(store.try_query_in(&AssetFilter::all(), &[], 10).unwrap().is_empty());
    }

    #[test]
    fn try_query_in_default_impl_still_applies_structured_filters() {
        let mut tagged = AssetRow::new("s::a", "skill", "甲", Authority::Read);
        tagged.tags = vec!["read_only".into()];
        let plain = AssetRow::new("s::b", "skill", "乙", Authority::Read);
        let store = crate::memstore::InMemoryStore::new(vec![tagged, plain]);
        let mut f = AssetFilter::all();
        f.tag = Some("read_only".into());
        let ids = vec!["s::b".to_string(), "s::a".to_string()];
        let got = store.try_query_in(&f, &ids, 10).unwrap();
        assert_eq!(
            got.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["s::a"],
            "结构化过滤必须仍然生效——全文层只负责召回，不负责放行"
        );
    }
}