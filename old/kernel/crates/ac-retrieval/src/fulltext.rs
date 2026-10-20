//! 全文 BM25 召回层（tantivy）—— M2 第二刀。
//!
//! # 这个模块在整条链路里的位置
//!
//! ```text
//! 查询 → tantivy BM25 召回 → 结构化过滤（kind/tag/dimensions）→ 精排 → 返回
//!          └─ 本模块 ─┘        └─ 回到 store 执行 ┘      └─ rerank.rs ┘
//! ```
//!
//! # 三条不可让步的约束
//!
//! 1. **可选层。** 本模块的任何失败都只影响 `Stage::FullText`，
//!    不影响结构化基线返回结果。索引不存在 / 过期 / 构建失败 / 损坏，
//!    四种情况一律降级，绝不冒泡成「检索整体失败」。
//! 2. **状态不说谎。** `FullTextIndex::attach` **永不返回 Err**：
//!    成功与失败都体现在 `StageState` 里，由流水线写进 `DegradationReport`。
//!    「没建索引」是 `Absent`，「建了但坏了」是 `Failed(原因)`，两者不可互换。
//! 3. **索引只是派生物。** 它只负责**召回与打分**，从不提供展示用的行。
//!    行一律回到 store 取（`AssetStore::try_query_in`），
//!    于是索引过期时返回的仍然是库里的真内容，而不是旧副本。
//!
//! # 索引放在哪
//!
//! `FullTextIndex::index_dir_for` 把索引目录定义为**数据库文件所在目录下的
//! `search-index/`**。理由：
//!
//! - 桌面端已有 `app_data_dir_override()`（`apps/desktop/src-tauri/src/lib.rs`），
//!   它决定 `data_dir`，而 `db_path = data_dir/registry.db`。
//!   索引挂在 db 旁边，就**自动继承了**那套改道逻辑——
//!   本机对 `C:\external-assets\` 启动的进程写 `%USERPROFILE%` 会
//!   `ERROR_ACCESS_DENIED`，把索引放进用户目录就会踩中同一条策略。
//! - 备份/迁移 `registry.db` 时索引跟着走，不会出现「库换了、索引没换」的
//!   静默错配。
//! - 索引是**可重建**的派生物，删掉不丢信息；放在库里没有任何代价。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, TermQuery};
use tantivy::schema::{
    IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
};
use tantivy::tokenizer::{LowerCaser, TextAnalyzer, Token, TokenStream, Tokenizer};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument};
use tantivy::schema::Field;

use crate::health::StageState;
use crate::store::{AssetStore, IndexSnapshot};

/// 索引目录名（挂在 db 文件同级）。
pub const INDEX_DIR_NAME: &str = "search-index";
/// 索引目录里的旁挂元数据文件名。
pub const META_FILE_NAME: &str = "ac-index-meta.json";
/// schema / 分词方案的版本号。**改了分词或字段就必须 +1**，
/// 否则旧索引会被当成同一种打开，而它的倒排是按老规则建的——结果是
/// 「能打开、但召回莫名其妙」，是本项目最怕的那类静默错误。
pub const SCHEMA_VERSION: u32 = 1;
/// 注册给 schema 用的分词器名。
pub const TOKENIZER_NAME: &str = "ac_script_v1";
/// 索引内存预算（字节）。tantivy 的 indexing thread 按这个预算分堆。
pub const WRITER_MEMORY_BUDGET: usize = 50_000_000;

// ─────────────────────────────────────────────────────────────────
// 分词：脚本感知（ASCII 按词，CJK 按二元组）
// ─────────────────────────────────────────────────────────────────
//
// 为什么不用 tantivy 默认分词器：默认实现按 `char::is_alphanumeric` 切，
// 而 CJK 字符**也是** alphanumeric，于是「画时序图」会变成**一个** token。
// 后果是全文检索退化成「整串精确匹配」：搜「时序图」一个都搜不到。
//
// 为什么不对全文本做 n-gram：ASCII 词被切成二元组后噪声很大，
// `the` 会变成 `th`+`he` 并到处命中。
//
// 所以按脚本分流：ASCII 词整体成词（BM25 的正常用法），
// CJK 串切二元组（中文全文检索的标准做法），单字串退回单字。
// 这是纯规则、可解释、可单测的，不含任何学习成分。

/// 是否 CJK 表意文字（含扩展 A 与兼容区的主要部分）。
fn is_cjk(c: char) -> bool {
    matches!(u32::from(c),
        0x3400..=0x4DBF      // 扩展 A
        | 0x4E00..=0x9FFF    // 基本区
        | 0xF900..=0xFAFF    // 兼容表意
        | 0x20000..=0x2A6DF  // 扩展 B
        | 0x2A700..=0x2EBEF  // 扩展 C-F
        | 0x2F800..=0x2FA1F) // 兼容补充
}

/// 脚本感知的切词器：ASCII 成词、CJK 切二元组。
#[derive(Debug, Clone)]
pub struct ScriptTokenizer {
    pub cjk_gram: usize,
}

impl Default for ScriptTokenizer {
    fn default() -> Self {
        Self { cjk_gram: 2 }
    }
}

impl Tokenizer for ScriptTokenizer {
    type TokenStream<'a> = ScriptTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        ScriptTokenStream {
            tokens: tokenize(text, self.cjk_gram),
            cursor: 0,
            current: Token::default(),
        }
    }
}

/// 预先切好的一次性 token 流。
///
/// 一次性建 `Vec` 而不是惰性迭代：文档正文最长 4203 字符（实测真实库 p95=1293），
/// 分配成本远低于把迭代器状态机写错的成本，且与 tantivy 其它内置切词器
/// （`NgramTokenizer` 等）的做法一致。
pub struct ScriptTokenStream {
    tokens: Vec<Token>,
    cursor: usize,
    /// 当前 token 的**副本**。
    ///
    /// 为什么不用 `&self.tokens[cursor]` 直接索引：tantivy 允许在
    /// `advance()` 返回 false **之后**再调 `token()`（若干内部组件会这么做）。
    /// 直接索引就会越界 panic —— 而 release profile 是 `panic = "abort"`，
    /// 于是一个切词器的边界 bug 会**直接杀掉整个进程**，连降级都来不及。
    /// 保留一份「永远有效」的副本，这条路径就不存在了。
    current: Token,
}

impl TokenStream for ScriptTokenStream {
    fn advance(&mut self) -> bool {
        let Some(next) = self.tokens.get(self.cursor) else {
            return false;
        };
        self.current.clone_from(next);
        self.cursor += 1;
        true
    }
    fn token(&self) -> &Token {
        &self.current
    }
    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

/// 切词本体。索引与查询共用它——**共用是硬要求**，
/// 两边规则不一致时 BM25 分数会静默失真，且没有任何报错。
pub fn tokenize(text: &str, cjk_gram: usize) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let mut position: usize = 0;
    let mut run: Vec<(usize, char)> = Vec::new();

    let flush = |run: &mut Vec<(usize, char)>, out: &mut Vec<Token>, position: &mut usize| {
        if run.is_empty() {
            return;
        }
        if is_cjk(run[0].1) {
            // CJK 串：重叠二元组。长度不足 gram 时整串作为一个 token，
            // 否则「图」这种单字词会一个 token 都留不下。
            let n = run.len();
            if n < cjk_gram {
                let from = run[0].0;
                let to = run[n - 1].0 + run[n - 1].1.len_utf8();
                out.push(Token {
                    offset_from: from,
                    offset_to: to,
                    position: *position,
                    text: run.iter().map(|(_, c)| *c).collect(),
                    position_length: 1,
                });
                *position += 1;
            } else {
                for i in 0..=(n - cjk_gram) {
                    let from = run[i].0;
                    let last = run[i + cjk_gram - 1];
                    let to = last.0 + last.1.len_utf8();
                    out.push(Token {
                        offset_from: from,
                        offset_to: to,
                        position: *position,
                        text: run[i..i + cjk_gram].iter().map(|(_, c)| *c).collect(),
                        position_length: 1,
                    });
                    *position += 1;
                }
            }
        } else {
            let from = run[0].0;
            let last = run[run.len() - 1];
            let to = last.0 + last.1.len_utf8();
            out.push(Token {
                offset_from: from,
                offset_to: to,
                position: *position,
                text: run.iter().map(|(_, c)| *c).collect::<String>().to_lowercase(),
                position_length: 1,
            });
            *position += 1;
        }
        run.clear();
    };

    for (i, c) in text.char_indices() {
        if !c.is_alphanumeric() {
            flush(&mut run, &mut out, &mut position);
            continue;
        }
        // 脚本切换即断词：`MCP服务` 切成 `mcp` + `服务` 的二元组。
        if let Some((_, prev)) = run.last() {
            if is_cjk(*prev) != is_cjk(c) {
                flush(&mut run, &mut out, &mut position);
            }
        }
        run.push((i, c));
    }
    flush(&mut run, &mut out, &mut position);
    out
}

/// 本项目统一使用的分析器（分词 + 小写化）。
pub fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(ScriptTokenizer::default())
        .filter(LowerCaser)
        .build()
}

/// 按与索引完全相同的规则切词，供精排复用。
/// 与 [tokenize] 的差别只有一处：不返回偏移与词位，只给词面。
pub fn analyze_terms(text: &str) -> Vec<String> {
    tokenize(text, ScriptTokenizer::default().cjk_gram)
        .into_iter()
        .map(|t| t.text)
        .collect()
}

// ─────────────────────────────────────────────────────────────────
// schema 与索引句柄
// ─────────────────────────────────────────────────────────────────

fn build_schema() -> Schema {
    let mut b = Schema::builder();
    // id：原样一个 term，用于 delete_term 精确定位与结果回填。
    b.add_text_field("id", STRING | STORED);
    let indexing = TextFieldIndexing::default()
        .set_tokenizer(TOKENIZER_NAME)
        // 词位让 BM25 能算真实的 term distance（默认 WithFreqs 已足够，
        // 这里显式要 positions 是为了将来做短语查询时不必重建索引）。
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let opts = TextOptions::default()
        .set_indexing_options(indexing)
        .set_stored();
    b.add_text_field("body", opts);
    b.build()
}

/// 挂上分词器。**打开已有索引时也必须调**：
/// schema 里只记了分词器的**名字**，实现要在运行时重新注册，
/// 否则查询会直接报错。
fn register_tokenizer(index: &Index) {
    index
        .tokenizers()
        .register(TOKENIZER_NAME, analyzer());
}

/// 索引的旁挂元数据（与 tantivy 自己的 meta.json 并列，互不干扰）。
///
/// 存在意义：判断「哪些文档在索引里、各自的变更信号是什么」需要一次 O(n) 读取。
/// 从倒排里反查当然也行，但要走 stored fields、还要处理多 segment，
/// 既慢又脆。索引本来就是派生物，这份账本丢了可以整体重建，
/// 丢了之后的判定是 **Failed**（原因明确），而不是「假装没变化」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMeta {
    pub schema_version: u32,
    pub tokenizer: String,
    /// 库侧快照的内容指纹（见 [crate::store::IndexSnapshot::fingerprint]）。
    pub fingerprint: String,
    /// (资产 id, 变更信号)，按 id 排序。
    pub docs: Vec<(String, String)>,
}

impl IndexMeta {
    fn fresh() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            tokenizer: TOKENIZER_NAME.to_string(),
            fingerprint: String::new(),
            docs: Vec::new(),
        }
    }
    fn map(&self) -> HashMap<&str, &str> {
        self.docs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
    }
}

/// 挂载策略：决定「索引不可用时要不要当场重建」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPolicy {
    /// 自动重建。缺索引 / 过期 / 损坏都当场重建。
    /// **首次运行的默认策略**：全新机器上索引本来就不存在，
    /// 不允许「因为索引不存在所以检索不了」。
    Auto,
    /// 只读挂载，**一个字都不写盘**。
    /// 用于「索引构建在后台跑，查询路径不参与写盘」的场景：
    /// 这时索引缺失/过期/损坏都必须如实报成不可用，由基线兜底。
    Manual,
}

/// 一次同步的实际动作统计——**增量更新的证据就在这里**。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// 新建文档数。
    pub added: u32,
    /// 变更信号变了、因而重写的文档数。
    pub updated: u32,
    /// 库里有、索引里没有因而删除的文档数。
    pub deleted: u32,
    /// 一个字都没动的文档数。
    pub unchanged: u32,
    /// 同步后索引里的文档总数。
    pub total: u32,
    /// 真正进倒排的文档数 = added + updated。**增量更新的核心指标**：
    /// 改一条资产时它应当是 1，而不是全库条数。
    pub rewritten: u32,
    pub elapsed: Duration,
}

/// 一条召回结果：id + BM25 原始分。
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub id: String,
    pub score: f32,
}

/// 全文索引句柄。
///
/// 构造成功后 `state()` 就是**这份索引此刻的真实状态**，
/// 不需要上层再猜、也不用担心「快照说就绪、实际已坏」。
pub struct FullTextIndex {
    dir: PathBuf,
    index: Index,
    reader: IndexReader,
    meta: IndexMeta,
    state: StageState,
    policy: SyncPolicy,
}

impl std::fmt::Debug for FullTextIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FullTextIndex")
            .field("dir", &self.dir)
            .field("docs", &self.meta.docs.len())
            .field("fingerprint", &self.meta.fingerprint)
            .field("state", &self.state)
            .finish()
    }
}

impl FullTextIndex {
    /// 索引目录：**与数据库文件同级**的 `search-index/`。
    ///
    /// 见模块文档：这样它自动继承桌面端的 `app_data_dir_override()`，
    /// 不会踩「从 C:\external-assets\ 启动的进程写不了 %USERPROFILE%」那条策略。
    pub fn index_dir_for(db_path: &Path) -> PathBuf {
        db_path
            .parent()
            .map(|p| p.join(INDEX_DIR_NAME))
            .unwrap_or_else(|| PathBuf::from(INDEX_DIR_NAME))
    }

    /// 挂载索引。**永不返回 Err**：所有结局都写进 `state()`。
    ///
    /// # 为什么「索引不存在」报 Failed 而不是 Absent
    ///
    /// `StageState::Absent` 的原义是「**这一版压根没有这个阶段**」，
    /// 是个不需要处理的事实。而「阶段接好了、索引文件却没有」是一项
    /// **需要动作**的故障：要么首次运行没建，要么被谁删了。
    /// 更要紧的是：`Absent` 携带不了原因，而任务要求这四种降级
    /// 都必须**带上原因**——只有 `Failed(String)` 带得动。
    /// 所以「没接全文层」才报 Absent（见流水线），「接了但没索引」报 Failed。
    pub fn attach<S: AssetStore + ?Sized>(
        dir: impl AsRef<Path>,
        store: &S,
        policy: SyncPolicy,
    ) -> Self {
        let dir = dir.as_ref().to_path_buf();
        // ① 库侧快照。读不到就到此为止：没有输入，任何重建都没有意义。
        let snapshot = match store.index_source() {
            Ok(s) => s,
            Err(e) => {
                return Self::failed(dir, format!("索引输入源不可用：{e}"));
            }
        };
        // ② 索引是否存在。
        let meta_path = dir.join(META_FILE_NAME);
        let meta: Option<IndexMeta> = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());

        let Some(meta) = meta else {
            // 读不到账本有两种可能：目录压根没有，或账本坏了。
            // 不区分「没有」与「坏了」——两者的正确处置都是重建，
            // 而在只读策略下两者都必须报不可用。真正要区分的是
            // 「Absent（没有这能力）」与「Failed（有但坏了）」，
            // 所以这里按「索引目录里连 tantivy 的 meta 都没有」来分。
            let index_exists = dir.join("meta.json").exists();
            if !index_exists {
                return match policy {
                    SyncPolicy::Auto => Self::build_from_scratch(dir, snapshot, "索引不存在，已新建"),
                    SyncPolicy::Manual => Self::failed(
                        dir,
                        "全文索引不存在（只读挂载，不自动创建；首次运行请用 SyncPolicy::Auto）"
                            .to_string(),
                    ),
                };
            }
            return match policy {
                SyncPolicy::Auto => Self::build_from_scratch(dir, snapshot, "索引账本损坏，已重建"),
                SyncPolicy::Manual => {
                    Self::failed(dir, "索引账本损坏且不可读（只读挂载，无法重建）".to_string())
                }
            };
        };

        // ③ schema / 分词版本。旧版本的索引**不能用**：倒排是按老规则建的。
        if meta.schema_version != SCHEMA_VERSION || meta.tokenizer != TOKENIZER_NAME {
            return match policy {
                SyncPolicy::Auto => Self::build_from_scratch(
                    dir,
                    snapshot,
                    "索引格式版本不符，已重建",
                ),
                SyncPolicy::Manual => Self::failed(
                    dir,
                    format!(
                        "索引格式版本不符（账本 v{} / {}，当前 v{} / {}）",
                        meta.schema_version,
                        meta.tokenizer,
                        SCHEMA_VERSION,
                        TOKENIZER_NAME
                    ),
                ),
            };
        }

        // ④ 打开倒排本体。打不开就是**索引损坏**。
        let index = match Index::open_in_dir(&dir) {
            Ok(i) => i,
            Err(e) => {
                return match policy {
                    SyncPolicy::Auto => {
                        Self::build_from_scratch(dir, snapshot, "索引损坏，已重建")
                    }
                    SyncPolicy::Manual => Self::failed(
                        dir,
                        format!("索引损坏，无法打开（只读挂载，无法重建）：{e}"),
                    ),
                };
            }
        };

        // ⑤ 过期判定。指纹不一致 = 库比索引新。
        let current = snapshot.fingerprint();
        if meta.fingerprint != current {
            return match policy {
                SyncPolicy::Auto => {
                    let mut h = match Self::open_handle(dir.clone(), index, meta.clone()) {
                        Ok(h) => h,
                        Err(e) => return Self::failed(dir, e),
                    };
                    match h.sync_from(&snapshot) {
                        Ok(_) => {
                            h.state = StageState::Ready;
                            h
                        }
                        Err(e) => Self::failed(dir, format!("索引重建失败：{e}")),
                    }
                }
                SyncPolicy::Manual => Self::failed(
                    dir,
                    "索引过期：库内容已变更（新增/删除/解析器修订号变化），待后台重建"
                        .to_string(),
                ),
            };
        }

        // ⑥ 一切吻合。
        match Self::open_handle(dir.clone(), index, meta) {
            Ok(mut h) => {
                h.state = StageState::Ready;
                h
            }
            Err(e) => Self::failed(dir, e),
        }
    }

    /// 造一个「没有可用索引」的句柄（只带路径与失败原因）。
    fn failed(dir: PathBuf, reason: String) -> Self {
        // 用一个内存空索引兜底：它永远打不开也永远搜不出东西，
        // 正好对应「这份句柄代表的索引不可用」。
        let schema = build_schema();
        let index = Index::create_in_ram(schema);
        register_tokenizer(&index);
        let reader = index.reader().expect("内存索引读句柄必然成功");
        Self {
            dir,
            index,
            reader,
            meta: IndexMeta::fresh(),
            state: StageState::Failed(reason),
            policy: SyncPolicy::Manual,
        }
    }

    /// 删掉目录并从零构建。
    fn build_from_scratch(dir: PathBuf, snapshot: IndexSnapshot, why: &str) -> Self {
        for attempt in 0..3 {
            let result=Self::build_once(dir.clone(),&snapshot,why);
            let transient=matches!(&result.state,StageState::Failed(e) if e.contains("PermissionDenied") || e.contains("拒绝访问") || e.contains("Access is denied"));
            if !transient || attempt==2{return result;}
            tracing::warn!(attempt=attempt+1,"Transient file access failure while building full-text index; retrying after releasing handles");
            drop(result);
            std::thread::sleep(Duration::from_millis(50*(attempt+1)));
        }
        unreachable!()
    }
    fn build_once(dir: PathBuf, snapshot: &IndexSnapshot, why: &str) -> Self {
        // 目录可能是只读、可能被占用、可能是损坏后残留的半成品——
        // 一律先尝试清除。任何一步失败都必须变成 `Failed`，不能崩。
        if dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                return Self::failed(
                    dir,
                    format!("索引重建失败（无法清除旧索引）：{e}（{why}）"),
                );
            }
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return Self::failed(dir, format!("索引重建失败（无法创建目录）：{e}（{why}）"));
        }
        let schema = build_schema();
        let index = match Index::create_in_dir(&dir, schema) {
            Ok(i) => i,
            Err(e) => return Self::failed(dir, format!("索引重建失败（无法创建索引）：{e}（{why}）")),
        };
        let mut h = match Self::open_handle(dir.clone(), index, IndexMeta::fresh()) {
            Ok(h) => h,
            Err(e) => return Self::failed(dir, e),
        };
        match h.sync_from(&snapshot) {
            Ok(_) => {
                h.state = StageState::Ready;
                h
            }
            Err(e) => Self::failed(dir, format!("索引重建失败：{e}（{why}）")),
        }
    }

    /// 打开读句柄并登记分词器。
    fn open_handle(dir: PathBuf, index: Index, meta: IndexMeta) -> Result<Self, String> {
        register_tokenizer(&index);
        let reader = index
            .reader_builder()
            // **Manual，不是 OnCommitWithDelay。**
            //
            // OnCommitWithDelay 是「提交后**在若干毫秒内**异步 reload」，
            // 于是「刚建完索引立刻查询」这条完全正常的时序会读到**上一个**
            // 版本——对刚建的索引来说就是空索引，于是查询静默返回 0 条。
            // 基准因为建完索引后还要打印一堆统计、跑 20 次预热，
            // 等到真正查询时 reload 早就发生了，所以只有紧跟着查询的测试会红。
            //
            // 手动 reload 让「提交 -> 可见」这一步变成**同步且确定**的：
            // sync_from 提交后立刻显式 reload 一次（见该函数末尾），
            // 于是「commit 返回 = 新版本可见」这条不变量永远成立。
            // reload 只读一次 meta.json，且只发生在同步路径上（不在查询路径上），
            // 代价可以忽略。
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| format!("索引读句柄不可用：{e}"))?;
        Ok(Self {
            dir,
            index,
            reader,
            meta,
            // 真正的状态由调用方在验证完毕后写入（Attach 的最后一步）。
            state: StageState::Ready,
            policy: SyncPolicy::Manual,
        })
    }

    /// 这份索引此刻的真实状态。**只有 `Ready` 才可参与查询。**
    pub fn state(&self) -> &StageState {
        &self.state
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn meta(&self) -> &IndexMeta {
        &self.meta
    }

    /// 索引里的文档数（读账本，不查倒排）。
    pub fn doc_count(&self) -> usize {
        self.meta.docs.len()
    }

    /// 挂载策略（重建失败时用它决定重试行为）。
    pub fn policy(&self) -> SyncPolicy {
        self.policy
    }

    /// 就地做一次增量同步。任何失败都如实落到 `state()`。
    pub fn sync<S: AssetStore + ?Sized>(&mut self, store: &S) -> Result<SyncReport, String> {
        let snapshot = store.index_source()?;
        match self.sync_from(&snapshot){
            Ok(report)=>{self.state=StageState::Ready;Ok(report)},
            Err(e)=>{self.state=StageState::Failed(e.clone());Err(e)}
        }
    }

    /// 增量同步本体：只重写**变更信号变了**的文档。
    fn sync_from(&mut self, snapshot: &IndexSnapshot) -> Result<SyncReport, String> {
        for attempt in 0..3 {
            let result=self.sync_once(snapshot);
            let transient=matches!(&result,Err(e) if e.contains("PermissionDenied") || e.contains("拒绝访问") || e.contains("Access is denied"));
            if !transient || attempt==2{return result;}
            std::thread::sleep(Duration::from_millis(50*(attempt+1)));
        }
        unreachable!()
    }
    fn sync_once(&mut self, snapshot: &IndexSnapshot) -> Result<SyncReport, String> {
        let started = Instant::now();
        let want: HashMap<&str, &str> = snapshot
            .entries
            .iter()
            .map(|e| (e.id.as_str(), e.sig.as_str()))
            .collect();
        let have = self.meta.map();

        let mut writer: IndexWriter = self
            .index
            .writer_with_num_threads(1,WRITER_MEMORY_BUDGET)
            .map_err(|e| format!("打开索引写入器失败：{e}"))?;
        let id_field = self
            .index
            .schema()
            .get_field("id")
            .map_err(|e| e.to_string())?;
        let body_field = self.index.schema().get_field("body").map_err(|e| e.to_string())?;

        let mut added = 0u32;
        let mut updated = 0u32;
        let mut deleted = 0u32;
        let mut unchanged = 0u32;

        // ① 库里有、索引里没有 → 删除。
        for (id, _sig) in &self.meta.docs {
            if !want.contains_key(id.as_str()) {
                writer
                    .delete_term(tantivy::Term::from_field_text(id_field, id));
                deleted += 1;
            }
        }

        // ② 新增 / 变更信号变了 → 先删旧文档再写新的。
        //    必须先删：tantivy 是追加写，只 add 会让同一 id 出现两份，
        //    搜索结果里同一个资产被列出两次——这是倒排索引最常见的低级错。
        for e in &snapshot.entries {
            match have.get(e.id.as_str()) {
                None => {
                    add_doc(&mut writer, id_field, body_field, e).map_err(|err| err.to_string())?;
                    added += 1;
                }
                Some(old) if *old != e.sig.as_str() => {
                    writer.delete_term(tantivy::Term::from_field_text(id_field, &e.id));
                    add_doc(&mut writer, id_field, body_field, e).map_err(|err| err.to_string())?;
                    updated += 1;
                }
                Some(_) => unchanged += 1,
            }
        }

        // `commit` 消费写入器（tantivy 0.26 的签名），提交成功后它就析构了。
        // 这里**刻意不**主动触发段合并：合并会让「改一条」的代价随库规模上涨，
        // 与增量更新的初衷相反。要回收空间交给后续的显式维护动作。
        writer
            .commit()
            .map_err(|e| format!("索引提交失败：{e}"))?;
        // 立刻把读句柄刷到新版本。见 open_handle 里 ReloadPolicy::Manual 的说明：
        // 没有这一步，「提交成功」与「新文档可查」之间就有一段不确定的窗口。
        self.reader
            .reload()
            .map_err(|e| format!("索引读句柄刷新失败：{e}"))?;

        let mut docs: Vec<(String, String)> = snapshot
            .entries
            .iter()
            .map(|e| (e.id.clone(), e.sig.clone()))
            .collect();
        docs.sort();
        self.meta = IndexMeta {
            schema_version: SCHEMA_VERSION,
            tokenizer: TOKENIZER_NAME.to_string(),
            fingerprint: snapshot.fingerprint(),
            docs,
        };
        write_meta(&self.dir, &self.meta)?;

        Ok(SyncReport {
            added,
            updated,
            deleted,
            unchanged,
            total: snapshot.len() as u32,
            rewritten: added + updated,
            elapsed: started.elapsed(),
        })
    }

    /// BM25 召回。返回按分数降序的 `(资产 id, 原始 BM25 分)`。
    ///
    /// 调用方**必须**先看 `state()`：本方法在索引不可用时返回 Err，
    /// 但那条 Err 永远只该被记进 `Stage::FullText`，不得冒泡。
    pub fn search(&self, text: &str, limit: usize) -> Result<Vec<Scored>, String> {
        if !self.state.is_usable() {
            return Err(format!("全文索引不可用（{}）", self.state));
        }
        let q = text.trim();
        if q.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let body_field = self
            .index
            .schema()
            .get_field("body")
            .map_err(|e| e.to_string())?;
        let id_field = self
            .index
            .schema()
            .get_field("id")
            .map_err(|e| e.to_string())?;

        // **不用 tantivy 的 QueryParser**，改为自己切词 + 显式 OR。
        //
        // 原因是一次实测出来的真 bug：QueryParser 的词法把 `_` 当作
        // 单词字符，于是 `渲染_容量规划` 被当成**一个词**送进分析器，
        // 结果一个 term 都构不出来 —— 召回 0 条，而同一个查询写成
        // `渲染 容量规划` 能召回 20 条。
        //
        // 这在本项目里不是假设：`mcp-server`、`cost_tier=low`、
        // `agent-design-studio` 全是用户会敲、语料里也确实有的词形。
        // 语法解析器会**静默**把它们吃掉——不报错、不告警，只是搜不到。
        //
        // 自己切词还顺带去掉了一整张转义表：用户输入被原样理解，
        // 不存在「哪些字符会被当成运算符」这种要靠记忆的规则。
        // 语义明确写成一句话：**用户输入 = 一串词，OR 起来**；
        // 相关性排序交给后面的精排（M2 链路里 BM25 只负责召回）。
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut terms: Vec<Box<dyn tantivy::query::Query>> = Vec::new();
        for t in analyze_terms(q) {
            if t.is_empty() || !seen.insert(t.clone()) {
                continue;
            }
            terms.push(Box::new(TermQuery::new(
                tantivy::Term::from_field_text(body_field, &t),
                IndexRecordOption::WithFreqsAndPositions,
            )));
        }
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let query = BooleanQuery::union(terms);
        let searcher = self.reader.searcher();
        let top = searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())
            .map_err(|e| format!("BM25 召回失败：{e}"))?;
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let d: TantivyDocument = searcher.doc(addr).map_err(|e| e.to_string())?;
            let id = d
                .get_first(id_field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !id.is_empty() {
                out.push(Scored { id, score });
            }
        }
        Ok(out)
    }
}

/// 写一条文档。
///
/// 刻意**不用** `doc!` 宏：它按字段名在当前作用域里找 `Field` 值，
/// 于是这个函数就得接受调用方的 `id_field`/`body_field` 变量名，
/// 读代码时要跳回去看调用点才知道绑的是哪个字段。这里显式构造更清楚。
fn add_doc(
    writer: &mut IndexWriter,
    id_field: Field,
    body_field: Field,
    e: &crate::store::IndexEntry,
) -> Result<(), tantivy::TantivyError> {
    let mut d = TantivyDocument::default();
    d.add_text(id_field, &e.id);
    d.add_text(body_field, &e.text);
    writer.add_document(d).map(|_| ())
}

/// **已删除**：原先靠转义查询语法字符来让 `QueryParser` 把用户输入当字面量。
///
/// 现在 [`FullTextIndex::search`] 直接用本模块的分词器切词、自己拼 OR，
/// 不再经过查询语法——整张转义表都没必要了。
///
/// 保留这段说明是因为它记录了一次**实测**踩坑：用 `QueryParser` 时
/// `渲染_容量规划` 召回 0 条，而 `渲染 容量规划` 召回 20 条。原因是
/// QueryParser 的词法把 `_` 当作单词字符，于是整个串被当成**一个词**
/// 送进分析器。靠转义兜是兜不住的：转义 `_` 会把它拆成 `渲染`/`容量`/`规划`
/// 三个词——**看起来能搜到，其实是搜错的**。
///
/// 本项目语料里 `mcp-server`、`cost_tier=low`、`agent-design-studio`
/// 全是用户会敲、库里也确实有的词形，所以这不是理论风险。
/// 自己切词既没有这个问题，也不需要记住哪些字符特殊。

/// 账本落盘。**先写临时文件再改名**：中途崩溃只会留下旧账本，
/// 不会出现「半个 JSON」被当成「索引损坏」之外的更难查的状态。
fn write_meta(dir: &Path, meta: &IndexMeta) -> Result<(), String> {
    let path = dir.join(META_FILE_NAME);
    let tmp = dir.join(META_FILE_NAME.to_string() + ".tmp");
    let json = serde_json::to_vec(meta).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &json).map_err(|e| format!("写索引账本失败：{e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换索引账本失败：{e}")
    })?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::memstore::InMemoryStore;
    use crate::store::{AssetFilter, AssetRow, AssetStore};
    use ac_core::Authority;

    // ── 测试用临时目录 ──────────────────────────────────────────
    //
    // **不用 %TEMP%**：本机有一条按目录生效的文件系统策略，从 C:\external-assets\
    // 启动的进程写 %USERPROFILE%（含 %TEMP%）会 ERROR_ACCESS_DENIED。
    // 放在 target/ 下：短路径、可写、随构建产物一起清掉。
    fn scratch(tag: &str) -> PathBuf {
        let mut p = std::env::current_exe().expect("current_exe");
        p.pop(); // deps
        if p.file_name().map(|n| n == "deps").unwrap_or(false) {
            p.pop(); // release / debug
        }
        p.push("m2-fulltext-tests");
        p.push(tag);
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("创建测试索引目录");
        p
    }

    /// 构造测试行。
    ///
    /// `text` **按契约**要含 name（见 `AssetRow::text` 的定义：
    /// 「name + description + capability 摘要拼接而成」，注册表侧的
    /// `search_blob` 也确实含 name）。所以这里统一拼上——否则会造出
    /// 一条「名字里写着 X、全文里没有 X」的假资产，测出来的召回率
    /// 毫无意义。
    fn row(id: &str, name: &str, text: &str) -> AssetRow {
        let mut r = AssetRow::new(id, "skill", name, Authority::Read);
        r.text = format!("{id} {name} {text}");
        r
    }

    /// 一份能区分「BM25 能查到 / LIKE 查不到」的小语料。
    ///
    /// 关键在 `s::multi`：它的正文里「时序」与「图」之间**隔着别的字**
    /// （「绘制时序 组件 与 流程图」），所以整串 LIKE '%时序图%' 抓不到它，
    /// 而 BM25 按词匹配能抓到。这就是全文层存在的意义本身。
    fn demo_rows() -> Vec<AssetRow> {
        vec![
            row("s::exact", "时序图", "把消息流转画成图 时序图 时序"),
            row("s::multi", "多关键词", "用于绘制时序 组件 与 流程图 时序 流程"),
            row("s::noise", "完全无关", "数据库备份与恢复脚本 备份 恢复"),
        ]
    }

    fn demo_store() -> InMemoryStore {
        InMemoryStore::new(demo_rows())
    }

    fn texts(t: &str) -> Vec<String> {
        tokenize(t, 2).into_iter().map(|x| x.text).collect()
    }

    // ── 分词器 ──────────────────────────────────────────────────

    #[test]
    fn cjk_is_split_into_bigrams_and_ascii_into_whole_words() {
        // CJK -> 二元组
        assert_eq!(texts("画时序图"), vec!["画时", "时序", "序图"]);
        // ASCII -> 整词（小写）
        assert_eq!(texts("MCP Server"), vec!["mcp", "server"]);
    }

    #[test]
    fn single_cjk_char_still_produces_a_token() {
        // 「图」只有一个字：切不出二元组，但**必须**留下一个 token，
        // 否则单字资产名一个都搜不到。
        assert_eq!(texts("图"), vec!["图"]);
    }

    #[test]
    fn script_switch_splits_ascii_and_cjk_runs() {
        // "MCP服务" 必须切成 mcp + 服务 的二元组，而不是混在一起。
        assert_eq!(texts("MCP服务"), vec!["mcp", "服务"]);
    }

    #[test]
    fn token_stream_is_safe_after_exhaustion() {
        // 回归测试：tantivy 允许在 advance() 返回 false 之后再调 token()。
        // 第一版直接索引 tokens[cursor] 会在这里越界 panic，
        // 而 release 是 panic=abort —— 直接杀进程，连降级都来不及。
        let toks = tokenize("画时序图", 2);
        let mut s = ScriptTokenStream {
            tokens: toks,
            cursor: 0,
            current: Token::default(),
        };
        let mut n = 0;
        while s.advance() {
            let _ = s.token().text.clone();
            n += 1;
        }
        assert_eq!(n, 3);
        // 耗尽后再调 token() 必须仍能给出合法引用，不能 panic。
        assert!(!s.advance());
        let _ = s.token();
        assert!(!s.advance());
        assert!(!s.advance());
    }

    // ── BM25 召回 ───────────────────────────────────────────────

    #[test]
    fn bm25_returns_relevance_ordered_hits_not_arbitrary_order() {
        let dir = scratch("bm25-order");
        let store = demo_store();
        let idx = FullTextIndex::attach(&dir, &store, SyncPolicy::Auto);
        assert!(idx.state().is_usable(), "{}", idx.state());

        let hits = idx.search("时序图", 10).expect("BM25 必须能查");
        assert!(hits.len() >= 2, "至少两条命中，实际 {}", hits.len());
        // 分数必须**单调不增**——这就是「按相关性排序」的可测形式。
        // 只断言「条数对」的话，一个恒返回原顺序的实现照样能过。
        for w in hits.windows(2) {
            assert!(
                w[0].score >= w[1].score,
                "BM25 结果未按分数降序：{:?} 然后 {:?}",
                w[0],
                w[1]
            );
        }
        // 名字里就有「时序图」的那条必须排在只靠描述命中的前面。
        assert_eq!(hits[0].id, "s::exact", "名字精确命中的应排第一，实际 {:?}", hits);
        assert_eq!(hits[1].id, "s::multi");
        // 毫不相干的必须不在结果里。
        assert!(!hits.iter().any(|h| h.id == "s::noise"));
    }

    #[test]
    fn bm25_recovers_what_substring_matching_cannot() {
        // 「时序」两个字在两条里都出现，但都不是完整子串「时序图」——
        // 结构化基线的 LIKE '%时序图%' 只能命中 s::exact。
        let store = demo_store();
        let dir = scratch("bm25-vs-like");
        let idx = FullTextIndex::attach(&dir, &store, SyncPolicy::Auto);

        let like_hits = store.query(&AssetFilter {
            kind: None,
            min_authority: None,
            tag: None,
            text: Some("时序图".into()),
        }, 10);
        assert_eq!(like_hits.len(), 1, "对照组：整串 LIKE 只能命中 1 条");

        let bm = idx.search("时序图", 10).unwrap();
        assert!(bm.len() > like_hits.len(), "BM25 应当比整串 LIKE 召回更多");
        assert!(bm.iter().any(|h| h.id == "s::multi"));
    }

    #[test]
    fn query_with_underscore_is_not_swallowed_as_one_word() {
        // 回归测试：曾经用 tantivy QueryParser，它把 `_` 当单词字符，
        // 于是 `渲染_容量规划` 整个被当成一个词送进分析器 -> 召回 0 条，
        // 而 `渲染 容量规划` 能召回 20 条。**不报错、不告警，只是搜不到。**
        // 本项目语料里 mcp-server / cost_tier=low 都是这种词形。
        let dir = scratch("underscore");
        let store = demo_store();
        let idx = FullTextIndex::attach(&dir, &store, SyncPolicy::Auto);
        let with_space = idx.search("时序 图", 10).unwrap();
        let with_underscore = idx.search("时序_图", 10).unwrap();
        assert!(!with_space.is_empty(), "对照组：空格分隔必须有召回");
        assert_eq!(
            with_underscore.len(),
            with_space.len(),
            "下划线分隔必须与空格分隔召回同样的结果"
        );
    }

    #[test]
    fn empty_or_punctuation_only_query_returns_nothing_without_erroring() {
        let dir = scratch("empty-query");
        let idx = FullTextIndex::attach(&dir, &demo_store(), SyncPolicy::Auto);
        assert!(idx.search("", 10).unwrap().is_empty());
        assert!(idx.search("   ", 10).unwrap().is_empty());
        // 查询语法字符不得让查询炸掉（用户会在搜索框里敲它们）。
        assert!(idx.search("C++ (a:b) {c}", 10).is_ok());
    }

    // ── 四种降级形态 ────────────────────────────────────────────
    //
    // M2 的承诺是「**索引没了检索也还在**」。这句话有四种破法，
    // 每一种都必须：结构化结果照样回来 + FullText 如实报 failed/absent + 带原因。
    // 这里把四种做成**同一组断言**跑四遍——它们必须是同一条性质，
    // 而不是四种碰巧都能用的实现。

    use crate::health::{DegradationReport, Stage};
    use crate::pipeline::{RetrievalEngine, SearchQuery};

    /// 一条查询，同时断言「结果没丢」与「状态没说谎」。
    fn assert_degraded_to_structured(reason_tag: &str, report: &DegradationReport, hits: &[AssetRow]) {
        assert!(
            !report.used.contains(&Stage::FullText),
            "{reason_tag}: 全文层不可用时绝不能被写进 used —— 那是谎报成功"
        );
        let st = report
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::FullText)
            .map(|(_, st)| st.clone())
            .unwrap_or_else(|| panic!("{reason_tag}: 降级报告里必须显式提到 FullText"));
        match st {
            StageState::Failed(msg) => assert!(
                !msg.is_empty(),
                "{reason_tag}: Failed 必须带原因，空消息等于没有归因"
            ),
            StageState::Absent => {}
            other => panic!("{reason_tag}: 全文层不可用时不应报 {other}"),
        }
        // 基线必须仍在 used，且真的产出了行。
        assert!(
            report.used.contains(&Stage::Structured),
            "{reason_tag}: 结构化基线必须照常交付，实际报告：{}",
            report.summary()
        );
        assert!(!report.baseline_failed(), "{reason_tag}: 基线不该被牵连");
        assert!(
            !hits.is_empty(),
            "{reason_tag}: 全文层坏了也必须返回结构化结果，实际 0 条"
        );
    }

    fn query_with(idx: FullTextIndex, store: InMemoryStore, q: &str) -> crate::SearchResult {
        let engine = RetrievalEngine::structured_only(store).with_fulltext(std::sync::Arc::new(idx));
        engine.search(&SearchQuery {
            filter: AssetFilter::all(),
            text: Some(q.to_string()),
            limit: 10,
        })
    }

    #[test]
    fn degradation_mode_1_index_absent() {
        let dir = scratch("deg-absent");
        let store = demo_store();
        // 只读挂载 + 目录空 = 索引不存在。
        let idx = FullTextIndex::attach(&dir, &store, SyncPolicy::Manual);
        let r = query_with(idx, store, "时序图");
        assert_degraded_to_structured("索引不存在", &r.degradation, &r.hits);
        let st = r
            .degradation
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::FullText)
            .unwrap();
        assert!(
            st.1.to_string().contains("不存在"),
            "原因必须指向「索引不存在」，实际：{}",
            st.1
        );
    }

    #[test]
    fn degradation_mode_2_index_stale() {
        let dir = scratch("deg-stale");
        let v1 = demo_store();
        // 先按 v1 建好索引。
        let built = FullTextIndex::attach(&dir, &v1, SyncPolicy::Auto);
        assert!(built.state().is_usable());

        // 库变了：加一条资产 -> 指纹必变 -> 索引变旧。
        let mut rows = demo_rows();
        rows.push(row("s::new", "新资产", "时序图相关的新组件"));
        let v2 = InMemoryStore::new(rows);

        let idx = FullTextIndex::attach(&dir, &v2, SyncPolicy::Manual);
        let r = query_with(idx, v2, "时序图");
        assert_degraded_to_structured("索引过期", &r.degradation, &r.hits);
        let st = r
            .degradation
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::FullText)
            .unwrap();
        assert!(
            st.1.to_string().contains("过期"),
            "原因必须指向「过期」，实际：{}",
            st.1
        );
        // 过期索引**不得**把库里那条新资产漏出来又假装没有——
        // 它必须只返回结构化（含 LIKE）能看到的东西。
        assert!(
            r.hits.iter().any(|h| h.id == "s::new"),
            "新资产走结构化 LIKE 应当命中：{}",
            r.degradation.summary()
        );
    }

    #[test]
    fn degradation_mode_3_index_build_failed() {
        // 造一个「建索引必定失败」的路径：把索引目录的位置占成一个普通文件。
        let dir = scratch("deg-buildfail");
        let file_path = dir.join("blocked");
        std::fs::write(&file_path, b"not a directory").expect("写入占位文件");
        let store = demo_store();

        // Auto 策略会去重建 —— 重建同样必须失败，且如实报出来。
        let idx = FullTextIndex::attach(&file_path, &store, SyncPolicy::Auto);
        let r = query_with(idx, store, "时序图");
        assert_degraded_to_structured("索引构建失败", &r.degradation, &r.hits);
        let st = r
            .degradation
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::FullText)
            .unwrap();
        assert!(
            st.1.to_string().contains("重建失败"),
            "原因必须指向「重建失败」，实际：{}",
            st.1
        );
    }

    #[test]
    fn degradation_mode_4_index_corrupt() {
        let dir = scratch("deg-corrupt");
        let store = demo_store();
        assert!(FullTextIndex::attach(&dir, &store, SyncPolicy::Auto)
            .state()
            .is_usable());

        // 把 tantivy 自己的 meta.json 写成垃圾：这是最真实的损坏形态
        // （写盘中断 / 磁盘坏道 / 被别的程序截断）。
        std::fs::write(dir.join("meta.json"), b"{ NOT JSON AT ALL")
            .expect("故意写坏 meta.json");

        let idx = FullTextIndex::attach(&dir, &store, SyncPolicy::Manual);
        let r = query_with(idx, store, "时序图");
        assert_degraded_to_structured("索引损坏", &r.degradation, &r.hits);
        let st = r
            .degradation
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::FullText)
            .unwrap();
        assert!(
            st.1.to_string().contains("损坏"),
            "原因必须指向「损坏」，实际：{}",
            st.1
        );
    }

    #[test]
    fn degradation_mode_index_source_unavailable() {
        // 第五种：库本身读不出索引输入源（表没了/被锁）。
        // 它不是任务书列的四种之一，但同属「全文层不可用」，
        // 值得一并钉住——否则这一支将来很容易被人写成 unwrap。
        let broken = InMemoryStore::unhealthy(demo_rows());
        let dir = scratch("deg-nosource");
        let idx = FullTextIndex::attach(&dir, &broken, SyncPolicy::Auto);
        let r = query_with(idx, InMemoryStore::new(demo_rows()), "时序图");
        assert!(!r.degradation.used.contains(&Stage::FullText));
        let st = r
            .degradation
            .skipped
            .iter()
            .find(|(s, _)| *s == Stage::FullText)
            .unwrap();
        assert!(
            st.1.to_string().contains("输入源"),
            "原因必须指向「输入源不可用」，实际：{}",
            st.1
        );
    }

    #[test]
    fn auto_policy_self_heals_corruption_instead_of_leaving_search_dead() {
        // 全新机器上索引本来就不存在；Auto 策略必须**建出来**而不是
        // 「因为索引不存在所以检索不了」。这是 M2 承诺在冷启动方向的形状。
        let dir = scratch("self-heal");
        let store = demo_store();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), b"{ NOT JSON AT ALL").unwrap();

        let idx = FullTextIndex::attach(&dir, &store, SyncPolicy::Auto);
        assert!(
            idx.state().is_usable(),
            "Auto 策略遇到损坏索引应当重建而不是放弃，实际：{}",
            idx.state()
        );
        let r = query_with(idx, store, "时序图");
        assert!(r.degradation.used.contains(&Stage::FullText));
        assert!(!r.hits.is_empty());
    }

    #[test]
    fn no_free_text_query_reports_fulltext_absent_not_ready() {
        // 没有自由文本 = 全文层确实没有参与。它不能被算进 used，
        // 否则报告会声称「这次用了全文」而实际上一个词都没搜。
        let dir = scratch("notext");
        let store = demo_store();
        let idx = FullTextIndex::attach(&dir, &store, SyncPolicy::Auto);
        assert!(idx.state().is_usable());
        let engine = RetrievalEngine::structured_only(store).with_fulltext(std::sync::Arc::new(idx));
        let r = engine.search(&SearchQuery::new(AssetFilter::all(), 10));
        assert!(
            !r.degradation.used.contains(&Stage::FullText),
            "没有查询词却把 FullText 记成 used = 谎报参与"
        );
        assert!(r.degradation.skipped_stage(Stage::FullText));
        assert_eq!(r.hits.len(), 3, "结构化全量必须照常返回");
    }

    #[test]
    fn structured_filters_still_gate_recalled_candidates() {
        // 全文层只负责召回。kind 过滤必须仍然在候选上生效——
        // 否则「搜 skill 里画时序图的」会把 mcp 里的同名资产也放进来。
        let dir = scratch("filter-gate");
        let mut rows = demo_rows();
        let mut as_mcp = row("m::same", "时序图", "把消息流转画成图 时序图 时序");
        as_mcp.kind = "mcp".to_string();
        rows.push(as_mcp);
        let store = InMemoryStore::new(rows);
        let idx = FullTextIndex::attach(&dir, &store, SyncPolicy::Auto);

        let engine = RetrievalEngine::structured_only(store).with_fulltext(std::sync::Arc::new(idx));
        let mut f = AssetFilter::all();
        f.kind = Some("skill".into());
        let r = engine.search(&SearchQuery {
            filter: f,
            text: Some("时序图".into()),
            limit: 10,
        });
        assert!(
            r.hits.iter().all(|h| h.kind == "skill"),
            "BM25 召回的 mcp 资产必须被结构化 kind 过滤挡掉：{:?}",
            r.hits.iter().map(|h| (&h.id, &h.kind)).collect::<Vec<_>>()
        );
        assert!(r.degradation.used.contains(&Stage::FullText));
    }

    #[test]
    fn reranked_hits_are_ordered_by_score_not_by_insertion_order() {
        let dir = scratch("rerank-order");
        let store = demo_store();
        let idx = FullTextIndex::attach(&dir, &store, SyncPolicy::Auto);
        let engine = RetrievalEngine::structured_only(store).with_fulltext(std::sync::Arc::new(idx));
        let r = engine.search(&SearchQuery {
            filter: AssetFilter::all(),
            text: Some("时序图".into()),
            limit: 10,
        });
        // s::noise 里既没有「时序」也没有「图」这两个词（只有 备份/恢复），
        // 所以本查询的正确答案就是 2 条：一条靠名字、一条靠正文分词。
        assert_eq!(
            r.hits.len(),
            2,
            "应当召回 2 条（名字命中的 + 正文分词命中的），实际 {:?}：{}",
            r.hits.iter().map(|h| &h.id).collect::<Vec<_>>(),
            r.degradation.summary()
        );
        assert_eq!(
            r.hits[0].id, "s::exact",
            "名字精确命中的「时序图」必须排第一（精排的 name_hit 分量），实际 {:?}",
            r.hits.iter().map(|h| &h.id).collect::<Vec<_>>()
        );
    }

    // ── 增量更新 ────────────────────────────────────────────────

    #[test]
    fn changing_one_asset_rewrites_exactly_one_document() {
        let dir = scratch("incr-one");
        let v1 = demo_store();
        let mut idx = FullTextIndex::attach(&dir, &v1, SyncPolicy::Auto);

        let mut rows = demo_rows();
        rows[1].text.push_str(" zzuniqueonlyhere");
        let v2 = InMemoryStore::new(rows);

        let rep = idx.sync(&v2).expect("增量同步必须成功");
        assert_eq!(rep.added, 0, "不该有新资产");
        assert_eq!(rep.deleted, 0, "不该有删除");
        assert_eq!(rep.updated, 1, "只应重写改动的那一条");
        assert_eq!(rep.rewritten, 1, "全库 3 条里只重写 1 条");
        assert_eq!(rep.unchanged, 2);
        assert_eq!(rep.total, 3);
    }

    #[test]
    fn an_unchanged_rescan_rewrites_nothing_at_all() {
        let dir = scratch("incr-noop");
        let store = demo_store();
        let mut idx = FullTextIndex::attach(&dir, &store, SyncPolicy::Auto);
        let rep = idx.sync(&store).expect("重扫必须成功");
        assert_eq!(rep.rewritten, 0, "内容没变却重写了文档 = 增量闸门失效");
        assert_eq!(rep.unchanged, 3);
    }

    #[test]
    fn added_and_deleted_assets_are_reflected_in_the_index() {
        let dir = scratch("incr-adddel");
        let v1 = demo_store();
        let mut idx = FullTextIndex::attach(&dir, &v1, SyncPolicy::Auto);

        let mut rows = demo_rows();
        rows.push(row("s::fresh", "全新资产", "刚刚加入的时序图工具"));
        let v2 = InMemoryStore::new(rows);
        let rep = idx.sync(&v2).unwrap();
        assert_eq!(rep.added, 1);
        assert!(idx
            .search("全新资产", 5)
            .unwrap()
            .iter()
            .any(|s| s.id == "s::fresh"));

        let rep = idx.sync(&v1).unwrap();
        assert_eq!(rep.deleted, 1, "库里删掉的资产必须从索引里删掉");
        assert!(
            !idx.search("全新资产", 5).unwrap().iter().any(|s| s.id == "s::fresh"),
            "已删除的资产不得还能被召回"
        );
    }

    #[test]
    fn updated_document_appears_exactly_once_not_twice() {
        // tantivy 是追加写：更新一条时若只 add 不 delete，
        // 同一个 id 会在倒排里出现两次，搜索结果里同一资产列两行。
        let dir = scratch("incr-dup");
        let v1 = demo_store();
        let mut idx = FullTextIndex::attach(&dir, &v1, SyncPolicy::Auto);

        let mut rows = demo_rows();
        rows[1].text.push_str(" zzduplicateprobe");
        let v2 = InMemoryStore::new(rows);
        idx.sync(&v2).unwrap();

        let hits = idx.search("zzduplicateprobe", 10).unwrap();
        assert_eq!(idx.doc_count(), 3, "账本里应当仍是 3 条");
        assert_eq!(
            hits.iter().filter(|h| h.id == "s::multi").count(),
            1,
            "同一条资产不得被召回两次，实际命中：{:?}",
            hits
        );
    }

    #[test]
    fn index_directory_sits_next_to_the_database_file() {
        // 索引必须挂在 db 旁边，才能自动继承桌面端的 app_data_dir_override
        // （本机从 C:\external-assets\ 启动的进程写 %USERPROFILE% 会被拒）。
        let db = Path::new(r"C:\somewhere\data\registry.db");
        assert_eq!(
            FullTextIndex::index_dir_for(db),
            PathBuf::from(r"C:\somewhere\data").join(INDEX_DIR_NAME)
        );
        // 没有父目录时也要给出一个可用路径，而不是 panic。
        let bare = Path::new("registry.db");
        assert_eq!(
            FullTextIndex::index_dir_for(bare),
            PathBuf::from(INDEX_DIR_NAME)
        );
    }

}

