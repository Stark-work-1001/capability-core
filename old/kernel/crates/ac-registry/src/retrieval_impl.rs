//! AssetStore trait 的 registry 侧实现。
//!
//! 方向来自设计文档耦合点清单：
//!
//!   registry -> retrieval | AssetStore trait | retrieval 定义 trait，registry 实现
//!
//! 本文件只做适配，不重新实现任何 SQL：把检索层的 AssetFilter 翻译成本 crate
//! 早已在 M1 证明过的那套 AssetQuery，直接复用 query_assets / count_assets。
//! 因此 kind / tag / min_authority / text 四维的语义天然与桌面端列表完全一致——
//! 它们走的是同一段 SQL。
//!
//! PARSER_REV 未 bump：本文件只加 trait 实现与类型转换，没有碰任何
//! parse / infer / fingerprint 逻辑，按 M1.5 记录的规则不需要 +1。

use crate::store::{AssetQuery, AssetSummary, Registry};
use ac_retrieval::store::{AssetFilter, AssetRow, AssetStore, IndexEntry, IndexSnapshot};
use rusqlite::params;

/// 检索层过滤 -> registry 已有的列表查询条件。
///
/// 整套适配的核心：四个字段一对一映射，不做任何语义改写。
/// 空串当作「没这个条件」，与桌面端 ListQuery 的处理一致。
pub fn to_asset_query(f: &AssetFilter, limit: u32) -> AssetQuery {
    AssetQuery {
        kind: non_empty(f.kind.clone()),
        min_authority: f.min_authority.map(|a| a.as_str().to_string()),
        tag: non_empty(f.tag.clone()),
        text: non_empty(f.text.clone()),
        limit,
        offset: 0,
        // 检索层默认**不检出归档资产**：搜到一个被用户删掉的能力，
        // 点进去才发现是 archived，是比搜不到更糟的体验。
        // 要包含归档由调用方显式置 true（见 AssetQuery 的字段文档）。
        include_archived: false,
    }
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|x| !x.trim().is_empty())
}

impl Registry {
    /// 取一个资产的标签全集（去重）。
    ///
    /// AssetSummary 是 UI 列表形状，不带标签；检索层的 AssetRow 需要。
    /// 这里用与 all_tags 完全相同的 json_each 展开写一次，保证「结果里回显的标签」
    /// 与「能被标签筛中的标签」出自同一条路径。
    pub fn tags_of_asset(&self, asset_id: &str) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT t.value FROM capabilities c, json_each(c.tags) t
                 WHERE c.asset_id = ?1 AND t.value IS NOT NULL AND t.value <> ''
                 ORDER BY t.value",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![asset_id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }
}

/// AssetSummary -> AssetRow（检索层最小视图）。
fn to_asset_row(s: &AssetSummary, tags: Vec<String>) -> AssetRow {
    let text = format!("{} {}", s.name, s.description);
    AssetRow {
        id: s.id.clone(),
        kind: s.kind.clone(),
        name: s.name.clone(),
        description: s.description.clone(),
        authority: s.authority.clone(),
        tags,
        text,
        logical_id: s.logical_id.clone(),
        location_count: s.location_count,
    }
}

impl Registry {
    /// 批量取资产的 `source_path`（id -> path）。
    ///
    /// 用途：ac-mcp 的对外接口要按**生态**收窄，而生态在库里没有列——
    /// 它是由 `source_path` 推断的（`agents::infer_ecosystem`，
    /// C 通道静态归属用的就是这条规则）。因此对外行必须带上源路径。
    ///
    /// 一次查询 `IN (...)`，而不是每个 id 查一次：调用方的批次
    /// 上限是 200，逐行查会把一次列表查询的耗时放大 200 倍。
    /// 路径为空的行**不**进映射（调用方据此判定「unassigned」）。
    pub fn source_paths_of(&self, ids: &[String]) -> Result<std::collections::HashMap<String, String>, String> {
        if ids.is_empty() {
            return Ok(Default::default());
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, source_path FROM assets
                 WHERE id IN (SELECT value FROM json_each(?1))",
            )
            .map_err(|e| e.to_string())?;
        let json = serde_json::to_string(ids).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![json], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut out = std::collections::HashMap::new();
        for r in rows {
            let (id, path) = r.map_err(|e| e.to_string())?;
            if let Some(p) = path.filter(|p| !p.trim().is_empty()) {
                out.insert(id, p);
            }
        }
        Ok(out)
    }
}

impl AssetStore for Registry {
    /// 真正会失败的那个入口：SQL 报错就如实上报，绝不退化成空列表。
    ///
    /// # 修复记录：这里曾经 `unwrap_or_default()`
    ///
    /// 旧写法是 `self.query_assets(&aq).unwrap_or_default()`。库被锁住、文件损坏、
    /// 表不见了，结果都是**空 Vec 且没有任何错误**——流水线上层读到的是
    /// 「结构化基线成功，0 条命中」，与「你的过滤条件没匹配到东西」无法区分。
    /// 对一个核心承诺是「永远返回结果」的检索层来说，这是最坏的失败模式：
    /// **谎报成功**。
    ///
    /// 现在失败沿 `Result` 上抛，由 `RetrievalEngine::query_baseline` 接住并
    /// 记成 `Stage::Structured = Failed`。**错误不冒泡到调用方，状态不说谎**，
    /// 两条同时成立。
    fn try_query(&self, filter: &AssetFilter, limit: u32) -> Result<Vec<AssetRow>, String> {
        let aq = to_asset_query(filter, limit);
        let summaries = self.query_assets(&aq)?;
        Ok(summaries
            .iter()
            .map(|s| {
                // 单个资产的标签取不到不该毁掉整批结果：空标签继续返回。
                // 这是**逐行**的部分结果降级（标签只是展示字段），与上面
                // 「整批查询失败」不是一回事，故意的：库里这一行还在。
                let tags = self.tags_of_asset(&s.id).unwrap_or_default();
                to_asset_row(s, tags)
            })
            .collect())
    }

    fn query(&self, filter: &AssetFilter, limit: u32) -> Vec<AssetRow> {
        // 刻意保留的**有损视图**：`AssetStore::query` 的签名没有错误通道。
        // 分得清「空是因为没有」还是「因为库坏了」的调用方请用 `try_query()`，
        // 检索流水线用的正是它。
        self.try_query(filter, limit).unwrap_or_default()
    }

    fn count(&self, filter: &AssetFilter) -> u32 {
        let aq = to_asset_query(filter, 1000);
        // 同样是有损的：数不出来时记 0。要区分请先问 `is_healthy()`。
        self.count_assets(&aq).unwrap_or(0)
    }

    // ── M2 第二刀：全文索引的输入源 + 候选集取行 ──────────────────
    //
    // PARSER_REV 未 bump（本轮只加读取与召回，没碰 parse / infer / fingerprint）。

    /// 取出全文索引的输入：一条 SQL 拿到 (id, content_hash, parser_rev, search_blob)。
    ///
    /// `sig = content_hash | parser_rev`——**刻意复用注册表自己的增量闸门信号**：
    /// 「文件内容变了」与「解析逻辑变了」走的是同一条判定，于是全文索引
    /// 不会和注册表对「什么算变了」各持一套说法。
    ///
    /// 一条 SQL 而不是去 JOIN capabilities 重新拼：search_blob 在入库时
    /// （`build_search_blob`）就已把 id / name / description / identity /
    /// 能力摘要 / 标签拼好，这里重拼会引入第二份可能与入库逻辑漂移的副本。
    fn index_source(&self) -> Result<IndexSnapshot, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, COALESCE(content_hash, ''), COALESCE(parser_rev, 0), search_blob
                 FROM assets ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                let hash: String = r.get(1)?;
                let rev: i64 = r.get(2)?;
                let text: String = r.get(3)?;
                Ok(IndexEntry {
                    id,
                    sig: format!("{hash}|{rev}"),
                    text,
                })
            })
            .map_err(|e| e.to_string())?;
        let entries = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(IndexSnapshot { entries })
    }

    /// 按 BM25 召回的 id 集合取回结构化行，并叠加 kind / tag / min_authority。
    ///
    /// WHERE 条件**逐条镜像** `query_assets`（那里有注释说明两处必须一致，
    /// 否则分页与结果会对不上）。漂移由
    /// `try_query_in_agrees_with_query_assets` 盯着。
    ///
    /// ids 按 CHUNK 切块：SQLite 对绑定变量个数有上限（老版本 999、新版本 32766），
    /// 一次性塞几千个进去会在换版本时突然炸掉。分块后每块都是主键 IN 查询。
    fn try_query_in(
        &self,
        filter: &AssetFilter,
        ids: &[String],
        limit: u32,
    ) -> Result<Vec<AssetRow>, String> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let min_rank: Option<i64> = filter.min_authority.map(|a| match a {
            ac_core::Authority::Read => 0,
            ac_core::Authority::Observe => 1,
            ac_core::Authority::Write => 2,
            ac_core::Authority::Destructive => 3,
        });
        let pattern = filter
            .text
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .map(|t| format!("%{}%", t));
        let kind = filter.kind.clone().filter(|k| !k.trim().is_empty());
        let tag = filter.tag.clone().filter(|t| !t.trim().is_empty());

        const CHUNK: usize = 400;
        let mut out: Vec<AssetRow> = Vec::new();
        for chunk in ids.chunks(CHUNK) {
            // **参数编号必须从分块长度往后排。**
            // 匿名占位符 `?` 在 SQLite 里会自动占用 ?1..?N；
            // 若过滤条件仍写成 ?1..?4，?1 指的就是第一个 id 而不是 kind，
            // 症状是「参数个数对不上」——这类错必须靠测试逼出来，
            // 因为它在**没填满过滤条件**时根本不报错。
            let n = chunk.len();
            let placeholders: Vec<String> = (1..=n).map(|i| format!("?{i}")).collect();
            let (k, p, m, g) = (n + 1, n + 2, n + 3, n + 4);
            let sql = format!(
                "SELECT a.id, a.kind, a.name, a.description, a.authority
                 FROM assets a
                 WHERE a.id IN ({})
                   AND (?{k} IS NULL OR a.kind = ?{k})
                   AND (?{p} IS NULL OR a.search_blob LIKE ?{p} ESCAPE '\\')
                   AND (?{m} IS NULL OR
                        CASE a.authority
                          WHEN 'read' THEN 0 WHEN 'observe' THEN 1
                          WHEN 'write' THEN 2 ELSE 3 END >= ?{m})
                   AND (?{g} IS NULL OR EXISTS (
                        SELECT 1 FROM capabilities c2, json_each(c2.tags) t
                        WHERE c2.asset_id = a.id AND t.value = ?{g}))",
                placeholders.join(",")
            );
            let mut sql_params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
            sql_params.push(&kind);
            sql_params.push(&pattern);
            sql_params.push(&min_rank);
            sql_params.push(&tag);
            let mut stmt = self.conn.prepare(&sql).map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(sql_params.as_slice(), |r| {
                    Ok(AssetRow {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        name: r.get(2)?,
                        description: r.get(3)?,
                        authority: r.get(4)?,
                        tags: Vec::new(),
                        text: String::new(),
                        logical_id: None,
                        location_count: 1,
                    })
                })
                .map_err(|e| e.to_string())?;
            for r in rows {
                let mut row = r.map_err(|e| e.to_string())?;
                // 单行标签取不到不该毁掉整批结果（与 try_query 同一处置）。
                row.tags = self.tags_of_asset(&row.id).unwrap_or_default();
                row.text = format!("{} {}", row.name, row.description);
                out.push(row);
            }
        }
        // 回到调用方给的顺序（= BM25 的相关性顺序），再截断。
        let mut picked: Vec<AssetRow> = out.into_iter().filter(|r| ids.contains(&r.id)).collect();
        picked.sort_by_key(|r| ids.iter().position(|i| *i == r.id).unwrap_or(usize::MAX));
        picked.truncate(limit as usize);
        Ok(picked)
    }

    fn is_healthy(&self) -> bool {
        // 「库能不能用」= 一条最简查询不报错。出错返回 false，
        // 让上层把它当故障，而不是静默给空结果。
        //
        // `RetrievalEngine::query_baseline` 在基线查询失败时会问这一位，
        // 用来区分「存储自认已不可用」与「查询单独失败」——两者处置不同。
        self.count_assets(&AssetQuery::default()).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::{AssetKind, Authority, CapabilityManifest, ProvidedCapability};
    // 仅测试用：把真库接进流水线，端到端验证「SQL 失败 → 基线报故障」。
    use ac_retrieval::{RetrievalEngine, SearchQuery};

    fn sample(id: &str, kind: AssetKind, name: &str, auth: Authority, tags: &[&str]) -> CapabilityManifest {
        let mut m = CapabilityManifest::new(id, kind, name);
        m.description = format!("{} 的描述", name);
        m.identity.authority = auth;
        m.provides.push(ProvidedCapability {
            capability_id: format!("{id}::cap"),
            name: "cap".into(),
            summary: "摘要".into(),
            input_schema: None,
            tags: tags.iter().map(|s| s.to_string()).collect(),
        });
        m.provenance.content_hash = Some(format!("hash-{id}"));
        m
    }

    fn demo_registry() -> Registry {
        let reg = Registry::open_in_memory().unwrap();
        reg.upsert(&sample("s::a", AssetKind::Skill, "画时序图", Authority::Read, &["read_only"])).unwrap();
        reg.upsert(&sample("s::b", AssetKind::Skill, "画流程图", Authority::Write, &["write"])).unwrap();
        reg.upsert(&sample("m::c", AssetKind::Mcp, "zotero检索", Authority::Read, &["Read", "read_only"])).unwrap();
        reg
    }

    #[test]
    fn store_is_healthy_after_open() {
        assert!(AssetStore::is_healthy(&demo_registry()));
    }

    #[test]
    fn trait_query_applies_kind_tag_authority_text() {
        let reg = demo_registry();

        let mut f = AssetFilter::all();
        f.kind = Some("mcp".into());
        let got: Vec<String> = reg.query(&f, 100).into_iter().map(|r| r.id).collect();
        assert_eq!(got, vec!["m::c"]);

        let mut f = AssetFilter::all();
        f.tag = Some("read_only".into());
        let mut ids: Vec<String> = reg.query(&f, 100).into_iter().map(|r| r.id).collect();
        ids.sort();
        assert_eq!(ids, vec!["m::c", "s::a"]);

        let mut f = AssetFilter::all();
        f.min_authority = Some(Authority::Write);
        let ids: Vec<String> = reg.query(&f, 100).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["s::b"]);

        let mut f = AssetFilter::all();
        f.text = Some("时序".into());
        let ids: Vec<String> = reg.query(&f, 100).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["s::a"]);
    }

    #[test]
    fn trait_count_matches_query_length_and_ignores_limit() {
        let reg = demo_registry();
        let mut f = AssetFilter::all();
        f.tag = Some("read_only".into());
        assert_eq!(reg.query(&f, 1).len(), 1);
        assert_eq!(reg.count(&f), 2, "count 不受 limit 影响");
    }

    #[test]
    fn returned_rows_carry_tags_and_text() {
        // 结果行要带回标签（供展示与将来向量匹配）与可携带文本。
        let reg = demo_registry();
        let rows = reg.query(&AssetFilter::all(), 100);
        let a = rows.iter().find(|r| r.id == "s::a").unwrap();
        assert!(a.tags.contains(&"read_only".to_string()));
        assert!(!a.text.is_empty(), "text 应至少带 name+description");
    }

    #[test]
    fn filter_to_query_mapping_is_one_to_one() {
        // 映射是纯函数：逐字段对照，防止某一维被悄悄丢掉。
        let f = AssetFilter {
            kind: Some("skill".into()),
            min_authority: Some(Authority::Write),
            tag: Some("read_only".into()),
            text: Some("图".into()),
        };
        let q = to_asset_query(&f, 50);
        assert_eq!(q.kind.as_deref(), Some("skill"));
        assert_eq!(q.min_authority.as_deref(), Some("write"));
        assert_eq!(q.tag.as_deref(), Some("read_only"));
        assert_eq!(q.text.as_deref(), Some("图"));
        assert_eq!(q.limit, 50);
        assert_eq!(q.offset, 0);
    }

    /// 制造一个**真的坏掉**的库：把 `assets` 表删掉。
    ///
    /// 这不是 mock——是真 SQLite 真报错，与「文件损坏 / 被锁住 / 路径不存在」
    /// 在错误通道上完全同形。关掉外键是为了让 DROP 不去级联删子表，
    /// 我们要的就是「表不见了」这一种故障形态。
    fn broken_registry() -> Registry {
        let reg = Registry::open_in_memory().unwrap();
        reg.conn
            .execute_batch("PRAGMA foreign_keys = OFF; DROP TABLE assets;")
            .expect("测试前置：制造一个真的坏掉的库");
        reg
    }

    #[test]
    fn broken_sqlite_answers_with_err_not_with_empty_vec() {
        let reg = broken_registry();
        let f = AssetFilter::all();

        // ① is_healthy 必须自己承认不可用（流水线的 query_baseline 会问这一位
        //    来区分「存储已不可用」与「查询单独失败」）。
        assert!(!AssetStore::is_healthy(&reg), "表没了就不该自称健康");

        // ② 关键：SQL 失败必须冒泡成 Err，绝不能是 Ok(空)。
        let r = AssetStore::try_query(&reg, &f, 100);
        assert!(
            r.is_err(),
            "SQL 失败必须报 Err，实际却返回了 {:?} 行",
            r.as_ref().map(|v| v.len())
        );
        let msg = r.unwrap_err();
        assert!(
            msg.contains("assets"),
            "错误原因应指向缺失的表（从而证明错误真的一路上抛了），实际：{msg}"
        );

        // ③ 有损视图仍然是空——这正是它危险、也正是必须由 try_query 取代的原因。
        assert!(AssetStore::query(&reg, &f, 100).is_empty());
        assert_eq!(AssetStore::count(&reg, &f), 0);
    }

    #[test]
    fn real_sqlite_failure_surfaces_as_failed_baseline_in_the_pipeline() {
        // 端到端：真 SQLite 失败 → 流水线必须报「基线故障」，
        // 而不是「基线成功但 0 条」。这正是 FIX 1 之前会静默发生的事。
        let healthy = demo_registry();
        let ok = RetrievalEngine::structured_only(&healthy)
            .search(&SearchQuery::new(AssetFilter::all(), 100));
        assert_eq!(ok.hits.len(), 3, "对照组：好库必须照常返回 3 条");
        assert!(ok.degradation.used.contains(&ac_retrieval::Stage::Structured));
        assert!(!ok.degradation.baseline_failed());

        let reg = broken_registry();
        let r = RetrievalEngine::structured_only(&reg)
            .search(&SearchQuery::new(AssetFilter::all(), 100));
        assert!(r.hits.is_empty());
        assert!(
            !r.degradation.used.contains(&ac_retrieval::Stage::Structured),
            "基线查询失败却记进 used = 谎报成功"
        );
        assert!(
            r.degradation.baseline_failed(),
            "必须报基线故障，实际报告：{}",
            r.degradation.summary()
        );
        let st = r.degradation.baseline_state();
        assert!(st.is_failed(), "实际状态：{st}");
        // 顺带证明 is_healthy() 确实被流水线问到了（不只是定义在那儿）：
        // 失败原因的措辞取自它返回 false 的那一支。
        assert!(
            st.to_string().contains("结构化存储不可用"),
            "失败分类应来自 is_healthy() 的返回值，实际：{st}"
        );
    }

    #[test]
    fn empty_string_filter_is_treated_as_absent() {
        // 与桌面端 ListQuery 一致：空串 = 没这个条件，而不是「搜空串」。
        let f = AssetFilter {
            kind: Some("   ".into()),
            min_authority: None,
            tag: Some("".into()),
            text: Some("  ".into()),
        };
        let q = to_asset_query(&f, 10);
        assert!(q.kind.is_none() && q.tag.is_none() && q.text.is_none());
    }

    // ── M2 第二刀：全文索引输入源 + 候选集取行 ──────────────────

    #[test]
    fn index_source_sig_is_content_hash_plus_parser_rev() {
        let reg = demo_registry();
        let snap = reg.index_source().expect("索引输入源必须可用");
        assert_eq!(snap.len(), 3);
        // sig 必须是「内容指纹 | 解析器修订号」——与注册表自己的增量闸门同源。
        for e in &snap.entries {
            let hash: String = reg
                .conn
                .query_row(
                    "SELECT COALESCE(content_hash, '') FROM assets WHERE id = ?1",
                    params![e.id],
                    |r| r.get(0),
                )
                .unwrap();
            let rev: i64 = reg
                .conn
                .query_row(
                    "SELECT parser_rev FROM assets WHERE id = ?1",
                    params![e.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                e.sig,
                format!("{hash}|{rev}"),
                "变更信号必须由 content_hash + parser_rev 构成，实际：{}",
                e.id
            );
            assert!(
                !e.text.trim().is_empty(),
                "可检索文本不得为空——空文本会让全文检索对该资产永久失效：{}",
                e.id
            );
        }
        // 指纹必须稳定：同一库读两次指纹相同。
        assert_eq!(snap.fingerprint(), reg.index_source().unwrap().fingerprint());
    }

    #[test]
    fn index_source_text_contains_name_and_tags() {
        // 索引的是 search_blob，而 search_blob 按入库时的 build_search_blob
        // 拼接，必须含 name 与能力标签，否则用户搜资产名/标签会搜不到。
        let reg = demo_registry();
        let snap = reg.index_source().unwrap();
        let a = snap.entries.iter().find(|e| e.id == "s::a").unwrap();
        assert!(a.text.contains("画时序图"), "search_blob 必须含 name，实际：{}", a.text);
        assert!(a.text.contains("read_only"), "search_blob 必须含能力标签，实际：{}", a.text);
    }

    #[test]
    fn try_query_in_agrees_with_query_assets_on_the_same_filter() {
        // 这是本切片最容易出的错：try_query_in 另写了一份 WHERE，
        // 一旦与 query_assets 漂移，「能被筛中的」与「结果里看到的」就对不上，
        // 而且**不会报错**——只是过滤悄悄失效。
        let reg = demo_registry();
        let cases = vec![
            AssetFilter::all(),
            AssetFilter { kind: Some("skill".into()), min_authority: None, tag: None, text: None },
            AssetFilter { kind: None, min_authority: Some(Authority::Write), tag: None, text: None },
            AssetFilter { kind: None, min_authority: None, tag: Some("read_only".into()), text: None },
            AssetFilter { kind: Some("mcp".into()), min_authority: Some(Authority::Read), tag: None, text: None },
        ];
        let all_ids: Vec<String> = vec!["s::a".into(), "s::b".into(), "m::c".into()];
        for f in cases {
            let mut via_query: Vec<String> = reg
                .query_assets(&to_asset_query(&f, 100))
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .collect();
            via_query.sort();
            let mut via_in: Vec<String> = reg
                .try_query_in(&f, &all_ids, 100)
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect();
            via_in.sort();
            assert_eq!(
                via_in, via_query,
                "try_query_in 与 query_assets 对同一过滤条件的答案不一致"
            );
        }
    }

    #[test]
    fn try_query_in_keeps_the_given_id_order() {
        // 调用方用顺序表达「谁更相关」，实现必须原样保留。
        let reg = demo_registry();
        let ids = vec!["m::c".to_string(), "s::a".to_string(), "s::b".to_string()];
        let got: Vec<String> = reg
            .try_query_in(&AssetFilter::all(), &ids, 100)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(got, vec!["m::c", "s::a", "s::b"]);
        // limit 必须真的截断。
        let got = reg.try_query_in(&AssetFilter::all(), &ids, 2).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "m::c");
    }

    #[test]
    fn try_query_in_returns_rows_with_tags_and_text_filled() {
        // 候选行是给用户看的展示行，标签与可检索文本必须填好，
        // 否则结果列表里每条都是空标签。
        let reg = demo_registry();
        let got = reg
            .try_query_in(&AssetFilter::all(), &["s::a".to_string()], 10)
            .unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].tags.contains(&"read_only".to_string()), "标签未回填：{:?}", got[0].tags);
        assert!(!got[0].text.is_empty(), "可检索文本未回填");
    }

    #[test]
    fn try_query_in_chunks_large_id_sets_instead_of_overflowing_sqlite_variable_limit() {
        // 一次性把上千个 id 塞进 WHERE IN 会撞 SQLite 的绑定变量上限。
        // 1200 个跨过分块阈值（CHUNK=400），必须照样能查。
        let reg = demo_registry();
        let ids: Vec<String> = (0..1200).map(|i| format!("不存在的id-{i}")).collect();
        let got = reg.try_query_in(&AssetFilter::all(), &ids, 100).unwrap();
        assert!(got.is_empty(), "全是假 id，必须返回空而不是报错");
    }

    #[test]
    fn broken_index_source_reports_err_not_empty_success() {
        // 索引输入源失败必须冒泡成 Err：返回空快照会让索引以为「库里就是空的」，
        // 然后全文检索返回空结果并报告就绪——正是 M2 第一刀抓到的谎报形状。
        let reg = broken_registry();
        let e = reg.index_source().expect_err("assets 表没了必须报错");
        assert!(e.contains("assets"), "错误应指向缺失的表，实际：{e}");
    }
}

