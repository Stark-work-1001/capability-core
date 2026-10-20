//! 内存 `AssetStore` —— 供测试与「脱离 SQLite 单独测检索」用。
//!
//! 硬性约束里写着「检索算法要能脱离 SQLite 单独测试，也能换存储后端」。
//! 这就是那个「脱离 SQLite」的载体：它让 `ac-retrieval` 的测试
//! 零数据库依赖，也让未来换存储后端有个最小可跑的参照实现。
//!
//! 过滤语义**逐条对齐** ac-registry 已在 M1 证明过的那套（kind 等值、
//! tag 单值 OR、min_authority 按枚举序、text 包含匹配），
//! 保证内存实现与 SQLite 实现在同一组用例下给出相同结果。

use crate::store::{AssetFilter, AssetRow, AssetStore, IndexEntry, IndexSnapshot};
use ac_core::Authority;

/// 内存实现。构造后即不可变，适合并发读。
#[derive(Debug, Clone, Default)]
pub struct InMemoryStore {
    rows: Vec<AssetRow>,
    healthy: bool,
}

impl InMemoryStore {
    pub fn new(rows: Vec<AssetRow>) -> Self {
        Self {
            rows,
            healthy: true,
        }
    }

    /// 构造一份**真的坏了**的存储，用于验证基线不可用时的行为。
    ///
    /// # 修复记录：这里曾经是个假故障
    ///
    /// 早先这个构造只把 `is_healthy()` 翻成 false，`query()` 照样把行返回。
    /// 于是「库坏了」与「库里没有」在测试里长得**一模一样**——这正是
    /// 「结构化基线失败被静默吞成空结果」能长期潜伏、无人发现的根因：
    /// 没有任何一条既有测试能把它逼出来。
    ///
    /// 现在它按损坏 SQLite 的方式表现：
    ///
    /// - `try_query` 返回 `Err`（不返回空列表）；
    /// - `count` 记 0；
    /// - `query`（有损视图）退化为空——但**绝不再假装自己健康**。
    ///
    /// `rows` 故意保留：被锁住 / 损坏的 SQLite **库里是有数据的**，只是读不出来。
    /// 保留它才能证明流水线不会因为「后端手里明明有行」就把失败读成成功。
    pub fn unhealthy(rows: Vec<AssetRow>) -> Self {
        Self {
            rows,
            healthy: false,
        }
    }
}

impl AssetStore for InMemoryStore {
    fn try_query(&self, filter: &AssetFilter, limit: u32) -> Result<Vec<AssetRow>, String> {
        if !self.healthy {
            return Err("内存存储被标记为故障（模拟 SQLite 锁住 / 损坏）".into());
        }
        Ok(self.rows
            .iter()
            .filter(|r| matches_filter(r, filter))
            .take(limit as usize)
            .cloned()
            .collect())
    }

    fn query(&self, filter: &AssetFilter, limit: u32) -> Vec<AssetRow> {
        self.try_query(filter, limit).unwrap_or_default()
    }

    fn count(&self, filter: &AssetFilter) -> u32 {
        if !self.healthy {
            return 0;
        }
        self.rows.iter().filter(|r| matches_filter(r, filter)).count() as u32
    }

    fn is_healthy(&self) -> bool {
        self.healthy
    }

    /// 内存后端的索引输入源。`sig` 用「行内容」现算——
    /// 内存库没有 `content_hash` / `parser_rev` 那对信号，而
    /// 「行内容变了 sig 就变」正是增量闸门要的性质。
    ///
    /// 故障存储**必须**报错：返回一个空快照会让索引以为「库里就是空的」，
    /// 那正是 M2 第一刀抓到的谎报形状。
    fn index_source(&self) -> Result<IndexSnapshot, String> {
        if !self.healthy {
            return Err("内存存储被标记为故障，无法读取索引输入源".into());
        }
        let mut entries: Vec<IndexEntry> = self
            .rows
            .iter()
            .map(|r| IndexEntry {
                id: r.id.clone(),
                sig: memory_sig(r),
                text: r.text.clone(),
            })
            .collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(IndexSnapshot { entries })
    }

    /// 覆写默认实现：内存后端是线性表，直接建 id -> 位置映射比
    /// 「先全扫过滤再按 ids 筛」少一轮比较，且结构化过滤仍然走
    /// `matches_filter`——与 SQLite 实现同源，不另写一份语义。
    fn try_query_in(
        &self,
        filter: &AssetFilter,
        ids: &[String],
        limit: u32,
    ) -> Result<Vec<AssetRow>, String> {
        if !self.healthy {
            return Err("内存存储被标记为故障，无法按候选集取行".into());
        }
        let mut want: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::with_capacity(ids.len());
        for (i, id) in ids.iter().enumerate() {
            want.entry(id.as_str()).or_insert(i);
        }
        let mut hit: Vec<(usize, AssetRow)> = Vec::new();
        for r in &self.rows {
            // filter.text 按契约已由调用方清空；这里仍然走 matches_filter，
            // 保证内存实现与 SQLite 实现用的是同一套维度语义。
            if let Some(pos) = want.get(r.id.as_str()) {
                if matches_filter(r, filter) {
                    hit.push((*pos, r.clone()));
                }
            }
        }
        hit.sort_by_key(|(p, _)| *p);
        Ok(hit.into_iter().take(limit as usize).map(|(_, r)| r).collect())
    }
}

/// 内存行的变更信号：任何**被索引或被展示**的字段变了 sig 就变。
///
/// **必须包含 `text`**：进倒排的就是它。第一版漏了它，于是
/// 「只改 text」的用例重写 0 条文档，报告却写「增量生效」——
/// 那个假阳性比测不出问题更糟。（注册表侧不受影响：那里 sig 用的是
/// `content_hash|parser_rev`，而 search_blob 由 manifest 派生，
/// 改 text 必然改 content_hash。）
///
/// 用 sha256 而不是 `DefaultHasher`——后者的算法**跨 Rust 版本不保证稳定**，
/// 一升级工具链就会让全库 sig 变一遍，索引被白白全量重建一次。
fn memory_sig(r: &AssetRow) -> String {
    let joined = format!(
        "{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        r.id,
        r.kind,
        r.name,
        r.description,
        r.authority,
        r.tags.join(","),
        r.logical_id.clone().unwrap_or_default(),
        r.text
    );
    let mut h = <sha2::Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut h, joined.as_bytes());
    format!("{:x}", sha2::Digest::finalize(h))
}

/// 过滤语义：与 ac-registry 的 SQL 逐条对齐。
fn matches_filter(row: &AssetRow, f: &AssetFilter) -> bool {
    // kind：等值
    if let Some(k) = &f.kind {
        if &row.kind != k {
            return false;
        }
    }
    // tag：单值 OR —— 任一标签命中即通过
    if let Some(t) = &f.tag {
        if !row.tags.iter().any(|x| x == t) {
            return false;
        }
    }
    // min_authority：按枚举序，不是字典序
    if let Some(min) = f.min_authority {
        let cur = Authority::parse(&row.authority).unwrap_or(Authority::Read);
        if cur < min {
            return false;
        }
    }
    // text：对可检索纯文本做包含匹配
    if let Some(t) = &f.text {
        if !t.trim().is_empty() && !row.text.contains(t.as_str()) {
            return false;
        }
    }
    true
}

