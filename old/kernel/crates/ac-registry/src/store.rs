//! SQLite 存储。
//!
//! 设计要点：
//! - **单文件、零运维**：`data/capabilitycore.db`，双击可删即重建
//! - **增量索引**：`content_hash` 相同则跳过写入，只更新 `last_seen`
//! - **全文检索下推**：把可检索文本拼成一列，交给 SQLite FTS5 建索引
//!   （M2 会换�� tantivy，这张表仍作为真值来源）

use ac_core::{CapabilityManifest, DedupCandidate, DedupDecision, MatchLevel};
use rusqlite::{params, Connection};
use std::path::Path;

const PRAGMAS: &str = "PRAGMA journal_mode = WAL;\nPRAGMA foreign_keys = ON;";

const ASSETS_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS assets (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    name         TEXT NOT NULL,
    version      TEXT NOT NULL DEFAULT '0',
    description  TEXT NOT NULL DEFAULT '',
    manifest     TEXT NOT NULL,
    content_hash TEXT,
    source_path  TEXT,
    source_format TEXT,
    identity     TEXT NOT NULL DEFAULT '{}',
    -- 身份偏序枚举从 identity JSON 里拆成独立列：
    -- M2/M3 的检索过滤要下推到 SQL，不能每行反序列化一次 JSON。
    -- 覆盖值写进 manual_identity，推断值只作为默认值。
    authority     TEXT NOT NULL DEFAULT 'read',
    side_effect   TEXT NOT NULL DEFAULT 'none',
    tool_posture  TEXT NOT NULL DEFAULT 'pure',
    role          TEXT,
    search_blob  TEXT NOT NULL DEFAULT '',
    first_seen   INTEGER NOT NULL,
    last_seen    INTEGER NOT NULL,
    -- 解析器修订号，见 PARSER_REV
    parser_rev   INTEGER NOT NULL DEFAULT 0,
    manual_identity INTEGER NOT NULL DEFAULT 0,
    -- ── v7（2026-10-18）：用户点名的「来源 / 类型」两个维度 ──
    -- 判据与回填逻辑见 `origin` 模块；这里只负责存。
    -- 全部带 DEFAULT，因此老库 ALTER 加列后既有行立刻可读、且不会 NULL。
    --
    -- origin_kind        vendor | self_built | nurtured（见 OriginKind）
    -- registration_kind  embedded | mounted | remote（见 RegistrationKind）
    -- origin_agent       推断出的宿主生态；判不出来是**空串**不是占位值
    -- self_contained     0/1：删掉包外的东西它还能不能跑
    origin_kind       TEXT NOT NULL DEFAULT '',
    registration_kind TEXT NOT NULL DEFAULT '',
    origin_agent      TEXT NOT NULL DEFAULT '',
    self_contained    INTEGER NOT NULL DEFAULT 0,
    -- ── v8（2026-10-18）：资产生命周期 + 作者（含出处）──
    --
    -- lifecycle  active | paused | archived。用户的「删除」= 置 archived，
    --            **绝不物理删行**（项目铁律：增量不删）。默认 active，
    --            所以老库 ALTER 之后既有行立刻是合法值，不存在 NULL 窗口期。
    -- author     作者名。判不出来是**空串**，绝不写 'unknown' 占位。
    -- author_source 作者出处：manifest | frontmatter | package | '' 空串。
    --            没有出处的作者是一个凭空字符串，用户点开属性栏没法判断可信度。
    lifecycle         TEXT NOT NULL DEFAULT 'active',
    author            TEXT NOT NULL DEFAULT '',
    author_source     TEXT NOT NULL DEFAULT ''
)"#;

const CAPS_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS capabilities (
    capability_id TEXT PRIMARY KEY,
    asset_id      TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    summary       TEXT NOT NULL DEFAULT '',
    tags          TEXT NOT NULL DEFAULT '[]'
)"#;

const REL_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS relations (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    src_id     TEXT NOT NULL,
    kind       TEXT NOT NULL,
    target_id  TEXT NOT NULL,
    narrowing  TEXT,
    max_depth  INTEGER
)"#;

/// 委派图：边关系单独建表，方便做环检测与深度遍历
const EDGE_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS delegation_edges (
    src      TEXT NOT NULL,
    dst      TEXT NOT NULL,
    max_depth INTEGER NOT NULL DEFAULT 8,
    PRIMARY KEY (src, dst)
)"#;

/// 依赖新列的索引单独放最后建：老库要先补列才能建它们。
const INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_assets_kind ON assets(kind)",
    "CREATE INDEX IF NOT EXISTS idx_assets_hash ON assets(content_hash)",
    "CREATE INDEX IF NOT EXISTS idx_assets_source ON assets(source_path)",
    "CREATE INDEX IF NOT EXISTS idx_cap_asset ON capabilities(asset_id)",
    "CREATE INDEX IF NOT EXISTS idx_rel_src ON relations(src_id)",
    "CREATE INDEX IF NOT EXISTS idx_rel_target ON relations(target_id)",
    "CREATE INDEX IF NOT EXISTS idx_rel_kind ON relations(kind)",
    "CREATE INDEX IF NOT EXISTS idx_assets_auth ON assets(authority, tool_posture)",
    // M1.5
    "CREATE INDEX IF NOT EXISTS idx_logical_hash ON logical_assets(content_hash)",
    "CREATE INDEX IF NOT EXISTS idx_logical_norm ON logical_assets(normalized_hash)",
    "CREATE INDEX IF NOT EXISTS idx_loc_logical ON asset_locations(logical_id)",
    "CREATE INDEX IF NOT EXISTS idx_loc_role ON asset_locations(role)",
    "CREATE INDEX IF NOT EXISTS idx_loc_path ON asset_locations(source_path)",
    // 标签过滤要下推到 SQL：capabilities.tags 是 JSON 数组，
    // 用 json_each 展开成行表，标签筛选才能走索引而不是每行反序列化。
    "CREATE INDEX IF NOT EXISTS idx_cap_asset_id ON capabilities(asset_id)",
    // v7：来源/注册方式要能直接分组筛选（UI 左栏那两个维度）
    "CREATE INDEX IF NOT EXISTS idx_assets_origin ON assets(origin_kind, registration_kind)",
    "CREATE INDEX IF NOT EXISTS idx_assets_origin_agent ON assets(origin_agent)",
    // M3：总览要按生态聚合、要能按能力反查是哪个智能体拉的
    "CREATE INDEX IF NOT EXISTS idx_agents_ecosystem ON agents(ecosystem)",
    "CREATE INDEX IF NOT EXISTS idx_agents_transport ON agents(transport)",
    "CREATE INDEX IF NOT EXISTS idx_usage_capability ON agent_capability_usage(capability_id)",
    "CREATE INDEX IF NOT EXISTS idx_usage_asset ON agent_capability_usage(asset_id)",
    "CREATE INDEX IF NOT EXISTS idx_usage_source ON agent_capability_usage(source)",
    // v8：列表默认按 detected + connected 排序，属性栏要能按生命周期筛。
    // 两个索引都只依赖本轮补的列/表，因此必须放在补列之后建。
    "CREATE INDEX IF NOT EXISTS idx_assets_lifecycle ON assets(lifecycle)",
    "CREATE INDEX IF NOT EXISTS idx_assets_author ON assets(author_source)",
    "CREATE INDEX IF NOT EXISTS idx_hosts_detected ON agent_hosts(detected, connected)",
];

// ── M1.5：逻辑资产 + 位置集 ──────────────────────────────────────
//
// **只增不删**：assets 表的旧列（尤其 id）一律保留 6 个月给回滚，
// 见设计文档「### 5. 迁移路径」。
//
// logical_assets：一份内容 = 一行。
//   logical_id 由 L1 指纹派生（见 fingerprint::group_key），
//   所以"字节级相同"必然落进同一行——这才是"真重复"。
//   name/version/description 全部取自 canonical 位置那份。
const LOGICAL_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS logical_assets (
    logical_id           TEXT PRIMARY KEY,
    content_hash         TEXT NOT NULL,
    -- L2 规范化指纹。L1 不同但 L2 相同的资产仍是**独立** logical asset，
    -- 只会作为 MatchLevel::L2 的候选提示用户，绝不自动合并。
    normalized_hash      TEXT,
    kind                 TEXT NOT NULL,
    name                 TEXT NOT NULL,
    version              TEXT NOT NULL DEFAULT '0',
    description          TEXT NOT NULL DEFAULT '',
    canonical_location_id TEXT NOT NULL DEFAULT '',
    location_count       INTEGER NOT NULL DEFAULT 0,
    first_seen           INTEGER NOT NULL,
    last_seen            INTEGER NOT NULL
)"#;

// asset_locations：一个逻辑资产的每个磁盘位置 = 一行。
//   role = 'canonical' | 'alias'。默认 alias，**未经用户确认不升 canonical**。
//   last_seen_at 让"上次还在、这次没了"的位置留下可追溯的痕迹。
const LOCATION_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS asset_locations (
    location_id   TEXT PRIMARY KEY,
    logical_id    TEXT NOT NULL,
    source_path   TEXT NOT NULL,
    role          TEXT NOT NULL DEFAULT 'alias',
    last_seen_at  INTEGER NOT NULL,
    first_seen    INTEGER NOT NULL,
    -- 关联 M1 的 assets.id，保留以便回滚与交叉核对
    asset_id      TEXT,
    FOREIGN KEY (logical_id) REFERENCES logical_assets(logical_id) ON DELETE CASCADE
)"#;

// 待用户确认的去重候选队列。
//   同一组可能同时被 L1 和 L2 发现，用 (group_key, match_level) 唯一。
//   decision 为空 = 还没问过 / 用户选了 ignore（下次扫描重新入队）。
const DEDUP_QUEUE_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS dedup_candidates (
    group_key            TEXT NOT NULL,
    match_level          TEXT NOT NULL,
    suggested_canonical  TEXT NOT NULL DEFAULT '',
    location_ids         TEXT NOT NULL DEFAULT '[]',
    decision             TEXT,
    decided_at           INTEGER,
    first_detected_at    INTEGER NOT NULL,
    -- 用户裁决后置位，不再重复打扰
    resolved             INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_key, match_level)
)"#;

// ── M3 产品化：智能体接入（A / B / C 三通道共用这三张表）────────────
//
// **只增不删。** 本轮没有任何 DROP / ALTER 落在既有表上，
// 因此 506 条资产 / 495 条能力 / 464 个逻辑资产在任何迁移路径下都不变。
// 回滚 = `rollback_v5`（只删这三张新表，其余表一个字节都不碰）。
//
// 三张表刻意分成「谁 / 活着吗 / 用了什么」：
//   agents                   —— 智能体本体（静态生态 / 心跳宿主 / MCP 注册端）
//   agent_heartbeat          —— liveness，只对 B 通道有意义
//   agent_capability_usage   —— 谁拉了哪项能力；source 列区分「声明」与「实报」
//
// 为什么 usage 不用「一张表装下所有」：static 通道只能从磁盘路径**推断**
// 归属（source='static'，call_count 恒为 0），而 A 通道是宿主**实报**
// （source='mcp'，call_count 累计）。两者混在一列里会让人把"推断"当成"实测"。
// source 列就是让上层一眼分清这两种语义的唯一依据。
const AGENTS_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS agents (
    agent_id     TEXT PRIMARY KEY,
    display_name TEXT NOT NULL DEFAULT '',
    -- 生态键：qoder / claude / codex / cursor / kimi / zcode / dsh / unassigned
    ecosystem    TEXT NOT NULL DEFAULT 'unassigned',
    kind         TEXT NOT NULL DEFAULT 'host',
    -- 接入通道：static | heartbeat | mcp
    transport    TEXT NOT NULL DEFAULT 'static',
    endpoint     TEXT NOT NULL DEFAULT '',
    first_seen   INTEGER NOT NULL,
    last_seen    INTEGER NOT NULL,
    meta         TEXT NOT NULL DEFAULT '{}'
)"#;

const AGENT_HEARTBEAT_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS agent_heartbeat (
    agent_id       TEXT PRIMARY KEY,
    host           TEXT NOT NULL DEFAULT '',
    version        TEXT NOT NULL DEFAULT '',
    pid            INTEGER,
    last_beat      INTEGER NOT NULL,
    source_file    TEXT NOT NULL DEFAULT '',
    -- online | stale | offline | unknown：按 last_beat 与当前时间的差算出来的
    observed_state TEXT NOT NULL DEFAULT 'unknown',
    capabilities   TEXT NOT NULL DEFAULT '[]',
    FOREIGN KEY (agent_id) REFERENCES agents(agent_id) ON DELETE CASCADE
)"#;

const AGENT_USAGE_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS agent_capability_usage (
    agent_id      TEXT NOT NULL,
    capability_id TEXT NOT NULL,
    asset_id      TEXT NOT NULL DEFAULT '',
    -- static（磁盘推断）| heartbeat（心跳文件自报）| mcp（端点实报）
    source        TEXT NOT NULL DEFAULT 'static',
    call_count    INTEGER NOT NULL DEFAULT 0,
    first_used_at INTEGER NOT NULL,
    last_used_at  INTEGER NOT NULL,
    last_error    TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (agent_id, capability_id)
)"#;

// ── v8（2026-10-18）：智能体协议注册表 ─────────────────────────────
//
// **只增不删**：本轮没有 DROP、没有重写既有行，因此 506 条资产 / 495 条能力
// / 464 个逻辑资产在任何迁移路径下不变。回滚面见 [rollback_v8]。
//
// 与 M3 的 agents 表的关系（**刻意分开，不是重复建表**）：
//   agents       —— 此刻**接入了中枢**的智能体（静态归属 / 心跳 / MCP 注册），
//                   有 last_seen、有通道、有实报调用。回答「谁在用我」。
//   agent_hosts  —— 中枢**认知里存在的全部宿主**，含从未接入的。回答
//                   「我支持哪些智能体、它们各自支持哪些协议、装没装」。
//
// 一个宿主可以 detected=true（装了）而 connected=false（没接中枢）；
// 反过来不会出现。把这两件事塞进一张表，早晚会有人把「装了」读成「在用」。
const AGENT_HOSTS_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS agent_hosts (
    -- kebab-case 稳定 id：claude-code / deepseek-harness / …
    -- 这是对外契约（HTTP / MCP / 前端表格都拿它做主键），改它等于破坏契约。
    id            TEXT PRIMARY KEY,
    display_name  TEXT NOT NULL DEFAULT '',
    vendor        TEXT NOT NULL DEFAULT '',
    -- plugin | cli | mcp | harness
    kind          TEXT NOT NULL DEFAULT 'harness',
    -- 本机是否**装了**它。必须有证据才 true，判不出来就是 0（见 hosts::detect）。
    detected      INTEGER NOT NULL DEFAULT 0,
    -- 此刻是否**接着**中枢。由 agents 表 / 心跳目录判定。
    connected     INTEGER NOT NULL DEFAULT 0,
    -- unix 秒；0 = 从未见过。前端按 0 显示「从未接入」。
    last_seen     INTEGER NOT NULL DEFAULT 0,
    -- Vec<HostProtocol> 整份 JSON。刻意不拆列：协议表是**一张表**，
    -- 拆成行之后「这个宿主有哪几条协议」要 GROUP BY 回来，而 UI 一次只渲染一行。
    protocols_json TEXT NOT NULL DEFAULT '[]',
    -- 判定依据（查了哪些路径 / 命中哪条）。detected 为假时这里写清楚**查过哪里**，
    -- 否则用户看到一个 false 无从判断是"没装"还是"我们没找"。
    evidence_json TEXT NOT NULL DEFAULT '[]',
    updated_at    INTEGER NOT NULL DEFAULT 0
)"#;

/// M1.5 一次性回填（设计文档「### 5. 迁移路径」里的那条一次性脚本）。
///
/// 设计文档写的是：
/// `UPDATE assets SET logical_id = content_hash WHERE content_hash IS NULL`
///
/// 但那条 SQL 有两个问题，直接照抄会出错：
/// 1. **方向反了**。要填的是 `logical_id IS NULL`（新列），
///    而不是 `content_hash IS NULL`（老列、且通常非空）。
/// 2. **值域不对**。`logical_id` 必须与 `logical_assets.logical_id` 对得上，
///    而后者是 `l1-<hash>` 前缀形式，不是裸 hash。
///
/// 所以这里展开成完整回填：
/// - 有 content_hash 的 → logical_id = 'l1-' || content_hash，
///   并建出对应的 logical_assets 行（含 canonical_location_id）；
/// - 没有 content_hash 的（极老库）→ 退化为 'raw-' || assets.id，
///   **一行一个逻辑资产**，绝不因为缺指纹就丢资产。
const BACKFILL_LOGICAL: &str = r#"
-- 1) 每个有指纹的资产一个逻辑资产；无指纹的一行一个（保底）
INSERT OR IGNORE INTO logical_assets
    (logical_id, content_hash, normalized_hash, kind, name, version, description,
     canonical_location_id, location_count, first_seen, last_seen)
SELECT
    CASE
        WHEN a.content_hash IS NOT NULL AND a.content_hash <> ''
            THEN 'l1-' || a.content_hash
        ELSE 'raw-' || a.id
    END,
    COALESCE(a.content_hash, ''),
    NULL,
    a.kind,
    a.name,
    a.version,
    a.description,
    '',
    0,
    MIN(a.first_seen),
    MAX(a.last_seen)
FROM assets a
GROUP BY CASE
    WHEN a.content_hash IS NOT NULL AND a.content_hash <> ''
        THEN 'l1-' || a.content_hash
    ELSE 'raw-' || a.id
END;

-- 2) 每个资产一个位置；组内 source_path 最短的那条当 canonical。
--    注意这是**迁移期**的占位规则，不是去重裁决——真正的裁决走 UI 确认队列。
INSERT OR IGNORE INTO asset_locations
    (location_id, logical_id, source_path, role, last_seen_at, first_seen, asset_id)
SELECT
    'loc-' || a.id,
    CASE
        WHEN a.content_hash IS NOT NULL AND a.content_hash <> ''
            THEN 'l1-' || a.content_hash
        ELSE 'raw-' || a.id
    END,
    COALESCE(a.source_path, ''),
    CASE WHEN a.id = (
        SELECT b.id FROM assets b
        WHERE COALESCE(b.content_hash, '') = COALESCE(a.content_hash, '')
          AND (CASE WHEN b.content_hash IS NOT NULL AND b.content_hash <> ''
                   THEN 'l1-' || b.content_hash ELSE 'raw-' || b.id END)
              = (CASE WHEN a.content_hash IS NOT NULL AND a.content_hash <> ''
                   THEN 'l1-' || a.content_hash ELSE 'raw-' || a.id END)
        ORDER BY LENGTH(COALESCE(b.source_path, '')), b.id
        LIMIT 1
    ) THEN 'canonical' ELSE 'alias' END,
    a.last_seen,
    a.first_seen,
    a.id
FROM assets a;

-- 3) 回填 assets 上的两列，让老行能连到新表
UPDATE assets SET
    logical_id = CASE
        WHEN content_hash IS NOT NULL AND content_hash <> '' THEN 'l1-' || content_hash
        ELSE 'raw-' || id END,
    location_id = 'loc-' || id
WHERE logical_id IS NULL OR location_id IS NULL;

-- 4) 逻辑资产回填规范位置与位置数
UPDATE logical_assets SET
    canonical_location_id = COALESCE((
        SELECT l.location_id FROM asset_locations l
        WHERE l.logical_id = logical_assets.logical_id AND l.role = 'canonical'
        LIMIT 1
    ), ''),
    location_count = (
        SELECT COUNT(*) FROM asset_locations l WHERE l.logical_id = logical_assets.logical_id
    )
WHERE canonical_location_id = '';
"#;

// ── M4：故障定位与纳管 ─────────────────────────────────────────────
//
// 两张表，语义刻意分开：
//   asset_faults      —— **当前**检测到的故障（每次检测整体替换该资产的记录）
//   fault_repairs     —— **历史**：谁在什么时候认领 / 修好了 / 判定不可修
//
// 为什么不合成一张表：当前状态要能被一次检测整体覆盖（否则已修好的故障
// 会永远挂在清单上），而历史绝不能被覆盖（那是在抹掉审计）。
// 一张表做不到两种语义，硬合只会让其中一种变成谎言。
//
// 只增不删：本轮不 ALTER 任何既有表，不动 assets / capabilities，
// 因此 506 条资产在任何迁移路径下不变。
const ASSET_FAULTS_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS asset_faults (
    fault_id        TEXT PRIMARY KEY,
    asset_id        TEXT NOT NULL,
    kind            TEXT NOT NULL DEFAULT 'mcp',
    asset_name      TEXT NOT NULL DEFAULT '',
    server          TEXT,
    failure_class   TEXT NOT NULL,
    -- 同一处还命中的其它分类，JSON 数组（含 also_detected）
    classes         TEXT NOT NULL DEFAULT '[]',
    -- 定位详情整份 JSON：field / value / source_file / evidence / prerequisites
    -- 刻意不拆列：这份结构的读者是**智能体**，它要的是完整上下文，
    -- 而拆成列之后每一列单独看都不足以让它接手。
    location        TEXT NOT NULL DEFAULT '{}',
    evidence        TEXT NOT NULL DEFAULT '[]',
    prerequisites   TEXT NOT NULL DEFAULT '[]',
    suggested_repair_kind TEXT NOT NULL DEFAULT 'none',
    -- pending（待修）| repairing（已被认领）| fixed（已修）| unrepairable（不可修）
    status          TEXT NOT NULL DEFAULT 'pending',
    -- 认领者：复用 agents.agent_id，空串表示无人认领
    claimed_by      TEXT NOT NULL DEFAULT '',
    claimed_at      INTEGER,
    detected_at     INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
)"#;

const FAULT_REPAIRS_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS fault_repairs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    fault_id    TEXT NOT NULL,
    asset_id    TEXT NOT NULL,
    -- claim | start | fixed | unrepairable | release | note
    action      TEXT NOT NULL,
    actor       TEXT NOT NULL DEFAULT '',
    note        TEXT NOT NULL DEFAULT '',
    at          INTEGER NOT NULL
)"#;

/// 已纳管的能力实体（assets/ 目录下真正能执行的那一份）。
///
/// 与 assets 表的关系：assets 记录「磁盘上发现了什么」，
/// 本表记录「中枢自己纳管了什么」。两者**不是**同一件事——
/// 一个资产可以被发现但没被纳管（比如 vendor/ 里的原件），
/// 而纳管实体必然有对应的资产 id。
const ASSET_ENTRIES_TABLE: &str = r#"CREATE TABLE IF NOT EXISTS asset_entries (
    asset_id     TEXT PRIMARY KEY,
    -- mcp | skill
    kind         TEXT NOT NULL,
    -- assets/ 下的相对路径，例如 mcp/obsidian-suite
    rel_path     TEXT NOT NULL,
    -- 纳管实体的启动命令（mcp）或入口文件（skill），空串表示未登记
    entry        TEXT NOT NULL DEFAULT '',
    source_asset TEXT NOT NULL DEFAULT '',
    managed_at   INTEGER NOT NULL,
    verified_at  INTEGER,
    note         TEXT NOT NULL DEFAULT ''
)"#;

const FAULT_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_faults_asset ON asset_faults(asset_id)",
    "CREATE INDEX IF NOT EXISTS idx_faults_class ON asset_faults(failure_class)",
    "CREATE INDEX IF NOT EXISTS idx_faults_status ON asset_faults(status)",
    "CREATE INDEX IF NOT EXISTS idx_faults_claimed ON asset_faults(claimed_by)",
    "CREATE INDEX IF NOT EXISTS idx_repairs_fault ON fault_repairs(fault_id)",
    "CREATE INDEX IF NOT EXISTS idx_repairs_asset ON fault_repairs(asset_id)",
    "CREATE INDEX IF NOT EXISTS idx_assets_kind ON asset_entries(kind)",
];
const TABLES: &[&str] = &[
    ASSETS_TABLE,
    CAPS_TABLE,
    REL_TABLE,
    EDGE_TABLE,
    LOGICAL_TABLE,
    LOCATION_TABLE,
    DEDUP_QUEUE_TABLE,
    // M3：智能体接入三表。CREATE TABLE IF NOT EXISTS 本身就是幂等迁移，
    // 老库第一次打开就会补上，新库直接建齐——两种路径得到同一张表。
    AGENTS_TABLE,
    AGENT_HEARTBEAT_TABLE,
    AGENT_USAGE_TABLE,
    // M4：故障定位与纳管三表。同样是幂等迁移。
    ASSET_FAULTS_TABLE,
    FAULT_REPAIRS_TABLE,
    ASSET_ENTRIES_TABLE,
    // v8：智能体协议注册表。CREATE TABLE IF NOT EXISTS 本身幂等。
    AGENT_HOSTS_TABLE,
];

pub struct Registry {
    // pub(crate)：同 crate 内的兄弟模块（`retrieval_impl`）需要直接查
    // `capabilities` 才能取回每个资产的标签。仍然只在本 crate 内可见，
    // **不扩大对外 API**，也不触碰任何解析逻辑（PARSER_REV 不需要 bump）。
    pub(crate) conn: Connection,
}

/// 当前 SQLite schema 版本。每改一次表结构就 +1，
/// 并在 [`migrate`] 里补一段对应的 ALTER。
///
/// # v6 → v7（2026-10-18 数据层）
///
/// `assets` 加四列：`origin_kind` / `registration_kind` / `origin_agent` /
/// `self_contained`。**只加列，不改既有列、不删行**，与 v4→v5、v5→v6
/// 的「只增不删」是同一条纪律。
///
/// # v7 → v8（2026-10-18 能力中枢）
///
/// 两件事，同一条纪律（**只增不删**）：
///
/// 1. `assets` 加三列：`lifecycle`（active / paused / archived）、`author`、
///    `author_source`（manifest / frontmatter / package / 空串）。
///    用户的「删除资产」= 置 `archived`，**没有任何一行会被物理删除**。
/// 2. 新表 `agent_hosts`：智能体协议注册表。
///
/// 回滚面 = [`rollback_v8`]：删掉新表 + 那三列**只能留着**（SQLite 不支持
/// DROP COLUMN 的老版本，且既有行的值是用户状态，删列就是删用户数据）。
/// 因此 v8 的回滚是**代码层回滚**：v7 程序读 v8 库时，多出来的列被忽略。
pub const SCHEMA_VERSION: i64 = 8;

/// 解析器修订号。
///
/// **改动解析/推断逻辑时必须 +1。** 增量检测只比对 `content_hash`，
/// 而那是文件字节的指纹——源文件没动，改了 `parse_mcp_json` 的展示名，
/// 老资产照样会被判成 "unchanged" 跳过，于是新逻辑永远落不了地，
/// 用户看到的还是旧数据，而且没有任何报错。
///
/// # 修订历史
///
/// - 5（2026-10-18）：frontmatter 支持块标量（`|` / `>`）并按缩进区分
///   顶层键与嵌套键；MCP / plugin 解析开始收集 `runtime_refs`。
///   不 bump 的话，506 行内容没变的老资产会全部走 `unchanged` 分支，
///   50 条 `>` 垃圾描述和 6 条 `name=string` 永远留在库里。
///
/// - 6（2026-10-18，v8）：解析器开始抽取**作者自声明**——
///   `plugin.json` 的 `author`（字符串与 `{"name": …}` 两种写法）
///   与 SKILL.md frontmatter 的 `author:`，写进 `provenance.author` /
///   `provenance.author_source`。
///
///   **这里必须 +1，理由与 v5 那次完全相同**：506 份源文件一个字节都没变，
///   不 bump 就全部走 `unchanged` 分支，`provenance.author` 永远为空。
///   迁移里的 `backfill_author_columns` 只能补到 `package` 档（它拿不到文件，
///   而老 manifest 里根本没有 author 字段）；**自声明那一档只能靠重解析**。
///   不 bump 的后果是：界面上所有资产的作者都显示成包名，
///   而真实语料里 26 份 plugin.json / 4 份 SKILL.md 是有作者自声明的。
pub const PARSER_REV: i64 = 7;

fn user_version(conn: &Connection) -> Result<i64, String> {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| format!("读取 schema 版本失败: {e}"))
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| e.to_string())?;
    for c in cols {
        if c.map_err(|e| e.to_string())? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 建表 / 升级。
///
/// **判定依据是实际列形状，不是 `user_version`。**
/// 早期版本从没写过 `user_version`，那些老库的版本号也是 0——
/// 拿 0 当"全新库"处理，会直接 `CREATE TABLE IF NOT EXISTS` 空转，
/// 然后在缺列上建索引时报 `no such column`。而这恰恰是用户第一次
/// 升级时会发生的事。版本号只用来发现"库比程序新"这一种情况。
///
/// # v4 → v5（M3 产品化）
///
/// 本轮**只新增三张表**（`agents` / `agent_heartbeat` / `agent_capability_usage`），
/// 既有表一个字都没改：没有 ALTER、没有 DROP、没有重写既有行。因此：
///
/// - 迁移前后的 `assets` / `capabilities` / `logical_assets` / `asset_locations`
///   / `dedup_candidates` 计数**必然相同**（测试 `migration_v5_preserves_existing_counts`
///   就是在钉这一条）；
/// - 回滚面精确等于新增面，见 [`rollback_v5`]；
/// - 迁移是幂等的：`CREATE TABLE IF NOT EXISTS` 在 v4 库与全新库上得到同一张表，
///   所以不需要按版本号分支。
fn migrate(conn: &Connection) -> Result<(), String> {
    let v = user_version(conn)?;
    if v > SCHEMA_VERSION {
        return Err(format!(
            "数据库 schema 版本 {v} 比当前程序({SCHEMA_VERSION})新，请升级 capability-core"
        ));
    }

    conn.execute_batch(PRAGMAS)
        .map_err(|e| format!("设置 PRAGMA 失败: {e}"))?;

    // 建表：已存在的会被 IF NOT EXISTS 跳过
    for t in TABLES {
        conn.execute_batch(t)
            .map_err(|e| format!("建表失败: {e}"))?;
    }

    // 补列：新拆出来的身份偏序列
    for (col, decl) in [
        ("authority", "TEXT NOT NULL DEFAULT 'read'"),
        ("side_effect", "TEXT NOT NULL DEFAULT 'none'"),
        ("tool_posture", "TEXT NOT NULL DEFAULT 'pure'"),
        ("role", "TEXT"),
        ("parser_rev", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !has_column(conn, "assets", col)? {
            conn.execute(&format!("ALTER TABLE assets ADD COLUMN {col} {decl}"), [])
                .map_err(|e| format!("迁移添加列 {col} 失败: {e}"))?;
        }
    }

    // M1.5 补列：**只加不删**。旧 id / content_hash / source_path 一律保留，
    // 设计文档要求 id 列再留 6 个月给回滚。
    // 加在 TABLES 之后、索引之前——老库要先补列才能建依赖它们的索引。
    for (col, decl) in [
        // 指向 logical_assets.logical_id
        ("logical_id", "TEXT"),
        // 指向 asset_locations.location_id
        ("location_id", "TEXT"),
    ] {
        if !has_column(conn, "assets", col)? {
            conn.execute(&format!("ALTER TABLE assets ADD COLUMN {col} {decl}"), [])
                .map_err(|e| format!("迁移添加列 {col} 失败: {e}"))?;
        }
    }

    // v7 补列：来源 / 注册方式。同样是**只加不删**；全部带 DEFAULT，
    // 所以 ALTER 之后既有行立刻是可读的合法值，不存在 NULL 窗口期。
    for (col, decl) in [
        ("origin_kind", "TEXT NOT NULL DEFAULT ''"),
        ("registration_kind", "TEXT NOT NULL DEFAULT ''"),
        ("origin_agent", "TEXT NOT NULL DEFAULT ''"),
        ("self_contained", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !has_column(conn, "assets", col)? {
            conn.execute(&format!("ALTER TABLE assets ADD COLUMN {col} {decl}"), [])
                .map_err(|e| format!("迁移添加列 {col} 失败: {e}"))?;
        }
    }

    // v8 补列：资产生命周期 + 作者（含出处）。与 v7 同一条纪律——
    // **只加不删**，全部带 DEFAULT，ALTER 之后既有行立刻是可读的合法值。
    for (col, decl) in [
        // 默认 active：老库里的资产本来就是能用的那些，没有理由把它们判成暂停。
        ("lifecycle", "TEXT NOT NULL DEFAULT 'active'"),
        ("author", "TEXT NOT NULL DEFAULT ''"),
        ("author_source", "TEXT NOT NULL DEFAULT ''"),
    ] {
        if !has_column(conn, "assets", col)? {
            conn.execute(&format!("ALTER TABLE assets ADD COLUMN {col} {decl}"), [])
                .map_err(|e| format!("迁移添加列 {col} 失败: {e}"))?;
        }
    }

    // v7 一次性回填：老库的既有行不会走 upsert 的更新分支（内容没变 = unchanged），
    // 所以四列必须在这里补上，否则它们会永远停在 DEFAULT '' / 0。
    //
    // 判据必须走 `origin` 模块而不是在这里写 SQL：迁移、入库、测试三处必须是
    // 同一套规则，否则"库里的值和界面上的值不一样"就是必然的。
    // 代价是这里要把 manifest 读出来反序列化一次（506 行，一次性）。
    backfill_origin_columns(conn)?;

    // v8 一次性回填：作者三列。与 origin 同款理由——
    // 内容没变的老行走 unchanged 分支，作者列会永远停在 DEFAULT ''。
    //
    // 判据复用 `author` 模块（迁移 / 入库 / 测试三处同一套规则）。
    backfill_author_columns(conn)?;

    // 一次性回填：内容没变的资产走 "unchanged" 分支，不会被重写，
    // 所以它们的 logical_id 必须在这里补上，否则永远是 NULL。
    //
    // 语义：content_hash 非空的资产先按 L1 指纹归入 logical_assets；
    // 还拿不到指纹的（极老库 content_hash 全空）退化为 "每行一个逻辑资产"，
    // 保证**不丢数据**——宁可不去重，也不能让资产凭空消失。
    if has_identity_columns(conn)? {
        conn.execute_batch(BACKFILL_LOGICAL)
            .map_err(|e| format!("回填 logical_id 失败: {e}"))?;
    }

    // 回填：内容没变的资产在下次扫描时走 "unchanged" 分支，
    // 只刷 last_seen，不写新列——不回填的话它们会永远停在默认值。
    if has_identity_columns(conn)? {
        conn.execute_batch(
            "UPDATE assets SET
                authority    = COALESCE(json_extract(identity, '$.authority'), 'read'),
                side_effect  = COALESCE(json_extract(identity, '$.side_effect'), 'none'),
                tool_posture = COALESCE(json_extract(identity, '$.tool_posture'), 'pure'),
                role         = json_extract(identity, '$.role')
             WHERE identity IS NOT NULL AND identity <> ''",
        )
        .map_err(|e| format!("回填身份列失败: {e}"))?;
    }

    // 索引放最后：依赖上面补齐的列
    for idx in INDEXES {
        conn.execute_batch(idx)
            .map_err(|e| format!("建索引失败: {e}"))?;
    }
    // M4 的索引单独走一张表：它们只依赖本轮新建的表，
    // 与上面那批「依赖补列」的索引混在一起会让失败原因变模糊。
    for idx in FAULT_INDEXES {
        conn.execute_batch(idx)
            .map_err(|e| format!("建索引失败（{idx}）: {e}"))?;
    }

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// **v8 → v7 回滚**：只删 v8 新增的 `agent_hosts` 表。
///
/// 与 `rollback_v6` / `rollback_v5` 同一条纪律：本轮的新增面里，
/// **表**可以删（删掉等于"中枢不再认识这些宿主"，不影响资产），
/// 但 `assets` 上的三列**不删**——它们装的是用户状态
/// （某个资产被暂停 / 归档、某个作者是谁），删列就是删用户数据。
///
/// 所以 v8 的完整回滚是「删新表 + 代码降级回 v7」。v7 程序读 v8 库时
/// 只会看到几张不认识的表和几列不认识的列，SQL 全部按列名取，不会报错。
pub fn rollback_v8(conn: &Connection) -> Result<(), String> {
    for sql in [
        "DROP INDEX IF EXISTS idx_hosts_detected",
        "DROP TABLE IF EXISTS agent_hosts",
    ] {
        conn.execute_batch(sql)
            .map_err(|e| format!("回滚失败（{sql}）: {e}"))?;
    }
    conn.pragma_update(None, "user_version", 7)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// **v6 → v5 回滚**：只删 M4 新增的三张表。
///
/// 与 `rollback_v5` 同一条纪律：本轮 schema 变更全部是**新增表**，
/// 没有 ALTER 既有表、没有 DROP 任何既有列，所以回滚面精确等于新增面。
pub fn rollback_v6(conn: &Connection) -> Result<(), String> {
    for sql in [
        "DROP TABLE IF EXISTS asset_entries",
        "DROP TABLE IF EXISTS fault_repairs",
        "DROP TABLE IF EXISTS asset_faults",
    ] {
        conn.execute_batch(sql)
            .map_err(|e| format!("回滚失败（{sql}）: {e}"))?;
    }
    conn.pragma_update(None, "user_version", 5)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// **v5 → v4 回滚**：只删 M3 新增的三张表。
///
/// 为什么回滚能这么"便宜"：本轮的 schema 变更**全部是新增表**，
/// 没有 ALTER 既有表、没有 DROP 任何既有列。于是回滚面精确等于新增面——
/// 删掉这三张表，506 条资产 / 495 条能力 / 464 个逻辑资产 / 54 组去重候选
/// 一行都不受影响。这是"只增不删"这条纪律最直接的兑现。
///
/// [`migrate`] 里没有任何一处会调用它：程序**不会**自动回滚用户的库。
/// 它是给运维/测试用的显式出口，由调用方决定何时执行。
pub fn rollback_v5(conn: &Connection) -> Result<(), String> {
    // 顺序：先删依赖方（heartbeat / usage 外键指向 agents），再删被引用方。
    // 开着 PRAGMA foreign_keys 时反序会直接失败，所以这个顺序不是风格问题。
    for sql in [
        "DROP TABLE IF EXISTS agent_capability_usage",
        "DROP TABLE IF EXISTS agent_heartbeat",
        "DROP TABLE IF EXISTS agents",
    ] {
        conn.execute_batch(sql).map_err(|e| format!("回滚失败（{sql}）: {e}"))?;
    }
    conn.pragma_update(None, "user_version", 4)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// v7 回填：把 `origin_kind` / `registration_kind` / `origin_agent` /
/// `self_contained` 从既有的 `source_path` 与 `manifest` 里算出来写回。
///
/// # 为什么必须在迁移里做，而不是"等下次扫描自然会写"
///
/// 增量检测只看 `content_hash`（源文件字节指纹）。老库的 506 行内容一个字节
/// 都没变，下次扫描会全部走 `unchanged` 分支、**只刷 last_seen**，
/// 四列于是永远停在 `''` / `0`。用户点开界面看到的还是"没有来源"——
/// 这正是要修的那个缺陷本身。
///
/// # 为什么判据要复用 `origin` 模块
///
/// 迁移、入库、测试三处必须是同一套规则。在这里写一段 SQL 近似判定
/// （比如 `source_path LIKE '%vendor%'`）会立刻与解析期判定分叉：
/// `vendorized-notes/` 这种目录名就会让两边给出不同答案。
///
/// 幂等：已经写过非空值的行不会被覆盖（`WHERE origin_kind = ''`）。
fn backfill_origin_columns(conn: &Connection) -> Result<(), String> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM assets WHERE origin_kind = '' OR registration_kind = ''",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(());
    }

    // 一次性读出全部待回填行的 (id, manifest, source_path)。
    let rows: Vec<(String, String, Option<String>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, manifest, source_path FROM assets
                 WHERE origin_kind = '' OR registration_kind = ''",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?;
        it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };

    let mut upd = conn
        .prepare(
            "UPDATE assets SET origin_kind = ?2, registration_kind = ?3,
                origin_agent = ?4, self_contained = ?5 WHERE id = ?1",
        )
        .map_err(|e| e.to_string())?;
    for (id, raw, source_path) in rows {
        // manifest 反序列化失败不该让整次迁移失败：老库里有手工写进去的行，
        // 也有列被改过形状的行。退化成"只有路径可用"的判定，先落一版保守值。
        let profile = match serde_json::from_str::<CapabilityManifest>(&raw) {
            Ok(mut m) => {
                // ownership：路径列是权威，manifest 里那份是老写法的副本。
                if let Some(p) = source_path.filter(|p| !p.trim().is_empty()) {
                    m.provenance.source_path = Some(p);
                }
                crate::origin::classify_manifest(&m)
            }
            Err(_) => {
                let p = source_path.unwrap_or_default();
                crate::origin::classify(&p, &[])
            }
        };
        upd.execute(params![
            id,
            profile.origin_kind,
            profile.registration_kind,
            profile.origin_agent,
            profile.self_contained as i64
        ])
        .map_err(|e| format!("回填 {id} 的来源列失败: {e}"))?;
    }
    Ok(())
}

/// v8 回填：把 `author` / `author_source` 从既有的 `manifest` 与
/// `source_path` 里算出来写回。
///
/// # 为什么必须在迁移里做
///
/// 与 v7 的 `backfill_origin_columns` 同一条理由：增量检测只看 `content_hash`。
/// 老库的 506 行内容一个字节都没变，下次扫描会全部走 `unchanged` 分支、
/// 只刷 `last_seen`，两列于是永远停在 `''`——用户点开属性栏看到的还是
/// "没有作者"，也就是缺陷本身没修。
///
/// # 为什么作者名要从 manifest 里读，而不是重扫磁盘
///
/// `migrate()` 只拿得到数据库，拿不到文件。老库里 506 份 manifest 是
/// 旧解析器写的，**没有 author 字段**（实测：506 条里 0 条带 author）——
/// 这正是需要回填的原因。于是这里只能做**包路径兜底**那一档：
/// `vendor/<kind>/<pkg>/…` → `<pkg>`，出处如实记 `package`。
///
/// 自声明那一档要等下一次全量入库（PARSER_REV bump 之后会强制重解析）。
/// 也就是说：**迁移后作者数会立刻 > 0，但它是低档位的**；重扫之后
/// 自声明的那些会被提升到 manifest / frontmatter 档。这是刻意的分档，
/// 不是漏做——把包名伪装成作者自声明才是骗人。
///
/// 幂等：已经有非空作者的资产不会被覆盖（`WHERE author = ''`），
/// 否则每次启动都会把用户看到的自声明作者降级成包名。
fn backfill_author_columns(conn: &Connection) -> Result<(), String> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM assets WHERE author = ''",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(());
    }

    let rows: Vec<(String, String, String, Option<String>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, manifest, COALESCE(source_path, ''), source_path
                 FROM assets WHERE author = ''",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        it.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };

    let mut upd = conn
        .prepare("UPDATE assets SET author = ?2, author_source = ?3 WHERE id = ?1")
        .map_err(|e| e.to_string())?;
    for (id, raw, path, _sp) in rows {
        // manifest 反序列化失败不该让整次迁移失败（与 origin 回填同款取舍）：
        // 退化成「只有路径可用」的判定，先落一版保守值。
        let profile = match serde_json::from_str::<CapabilityManifest>(&raw) {
            Ok(m) => crate::author::infer_author(&m, &path),
            Err(_) => match crate::author::package_of(&path) {
                Some(pkg) => crate::author::AuthorProfile {
                    author: pkg,
                    source: crate::author::SOURCE_PACKAGE.to_string(),
                },
                None => crate::author::AuthorProfile::default(),
            },
        };
        if profile.is_empty() {
            // 判不出来就**不动**这一行：写空串等于把「查过了、判不出」
            // 也记成一次更新，下次迁移照样扫它，白白浪费一轮。
            continue;
        }
        upd.execute(params![id, profile.author, profile.source])
            .map_err(|e| format!("回填 {id} 的作者列失败: {e}"))?;
    }
    Ok(())
}

/// 刚补完列时为 false，跳过回填——空表回填没有意义。
fn has_identity_columns(conn: &Connection) -> Result<bool, String> {
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM assets", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UpsertResult {
    pub inserted: u32,
    pub updated: u32,
    pub unchanged: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KindCount {
    pub kind: String,
    pub count: u32,
}

/// 列表查询条件。
///
/// 过滤全部下推到 SQL：M1 之后资产量会上千，而 M2 的检索层也要
/// 复用同一套过滤（先按身份/类型收窄，再做相关性排序）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AssetQuery {
    /// 按资产类型过滤（skill / plugin / mcp / suite / ghost / organization）
    pub kind: Option<String>,
    /// 最低权限要求：只返回 authority 不低于该值的资产
    pub min_authority: Option<String>,
    /// 对搜索文本做包含匹配（已转义 LIKE 通配符）
    pub text: Option<String>,
    /// 按标签过滤：**任一能力**带这个标签即命中（OR 语义）。
    ///
    /// 标签的来源与取舍见本文件底部 `TAG_SEMANTICS`。
    pub tag: Option<String>,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
    /// v8：是否**连归档的一起**列出来。默认 false = 归档的不出现在列表里。
    ///
    /// # 为什么是 `include_archived` 而不是 `lifecycle: Option<String>`
    ///
    /// 用户的「删除」= 置 `archived`，而列表默认必须看不见被删的东西，
    /// 否则"删除"在界面上等于没发生。所以默认值就是过滤，不是"不过滤"。
    /// 想专门看归档资产时把它置 true，再由调用方按 lifecycle 自行分组。
    ///
    /// # 必须带 `#[serde(default)]`
    ///
    /// `apps/desktop/src-tauri/src/lib.rs` 用**逐字段结构体字面量**构造
    /// `AssetQuery`（见该文件 list_assets）。字段若没有默认值，那边会当场
    /// 编译失败——而本轮不碰 apps/。`#[serde(default)]` 同时保证
    /// 反序列化（HTTP / MCP 侧）缺字段时是 false 而不是报错。
    #[serde(default)]
    pub include_archived: bool,
}

/// 库内标签清单，供 UI 渲染筛选控件。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TagCount {
    pub tag: String,
    pub count: u32,
}

/// 一次 `sync_logical` 的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogicalSync {
    /// 本次新建的逻辑资产数
    pub logical_created: u32,
    /// 本次补写了 `normalized_hash` 的既有逻辑资产数。
    ///
    /// M1.5 v4 修复：升级老库时这一项会是 464（BACKFILL_LOGICAL 建出来的
    /// 464 行都把 normalized_hash 留成了 NULL），之后正常扫描应当恒为 0。
    pub logical_updated: u32,
    /// 本次见到（刷新过 last_seen）的位置数
    pub locations_seen: u32,
    /// 全库当前 alias 位置数（>0 即存在重复位置）
    pub alias_count: u32,
}

/// 一条已裁决的记录（审计用）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DedupRecord {
    pub group_key: String,
    pub match_level: String,
    pub decision: String,
    pub decided_at: i64,
    pub location_ids: Vec<String>,
}

/// 列表行。刻意只带 UI 要显示的字段，完整 manifest 走 `get_manifest`。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AssetSummary {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub authority: String,
    pub authority_label: String,
    pub tool_posture: String,
    pub side_effect: String,
    pub role: Option<String>,
    pub capability_count: u32,
    pub source_path: Option<String>,
    /// M1.5：所属逻辑资产（内容相同即同一份）
    pub logical_id: Option<String>,
    /// M1.5：同一逻辑资产下有几个磁盘位置（>1 说明内容重复）
    #[serde(default)]
    pub location_count: u32,
    /// M1.5：本位置在逻辑资产内的角色（canonical / alias）
    #[serde(default)]
    pub location_role: Option<String>,
    // ── v7：用户点名的「来源 / 类型」两个维度 ──
    //
    // 加在**行类型**上而不是查询条件里：用户要的是"看到"这两个维度。
    // `AssetQuery` 一个字段都没动 —— 它会被 apps/desktop 以结构体字面量构造，
    // 加字段会让那边当场编译失败（本轮不碰 apps/）。筛选器以后要加，
    // 那时是 `..Default::default()` 补一行的事，与本次的数据落地无关。
    /// vendor | self_built | nurtured
    #[serde(default)]
    pub origin_kind: String,
    /// embedded | mounted | remote
    #[serde(default)]
    pub registration_kind: String,
    /// 推断出的宿主生态；判不出来是空串
    #[serde(default)]
    pub origin_agent: String,
    /// 删掉包外的东西它还能不能跑
    #[serde(default)]
    pub self_contained: bool,
    // ── v8：生命周期 / 作者 / 宿主支持 ──
    //
    // 三个字段全部带 `#[serde(default)]`：apps/desktop 的 types.ts 与
    // preview.json 都是**按这个类型反序列化**的，缺字段时默认值必须成立，
    // 否则老前端连列表都打不开。
    /// active | paused | archived。用户的「删除」= archived（软删除）。
    #[serde(default)]
    pub lifecycle: String,
    /// 作者名；判不出来是空串（不是占位值）。
    #[serde(default)]
    pub author: String,
    /// 作者出处：manifest | frontmatter | package | 空串。
    ///
    /// 与 `author` 成对存在：前端按这一列决定属性栏显示
    /// 「作者（自声明）/ 作者（frontmatter）/ 作者（包推断）」，空串则不显示作者行。
    #[serde(default)]
    pub author_source: String,
    /// 哪些宿主智能体**能**用这个资产（宿主 id 列表，按字母序）。
    ///
    /// 判据见 `hosts::asset_host_support`：origin_agent + 路径标记目录 + 协议匹配，
    /// 三档全部有证据才认。空表 = 判不出来，**不是**"谁都不能用"。
    #[serde(default)]
    pub host_support: Vec<String>,
    /// 宿主支持的判定依据。字段名与 hosts 模块的 evidence 一致，
    /// 让用户能回答"凭什么说它支持 qoder"。
    #[serde(default)]
    pub host_support_basis: Vec<String>,
}

impl AssetQuery {
    fn clamp(&mut self) {
        self.limit = self.limit.clamp(1, 1000);
    }
}

/// LIKE 模式里 `%` / `_` 是通配符，用户输入必须转义，否则搜 "a_b" 会匹配到 "axb"。
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl Registry {
    /// 打开或创建数据库。
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败 {dir:?}: {e}"))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("打开数据库失败: {e}"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
        migrate(&conn)?;
        let reg = Self { conn };
        reg.init_operations()?;
        Ok(reg)
    }

    /// 内存库，供测试使用。
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        migrate(&conn)?;
        let reg = Self { conn };
        reg.init_operations()?;
        Ok(reg)
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// 写入或更新一个资产。
    ///
    /// 判「是否真的变了」看两样：`content_hash`（源文件字节指纹）
    /// **和** `PARSER_REV`（解析器修订号）。只看前者的话，改了解析逻辑
    /// 之后老资产会被永远判成 unchanged，用户看到的还是旧数据。
    pub fn upsert(&self, m: &CapabilityManifest) -> Result<&'static str, String> {
        let now = Self::now();
        let json = serde_json::to_string(m).map_err(|e| e.to_string())?;
        let identity = serde_json::to_string(&m.identity).map_err(|e| e.to_string())?;
        let blob = build_search_blob(m);
        let auth = m.identity.authority.as_str();
        let effect = m.identity.side_effect.as_str();
        let posture = m.identity.tool_posture.as_str();
        let role = m.identity.role.clone();
        // v7：来源 / 注册方式。判据只此一处（origin 模块），迁移回填也调它。
        let origin = crate::origin::classify_manifest(m);
        // v8：作者 + 出处。判据只此一处（author 模块），迁移回填也调它。
        //
        // 注意这里**不写 lifecycle**：它是用户状态，不是解析结果。
        // upsert 是"磁盘上有什么"的写入路径，每次扫描都跑一遍；
        // 如果它顺带把 lifecycle 写成 'active'，用户暂停/归档过的资产
        // 在下一次扫描时会被静默复活——那是比没有这个功能更糟的骗人。
        let author = crate::author::infer_author(
            m,
            m.provenance.source_path.as_deref().unwrap_or(""),
        );

        let existing: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT COALESCE(content_hash, ''), COALESCE(parser_rev, 0) FROM assets WHERE id = ?1",
                params![m.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();

        let outcome = match existing {
            Some((h, rev))
                if h == m.provenance.content_hash.as_deref().unwrap_or("")
                    && !h.is_empty()
                    && rev == PARSER_REV =>
            {
                // 内容没变、解析器也没变：只刷新 last_seen
                self.conn
                    .execute(
                        "UPDATE assets SET last_seen = ?2, source_path=?3,
                         manifest=json_set(manifest,'$.provenance.source_path',?3) WHERE id = ?1",
                        params![m.id, now, m.provenance.source_path],
                    )
                    .map_err(|e| e.to_string())?;
                "unchanged"
            }
            Some(_) => {
                self.conn
                    .execute(
                        "UPDATE assets SET kind=?2, name=?3, version=?4, description=?5,
                            manifest=?6, content_hash=?7, source_path=?8, source_format=?9,
                            identity=?10, authority=?13, side_effect=?14, tool_posture=?15,
                            role=?16, search_blob=?11, last_seen=?12, parser_rev=?17,
                            origin_kind=?18, registration_kind=?19,
                            origin_agent=?20, self_contained=?21,
                            -- v8：作者按「新结果优先，自声明可保留」写。
                            --
                            -- 三种情形必须分开：
                            --   1. 这次判出了作者（?22 非空）→ 直接覆盖，它是当前事实；
                            --   2. 这次判不出来，但库里那份是作者自声明
                            --      （manifest / frontmatter）→ 保留。空结果的含义是
                            --      本次没读到声明，不是作者当初没写；
                            --   3. 这次判不出来，库里那份只是包路径推断 → 清空。
                            --      路径已经变了（否则不会走到 updated 分支），
                            --      旧的包名就是过期事实，留着比空着更误导。
                            author = CASE
                                WHEN ?22 <> '' THEN ?22
                                WHEN author_source IN ('manifest', 'frontmatter') THEN author
                                ELSE '' END,
                            author_source = CASE
                                WHEN ?23 <> '' THEN ?23
                                WHEN author_source IN ('manifest', 'frontmatter') THEN author_source
                                ELSE '' END
                          WHERE id=?1",
                        params![
                            m.id,
                            m.kind.as_str(),
                            m.name,
                            m.version,
                            m.description,
                            json,
                            m.provenance.content_hash,
                            m.provenance.source_path,
                            m.provenance.source_format,
                            identity,
                            blob,
                            now,
                            auth,
                            effect,
                            posture,
                            role,
                            PARSER_REV,
                            origin.origin_kind,
                            origin.registration_kind,
                            origin.origin_agent,
                            origin.self_contained as i64,
                            author.author,
                            author.source
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                "updated"
            }
            None => {
                self.conn
                    .execute(
                        // v8：`lifecycle` 显式写 'active'（新资产的起点），
                        // 但**只在这里写**——用户改过之后再也不碰它。
                        "INSERT INTO assets
                            (id,kind,name,version,description,manifest,content_hash,
                             source_path,source_format,identity,authority,side_effect,
                             tool_posture,role,search_blob,first_seen,last_seen,parser_rev,
                             origin_kind,registration_kind,origin_agent,self_contained,
                             lifecycle,author,author_source)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?13,?14,?15,?16,?11,?12,?12,?17,
                                 ?18,?19,?20,?21,'active',?22,?23)",
                        params![
                            m.id,
                            m.kind.as_str(),
                            m.name,
                            m.version,
                            m.description,
                            json,
                            m.provenance.content_hash,
                            m.provenance.source_path,
                            m.provenance.source_format,
                            identity,
                            blob,
                            now,
                            auth,
                            effect,
                            posture,
                            role,
                            PARSER_REV,
                            origin.origin_kind,
                            origin.registration_kind,
                            origin.origin_agent,
                            origin.self_contained as i64,
                            author.author,
                            author.source
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                "inserted"
            }
        };

        // 能力与关系始终重建（它们是派生数据，保持与 manifest 一致）
        self.conn
            .execute("DELETE FROM capabilities WHERE asset_id = ?1", params![m.id])
            .map_err(|e| e.to_string())?;
        for c in &m.provides {
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO capabilities
                        (capability_id, asset_id, name, summary, tags)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![
                        c.capability_id,
                        m.id,
                        c.name,
                        c.summary,
                        serde_json::to_string(&c.tags).unwrap_or_else(|_| "[]".into())
                    ],
                )
                .map_err(|e| e.to_string())?;
        }

        self.conn
            .execute("DELETE FROM relations WHERE src_id = ?1", params![m.id])
            .map_err(|e| e.to_string())?;
        for r in &m.relations {
            self.conn
                .execute(
                    "INSERT INTO relations (src_id, kind, target_id, narrowing, max_depth)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![
                        m.id,
                        format!("{:?}", r.kind).to_lowercase(),
                        r.target_id,
                        r.scope_narrowing,
                        r.max_depth.map(|d| d as i64)
                    ],
                )
                .map_err(|e| e.to_string())?;

            // 只有层级关系进委派图
            if r.is_hierarchy() {
                self.conn
                    .execute(
                        "INSERT OR REPLACE INTO delegation_edges (src, dst, max_depth)
                         VALUES (?1,?2,?3)",
                        params![m.id, r.target_id, r.max_depth.unwrap_or(8) as i64],
                    )
                    .map_err(|e| e.to_string())?;
            }
        }

        Ok(outcome)
    }

    /// 批量写入，返回统计。
    pub fn upsert_all(&self, list: &[CapabilityManifest]) -> Result<UpsertResult, String> {
        let mut r = UpsertResult::default();
        for m in list {
            match self.upsert(m)? {
                "inserted" => r.inserted += 1,
                "updated" => r.updated += 1,
                _ => r.unchanged += 1,
            }
        }
        Ok(r)
    }

    /// 按类型统计数量，供 UI 左栏显示。
    pub fn counts_by_kind(&self) -> Result<Vec<KindCount>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind, COUNT(*) FROM assets GROUP BY kind ORDER BY kind")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(KindCount {
                    kind: r.get(0)?,
                    count: r.get::<_, i64>(1)? as u32,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn total(&self) -> Result<u32, String> {
        self.conn
            .query_row("SELECT COUNT(*) FROM assets", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as u32)
            .map_err(|e| e.to_string())
    }

    /// 取回完整 manifest（按 id）。
    pub fn get_manifest(&self, id: &str) -> Result<Option<CapabilityManifest>, String> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT manifest FROM assets WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .ok();
        match raw {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| format!("反序列化失败: {e}")),
        }
    }

    /// 按条件列出资产摘要。
    pub fn query_assets(&self, q: &AssetQuery) -> Result<Vec<AssetSummary>, String> {
        let mut q = q.clone();
        q.clamp();

        // 权限下推：把字符串序映射回枚举序，交给 SQLite 比较。
        // 枚举升序 = 字符串字典序恰好不一致（destructive < observe < read < write），
        // 所以必须显式转成数值等级。
        let min_rank: Option<i64> = q
            .min_authority
            .as_deref()
            .and_then(ac_core::Authority::parse)
            .map(|a| match a {
                ac_core::Authority::Read => 0,
                ac_core::Authority::Observe => 1,
                ac_core::Authority::Write => 2,
                ac_core::Authority::Destructive => 3,
            });

        // 标签过滤用 EXISTS 而不是 JOIN：同一资产可能有多个能力都带这个标签，
        // JOIN 会产生重复行，把一条资产在列表里显示好几次。
        // v8：默认过滤 archived。`include_archived` 为真时 ?7 = 1，条件恒成立。
        //
        // 为什么把 archvid 过滤写成 (?7 <> 0 OR ...) 而不是拼 SQL 字符串：
        // 拼串会让 list 与 count 两条 SQL 有分叉的机会，而分页错位
        // 正是"列表少一行、count 多一行"那类最难查的 bug（见 count_assets 的注释）。
        let sql = "
            SELECT a.id, a.kind, a.name, a.version, a.description,
                   a.authority, a.tool_posture, a.side_effect, a.role, a.source_path,
                   a.logical_id,
                   (SELECT COUNT(*) FROM capabilities c WHERE c.asset_id = a.id) AS n_cap,
                   (SELECT COUNT(*) FROM asset_locations l WHERE l.asset_id = a.id) AS n_loc,
                   (SELECT l2.role FROM asset_locations l2 WHERE l2.asset_id = a.id LIMIT 1) AS loc_role,
                   a.origin_kind, a.registration_kind, a.origin_agent, a.self_contained,
                   COALESCE(a.lifecycle, 'active') AS lifecycle,
                   COALESCE(a.author, '') AS author,
                   COALESCE(a.author_source, '') AS author_source,
                   COALESCE(a.source_format, '') AS source_format
            FROM assets a
            WHERE (?1 IS NULL OR a.kind = ?1)
              AND (?2 IS NULL OR a.search_blob LIKE ?2 ESCAPE '\\')
              AND (?3 IS NULL OR
                   CASE a.authority
                     WHEN 'read' THEN 0 WHEN 'observe' THEN 1
                     WHEN 'write' THEN 2 ELSE 3 END >= ?3)
              AND (?6 IS NULL OR EXISTS (
                   SELECT 1 FROM capabilities c2, json_each(c2.tags) t
                   WHERE c2.asset_id = a.id AND t.value = ?6))
              AND (?7 <> 0 OR COALESCE(a.lifecycle, 'active') <> 'archived')
            ORDER BY a.kind, a.name, a.id
            LIMIT ?4 OFFSET ?5";

        let pattern = q.text.as_deref().map(|t| format!("%{}%", escape_like(t)));
        let mut stmt = self.conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![
                    q.kind,
                    pattern,
                    min_rank,
                    i64::from(q.limit),
                    i64::from(q.offset),
                    q.tag,
                    q.include_archived as i64,
                ],
                |r| {
                    let authority: String = r.get(5)?;
                    let label = ac_core::Authority::parse(&authority)
                        .map(|a| a.label_zh().to_string())
                        .unwrap_or_else(|| authority.clone());
                    // v8：宿主支持必须与列表**同一趟**算出来。
                    // 单独再查一次会让 N 行变成 N+1 次查询，而且两处判据
                    // 迟早分叉（见 hosts::asset_host_support 的注释）。
                    let source_path: Option<String> = r.get(9)?;
                    let origin_agent: String = r.get(16)?;
                    let source_format: String = r.get(21)?;
                    let kind: String = r.get(1)?;
                    // source_format 缺失的老行按 kind 兜底（见 effective_format 的注释）。
                    let format = crate::hosts::effective_format(&kind, &source_format);
                    let support = crate::hosts::support_from_columns(
                        &origin_agent,
                        source_path.as_deref().unwrap_or(""),
                        &format,
                    );
                    Ok(AssetSummary {
                        id: r.get(0)?,
                        kind,
                        name: r.get(2)?,
                        version: r.get(3)?,
                        description: r.get(4)?,
                        authority_label: label,
                        authority,
                        tool_posture: r.get(6)?,
                        side_effect: r.get(7)?,
                        role: r.get(8)?,
                        source_path,
                        logical_id: r.get(10)?,
                        capability_count: r.get::<_, i64>(11)? as u32,
                        location_count: r.get::<_, i64>(12)? as u32,
                        location_role: r.get(13)?,
                        origin_kind: r.get(14)?,
                        registration_kind: r.get(15)?,
                        origin_agent,
                        self_contained: r.get::<_, i64>(17)? != 0,
                        lifecycle: r.get(18)?,
                        author: r.get(19)?,
                        author_source: r.get(20)?,
                        host_support: support.hosts,
                        host_support_basis: support.basis,
                    })
                },
            )
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// 满足条件的总条数，供 UI 做分页。
    pub fn count_assets(&self, q: &AssetQuery) -> Result<u32, String> {
        let min_rank: Option<i64> = q
            .min_authority
            .as_deref()
            .and_then(ac_core::Authority::parse)
            .map(|a| match a {
                ac_core::Authority::Read => 0,
                ac_core::Authority::Observe => 1,
                ac_core::Authority::Write => 2,
                ac_core::Authority::Destructive => 3,
            });
        let pattern = q.text.as_deref().map(|t| format!("%{}%", escape_like(t)));
        // 必须与 query_assets 的 WHERE 完全一致，否则分页会错：
        // 列表少一行、count 多一行，界面就会一直显示"还有更多"。
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM assets a
                 WHERE (?1 IS NULL OR a.kind = ?1)
                   AND (?2 IS NULL OR a.search_blob LIKE ?2 ESCAPE '\\')
                   AND (?3 IS NULL OR
                        CASE a.authority
                          WHEN 'read' THEN 0 WHEN 'observe' THEN 1
                          WHEN 'write' THEN 2 ELSE 3 END >= ?3)
                   AND (?4 IS NULL OR EXISTS (
                        SELECT 1 FROM capabilities c2, json_each(c2.tags) t
                        WHERE c2.asset_id = a.id AND t.value = ?4))
                   AND (?5 <> 0 OR COALESCE(a.lifecycle, 'active') <> 'archived')",
                params![q.kind, pattern, min_rank, q.tag, q.include_archived as i64],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(n as u32)
    }

    /// 库中所有资产 id，供扫描对账用。
    pub fn list_ids(&self) -> Result<Vec<String>, String> {        let mut stmt = self
            .conn
            .prepare("SELECT id FROM assets ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// 删除一个资产。`capabilities` 由外键 `ON DELETE CASCADE` 一并清掉。
    ///
    /// `relations` / `delegation_edges` 不动：它们可能指向尚未入库的
    /// 资产（先声明关系后导入是常见用法），这里只清理"自己拥有"的
    /// 数据，别把别人的关系图删残。
    pub fn remove(&self, id: &str) -> Result<bool, String> {
        let n = self
            .conn
            .execute("DELETE FROM assets WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(n > 0)
    }

    // ── v8：资产生命周期（软删除）────────────────────────────────

    /// 切换一个资产的生命周期状态，返回**是否命中**。
    ///
    /// `state` 只接受 `active` / `paused` / `archived`，别的一律报错——
    /// 静默接受错拼的状态会让资产在列表里凭空消失或永远不出现，
    /// 而调用方从返回值里看不出来。
    ///
    /// # 「删除」是 archived，不是 DELETE
    ///
    /// 项目铁律：**增量不删**。用户点「删除」之后，这一行仍然在库里，
    /// 它的能力、关系、位置痕迹、故障历史全都还在——只是默认列表里看不见。
    /// 这样三个后果才成立：
    ///
    /// - 误删可恢复（再置回 active 即可，不需要重扫磁盘）；
    /// - `asset_host_support` / 去重 / 故障这些交叉引用不会指向一个不存在的 id；
    /// - 审计能回答"这个资产什么时候被谁停掉的"。
    ///
    /// # 为什么不走 upsert
    ///
    /// `upsert` 是「磁盘上有什么」的写入路径，每次扫描都跑。生命周期是
    /// **用户状态**，两者混在一起意味着下一次扫描会把用户归档的资产复活。
    pub fn set_asset_lifecycle(&self, id: &str, state: &str) -> Result<bool, String> {
        let want = state.trim().to_ascii_lowercase();
        let valid = ["active", "paused", "archived"];
        if !valid.contains(&want.as_str()) {
            return Err(format!(
                "非法生命周期状态 {state:?}：只接受 {}",
                valid.join(" / ")
            ));
        }
        let n = self
            .conn
            .execute(
                "UPDATE assets SET lifecycle = ?2 WHERE id = ?1",
                params![id, want],
            )
            .map_err(|e| e.to_string())?;
        // 返回 bool 而不是 ()：调用方要能区分「改了」与「这个 id 根本不存在」。
        // 后者在界面上必须报错，静静返回成功会让用户以为删掉了。
        Ok(n > 0)
    }

    /// 读一个资产当前的生命周期；id 不存在返回 None。
    pub fn asset_lifecycle(&self, id: &str) -> Result<Option<String>, String> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT COALESCE(lifecycle, 'active') FROM assets WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .ok();
        Ok(v)
    }

    /// 各生命周期状态的资产数，供总览/侧栏显示。
    ///
    /// 用 SQL 一次分组算出来，不在 Rust 侧遍历——列表可能上千行。
    pub fn lifecycle_counts(&self) -> Result<Vec<KindCount>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT COALESCE(lifecycle, 'active') AS s, COUNT(*) FROM assets
                 GROUP BY s ORDER BY s",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(KindCount {
                    kind: r.get(0)?,
                    count: r.get::<_, i64>(1)? as u32,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    // ── M1.5：逻辑资产 + 位置集 ────────────────────────────────

    /// 把一次扫描的结果同步进 `logical_assets` / `asset_locations`。
    ///
    /// **只增不删**（append-only），这是本里程碑最重要的纪律：
    ///
    /// - 本次没扫到的位置**不会**被删除，只会因为没被刷新而留在库里；
    ///   它的 `role` 由 canonical 降级为 `alias`，`last_seen_at` 停在最后一次
    ///   真正看到它的时间。用户因此能回答"上次还是这版、现在升级了"。
    /// - 磁盘上删掉的资产在 `assets` 表里仍会被清理（M1 既有行为，不动），
    ///   但它的**位置痕迹**留在 `asset_locations` 里，这就是"追溯"的载体。
    ///
    /// 返回本次新建的逻辑资产数与新增位置数。
    pub fn sync_logical(
        &self,
        manifests: &[CapabilityManifest],
        now: i64,
    ) -> Result<LogicalSync, String> {
        let mut s = LogicalSync::default();
        // 本轮真正见到过的位置。收尾算 canonical 时只看它们，
        // 免得把上一轮已消失的位置又算成"还在场"。
        let mut seen: Vec<String> = Vec::new();

        for m in manifests {
            let hash = m
                .provenance
                .content_hash
                .as_deref()
                .filter(|h| !h.is_empty());
            // 缺指纹的老行**不能报错退出**——那会让整个同步中断，
            // 连带这一批里所有有指纹的资产都建不出逻辑资产。
            // 退化成 "一行一个逻辑资产"：不去重，但绝不丢数据。
            let (logical_id, l1) = match hash {
                Some(h) => (crate::fingerprint::group_key(h), h),
                None => (format!("raw-{}", m.id), ""),
            };
            let location_id = format!("loc-{}", m.id);
            let path = m.provenance.source_path.clone().unwrap_or_default();

            // 1) 逻辑资产：不存在才建。已存在**不覆盖** name/description——
            //    那是从 canonical 位置继承来的，canonical 换了才有理由改。
            let exists: i64 = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM logical_assets WHERE logical_id = ?1",
                    params![logical_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;

            if exists == 0 {
                self.conn
                    .execute(
                        "INSERT INTO logical_assets
                            (logical_id, content_hash, normalized_hash, kind, name, version,
                             description, canonical_location_id, location_count, first_seen, last_seen)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,?9,?9)",
                        params![
                            logical_id,
                            l1,
                            m.provenance.normalized_hash,
                            m.kind.as_str(),
                            m.name,
                            m.version,
                            m.description,
                            location_id,
                            now
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                s.logical_created += 1;
            } else {
                // M1.5 v4 修复：给**已存在**的逻辑资产补写 normalized_hash。
                //
                // 为什么必须在这里补，而不能只靠 +1 PARSER_REV：
                // 旧库的 464 行 logical_assets 是 BACKFILL_LOGICAL 一次性创建的，
                // 那一记 SQL 保留了列名却把 normalized_hash 写成了 NULL；而上面
                // `if exists == 0` 意味着「已存在就不写」，于是旧行永远拿不回它。
                // 只改 PARSER_REV 的话，重解析能修正 assets.version，但 L2 仍然是
                // 0 组——L2 在真实库上依然是死的。
                //
                // 为什么放同步而不是 migrate()：l2_hash 需要文件正文，
                // migrate() 只拿得到数据库（没有文件访问），算不出归一化指纹；
                // 而本函数正好握有每个资产的正文。
                //
                // 只在为空或与当前值不同时才写：不覆盖用户已完成的其它字段，
                // 也不会让每次扫描都变成一次写入。
                if let Some(nh) = m.provenance.normalized_hash.as_deref() {
                    let n = self
                        .conn
                        .execute(
                            "UPDATE logical_assets SET normalized_hash = ?2
                             WHERE logical_id = ?1
                               AND COALESCE(normalized_hash, '') <> ?2",
                            params![logical_id, nh],
                        )
                        .map_err(|e| e.to_string())?;
                    s.logical_updated += n as u32;
                }
            }

            // 2) 位置：upsert + 刷新 last_seen_at。
            //    role 绝不在这里被"自动纠正"成 canonical —— 那是用户的决定。
            let n = self
                .conn
                .execute(
                    "INSERT INTO asset_locations
                        (location_id, logical_id, source_path, role, last_seen_at, first_seen, asset_id)
                     VALUES (?1,?2,?3,'alias',?4,?4,?5)
                     ON CONFLICT(location_id) DO UPDATE SET
                        last_seen_at = excluded.last_seen_at,
                        source_path = excluded.source_path,
                        logical_id   = excluded.logical_id,
                        asset_id     = excluded.asset_id",
                    params![location_id, logical_id, path, now, m.id],
                )
                .map_err(|e| e.to_string())?;
            if n > 0 {
                s.locations_seen += 1;
            }
            seen.push(location_id.clone());

            // 3) 把逻辑资产 id 回写到 assets 行上（老列一个都不动）
            self.conn
                .execute(
                    "UPDATE assets SET logical_id = ?2, location_id = ?3 WHERE id = ?1",
                    params![m.id, logical_id, location_id],
                )
                .map_err(|e| e.to_string())?;
        }

        // 4) 收尾：重算每个逻辑资产的位置数与规范位置。
        //
        // 传**本轮真正见到过的 location_id 列表**，而不是时间戳：
        // 两次扫描可能落在同一秒内（506 个资产扫一遍不到 3 秒，
        // 测试里更是同秒），按 `last_seen_at >= t` 判"还在场"
        // 会把上一轮已经消失的位置也算进来。
        self.refresh_canonical_roles(&seen)?;
        s.alias_count = self.count_aliases()?;
        Ok(s)
    }

    /// 在每个逻辑资产内部挑出 canonical：**本轮仍在场的、source_path 最短**的那条。
    ///
    /// 选"最短"是因为它通常最靠近扫描根、最不像引用副本。
    /// 这只是**默认**归属，不是去重裁决：合并与否仍由用户决定。
    ///
    /// **只看本轮仍在场的**（`last_seen_at >= seen_since`）：
    /// 主副本从磁盘消失后，它在库里降级成 alias 痕迹，
    /// canonical 让给还在的那条。位置记录本身一条都不删——
    /// 那才是 append-only 要保住的东西。
    fn refresh_canonical_roles(&self, seen: &[String]) -> Result<(), String> {
        if seen.is_empty() {
            return Ok(());
        }
        // 只重算「本轮至少有一个成员在场」的组。
        // 全员消失的组原样保留——那里的 role 可能是用户上次的裁决结果，
        // 抹掉它就等于让 append-only 留下的痕迹失去意义。
        // 先把这类组里的在场位置全压回 alias，再由下面那条挑出唯一 canonical。
        // 两步走而不是一条 UPDATE，因为 SQLite 没有 UPDATE ... ORDER BY ... LIMIT。
        // "本轮在场"的集合要走一张临时表，理由有两条，都很硬：
        // 1. 这批 id 会在同一条 SQL 里用到**多次**，而 `IN (?)`
        //    每出现一次就要多绑一遍参数，rusqlite 按 `?` 个数校验；
        // 2. SQLite 单条语句的绑定参数上限是 999，而真实扫描一轮
        //    就有 500+ 个位置——迟早撞上限。
        self.conn
            .execute_batch("CREATE TEMP TABLE IF NOT EXISTS seen_now (location_id TEXT PRIMARY KEY); DELETE FROM seen_now;")
            .map_err(|e| e.to_string())?;
        {
            let mut ins = self
                .conn
                .prepare("INSERT OR IGNORE INTO seen_now (location_id) VALUES (?1)")
                .map_err(|e| e.to_string())?;
            for id in seen {
                ins.execute(params![id]).map_err(|e| e.to_string())?;
            }
        }

        // 注意这里清的是**组内全部成员**，不只是在场的那几条。
        // canonical 的含义是"这一组现在的活代表"，已经从磁盘消失的位置
        // 占着这个角色是错的——它必须是 alias（append-only 留下的痕迹）。
        // 代价是：上一轮用户裁决出来的 canonical 会在它消失后失效，
        // 这正是我们要的，canonical 本来就该跟着还在的东西走。
        self.conn
            .execute(
                "UPDATE asset_locations
                 SET role = 'alias'
                 WHERE logical_id IN (
                     SELECT logical_id FROM asset_locations
                     WHERE logical_id IN (
                         SELECT logical_id FROM asset_locations
                         WHERE location_id IN (SELECT location_id FROM seen_now)
                     )
                     GROUP BY logical_id HAVING COUNT(*) > 1
                 )",
                [],
            )
            .map_err(|e| e.to_string())?;

        // 每个 logical 组里，**本轮仍在场的**、source_path 最短的那条升为 canonical。
        // 已消失的位置留在库里当痕迹，但不再占着 canonical 这个坑。
        // 单元素组保持原样（无歧义，无需角色）。
        self.conn
            .execute(
                "UPDATE asset_locations
                 SET role = 'canonical'
                 WHERE location_id IN (
                     SELECT location_id FROM (
                         SELECT location_id,
                                ROW_NUMBER() OVER (
                                    PARTITION BY logical_id
                                    ORDER BY LENGTH(source_path), location_id
                                ) AS rn
                         FROM asset_locations
                         WHERE location_id IN (SELECT location_id FROM seen_now)
                     ) WHERE rn = 1
                 )
                 AND logical_id IN (
                     SELECT logical_id FROM asset_locations
                     WHERE location_id IN (SELECT location_id FROM seen_now)
                     GROUP BY logical_id HAVING COUNT(*) > 1
                 )",
                [],
            )
            .map_err(|e| e.to_string())?;

        self.conn
            .execute_batch(
                "UPDATE logical_assets SET
                    location_count = (
                        SELECT COUNT(*) FROM asset_locations l WHERE l.logical_id = logical_assets.logical_id
                    ),
                    canonical_location_id = COALESCE((
                        SELECT l.location_id FROM asset_locations l
                        WHERE l.logical_id = logical_assets.logical_id AND l.role = 'canonical'
                        LIMIT 1
                    ), ''),
                    last_seen = MAX(last_seen, COALESCE((
                        SELECT MAX(l.last_seen_at) FROM asset_locations l
                        WHERE l.logical_id = logical_assets.logical_id
                    ), last_seen))",
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 找出「一个逻辑资产挂了多个位置」的分组，作为去重候选。
    ///
    /// 这些分组**只是候选**，绝不自动合并。L1 相同（字节级重复）优先。
    pub fn dedup_candidates(&self) -> Result<Vec<DedupCandidate>, String> {
        let mut out = Vec::new();

        // L1 组：一个 content_hash 对应多个位置
        let mut stmt = self
            .conn
            .prepare(
                "SELECT l.logical_id,
                        GROUP_CONCAT(l.location_id, char(10)),
                        MIN(LENGTH(l.source_path))
                 FROM asset_locations l
                 JOIN logical_assets g ON g.logical_id = l.logical_id
                 WHERE g.logical_id LIKE 'l1-%'
                 GROUP BY l.logical_id
                 HAVING COUNT(*) > 1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (gid, joined, _min) = row.map_err(|e| e.to_string())?;
            let mut ids: Vec<String> =
                joined.split(0x0a as char).filter(|s| !s.is_empty()).map(String::from).collect();
            ids.sort();
            out.push(DedupCandidate {
                group_key: gid.clone(),
                match_level: MatchLevel::L1,
                suggested_canonical: self
                    .canonical_of(&gid)?
                    .unwrap_or_else(|| ids.first().cloned().unwrap_or_default()),
                location_ids: ids,
            });
        }

        // L2 组：normalized_hash 相同但 L1 不同 -> 只是"疑似重打包"，提示但不强推。
        //
        // # 必须排除「同一 logical_id 的多个位置」
        //
        // L2 按 normalized_hash 分组，而**同一个 logical_id 的所有位置本来就共享
        // 一个 normalized_hash**（同一个逻辑资产 = 同一份内容 = 同一份规范化指纹）。
        // 于是只要某个 L1 组有 2 个位置，L2 就会把**完全相同的这 2 个位置**再报一遍，
        // 而 L1 已经报过了。
        //
        // 实测（真实库 506 资产 / 464 逻辑资产）：L1 27 组，L2 也 27 组，
        // 界面因此显示 54 组，而真实只有 27 组。跨 logical_id 的 L2 组 = 0。
        //
        // 修法是加一条 HAVING：只保留真正**跨逻辑资产**的分组。
        // 这不是"把数字调小"，而是把 L2 的定义补齐——L2 存在的理由是
        // "L1 不同但规范化后相同"，同一 logical_id 内的位置压根不满足"L1 不同"。
        let mut stmt = self
            .conn
            .prepare(
                "SELECT normalized_hash,
                        GROUP_CONCAT(location_id, char(10))
                 FROM (
                     SELECT g.normalized_hash AS normalized_hash, l.location_id AS location_id,
                            l.logical_id AS logical_id
                     FROM asset_locations l
                     JOIN logical_assets g ON g.logical_id = l.logical_id
                     WHERE g.normalized_hash IS NOT NULL AND g.normalized_hash <> ''
                 )
                 GROUP BY normalized_hash
                 HAVING COUNT(DISTINCT location_id) > 1
                    -- 关键一行：跨 logical_id 才算 L2 候选
                    AND COUNT(DISTINCT logical_id) > 1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (norm, joined) = row.map_err(|e| e.to_string())?;
            let Some(joined) = joined else { continue };
            let mut ids: Vec<String> =
                joined.split('\n').filter(|s| !s.is_empty()).map(String::from).collect();
            ids.sort();
            out.push(DedupCandidate {
                group_key: crate::fingerprint::l2_group_key(&norm),
                match_level: MatchLevel::L2,
                suggested_canonical: ids.first().cloned().unwrap_or_default(),
                location_ids: ids,
            });
        }

        // 确定性排序，便于 UI 与测试
        out.sort_by(|a, b| {
            a.match_level
                .as_str()
                .cmp(b.match_level.as_str())
                .then(a.group_key.cmp(&b.group_key))
        });
        Ok(out)
    }

    // ── 用户裁决：三个选择，绝无默认 ─────────────────────────────

    /// 记录一次用户裁决。
    ///
    /// 这是整个 M1.5 **唯一** 会改变去重结果���入口，
    /// 因此它必须显式、留痕、可查询——不允许任何路径绕过它去改 role。
    ///
    /// - `Merge`     合并：把该组里除 canonical 外的位置标记为已合并，
    ///                 逻辑资产维持 1 个。这是用户说"它们就是同一份"的表达。
    /// - `KeepBoth` 共存：每个位置升为**独立逻辑资产**，多版本/多副本各自保留。
    /// - `Ignore`   忽略：一切不动，下次扫描重新入队再问。
    pub fn decide_dedup(
        &self,
        group_key: &str,
        match_level: MatchLevel,
        decision: DedupDecision,
    ) -> Result<bool, String> {
        let now = Self::now();
        let lvl = match_level.as_str();

        // 组里到底有哪些位置（决定 Merge / KeepBoth 怎么落）
        let locs: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT location_id, logical_id FROM asset_locations ORDER BY location_id",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
        };

        // L1 组：直接按 logical_id 取成员
        let members: Vec<String> = if match_level == MatchLevel::L1 {
            locs.iter()
                .filter(|(_, lid)| lid == group_key)
                .map(|(loc, _)| loc.clone())
                .collect()
        } else {
            // L2 组：跨逻辑资产找同 normalized_hash 的位置
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT l.location_id FROM asset_locations l
                     JOIN logical_assets g ON g.logical_id = l.logical_id
                     WHERE g.normalized_hash = ?1 ORDER BY l.location_id",
                )
                .map_err(|e| e.to_string())?;
            let norm = group_key.trim_start_matches("l2-");
            let rows = stmt
                .query_map(params![norm], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
        };

        if members.is_empty() {
            return Ok(false);
        }

        match decision {
            DedupDecision::Ignore => {
                // 什么都不改。只留痕：记下用户选了"下次再问"。
                self.conn
                    .execute(
                        "INSERT INTO dedup_candidates
                            (group_key, match_level, suggested_canonical, location_ids,
                             decision, decided_at, first_detected_at, resolved)
                         VALUES (?1,?2,'',?3,'ignore',?4,?4,0)
                         ON CONFLICT(group_key, match_level) DO UPDATE SET
                            decision = 'ignore', decided_at = excluded.decided_at,
                            resolved = 0,
                            location_ids = excluded.location_ids",
                        params![
                            group_key,
                            lvl,
                            serde_json::to_string(&members).unwrap_or_else(|_| "[]".into()),
                            now
                        ],
                    )
                    .map_err(|e| e.to_string())?;
            }
            DedupDecision::Merge => {
                let canonical = self
                    .canonical_of(group_key)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| members.first().cloned().unwrap_or_default());
                for loc in &members {
                    let role = if *loc == canonical { "canonical" } else { "merged" };
                    self.conn
                        .execute(
                            "UPDATE asset_locations SET role = ?2 WHERE location_id = ?1",
                            params![loc, role],
                        )
                        .map_err(|e| e.to_string())?;
                }
                self.record_decision(group_key, lvl, &members, "merge", now, 1)?;
            }
            DedupDecision::KeepBoth => {
                // 每个位置拆成独立逻辑资产：logical_id 从位置 id 派生，
                // 保证**互相独立**，再不会共享 content_hash 分组。
                for loc in &members {
                    let new_logical = format!("kb-{loc}");
                    self.conn
                        .execute(
                            // 列名要全部带表前缀：location_id 在两张表里都有，
                        // 不限定就是 SQLite 的 "ambiguous column name"。
                        "INSERT OR IGNORE INTO logical_assets
                                (logical_id, content_hash, normalized_hash, kind, name, version,
                                 description, canonical_location_id, location_count, first_seen, last_seen)
                             SELECT ?1, a.content_hash, NULL, a.kind, a.name, a.version, a.description,
                                    l.location_id, 1, ?3, ?3
                             FROM asset_locations l
                             JOIN assets a ON a.id = l.asset_id
                             WHERE l.location_id = ?2",
                            params![new_logical, loc, now],
                        )
                        .map_err(|e| e.to_string())?;
                    self.conn
                        .execute(
                            "UPDATE asset_locations
                             SET logical_id = ?2, role = 'canonical'
                             WHERE location_id = ?1",
                            params![loc, new_logical],
                        )
                        .map_err(|e| e.to_string())?;
                    // assets 行也要跟着改，否则列表与逻辑表对不上
                    self.conn
                        .execute(
                            "UPDATE assets SET logical_id = ?2 WHERE location_id = ?1",
                            params![loc, new_logical],
                        )
                        .map_err(|e| e.to_string())?;
                }
                // 原组被拆空后就是一条没有任何位置的空壳。
                // 留着它会让 logical_total 比真实资产数多一个 ——
                // "506 条变 464 个逻辑资产"这种数字从此对不上账。
                self.conn
                    .execute(
                        "DELETE FROM logical_assets WHERE logical_id NOT IN
                            (SELECT DISTINCT logical_id FROM asset_locations)",
                        [],
                    )
                    .map_err(|e| e.to_string())?;

                self.record_decision(group_key, lvl, &members, "keep_both", now, 0)?;
            }
        }
        Ok(true)
    }

    fn record_decision(
        &self,
        group_key: &str,
        lvl: &str,
        members: &[String],
        decision: &str,
        now: i64,
        resolved: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO dedup_candidates
                    (group_key, match_level, suggested_canonical, location_ids,
                     decision, decided_at, first_detected_at, resolved)
                 VALUES (?1,?2,'',?3,?4,?5,?5,?6)
                 ON CONFLICT(group_key, match_level) DO UPDATE SET
                    decision = excluded.decision,
                    decided_at = excluded.decided_at,
                    resolved = excluded.resolved,
                    location_ids = excluded.location_ids",
                params![
                    group_key,
                    lvl,
                    serde_json::to_string(members).unwrap_or_else(|_| "[]".into()),
                    decision,
                    now,
                    resolved
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 已裁决的候选记录（审计用：谁在什么时候把哪组判成了什么）。
    pub fn dedup_history(&self) -> Result<Vec<DedupRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT group_key, match_level, decision, decided_at, location_ids
                 FROM dedup_candidates WHERE decision IS NOT NULL
                 ORDER BY decided_at DESC, group_key",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                let ids_json: Option<String> = r.get(4)?;
                let locs = ids_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                    .unwrap_or_default();
                Ok(DedupRecord {
                    group_key: r.get(0)?,
                    match_level: r.get(1)?,
                    decision: r.get(2)?,
                    decided_at: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    location_ids: locs,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    fn canonical_of(&self, logical_id: &str) -> Result<Option<String>, String> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT location_id FROM asset_locations
                 WHERE logical_id = ?1 AND role = 'canonical' LIMIT 1",
                params![logical_id],
                |r| r.get(0),
            )
            .ok();
        Ok(v)
    }

    /// 位置总数。
    pub fn location_total(&self) -> Result<u32, String> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM asset_locations", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        Ok(n as u32)
    }

    /// alias 位置数（对外命名，preview 导出用）。
    pub fn locations_of_count_alias(&self) -> Result<u32, String> {
        self.count_aliases()
    }

    /// 逻辑资产总数。
    pub fn logical_total(&self) -> Result<u32, String> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM logical_assets", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        Ok(n as u32)
    }

    /// 有指纹的资产里，distinct 的 L1 指纹数。
    ///
    /// 这是"如果去重真的发生，应该折叠成多少个"的基准值。
    /// 自检拿它和 `logical_total()` 对比——两者不等就说明有静默合并。
    pub fn distinct_l1_count(&self) -> Result<u32, String> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT content_hash) FROM assets
                 WHERE content_hash IS NOT NULL AND content_hash <> ''",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(n as u32)
    }

    /// 逻辑资产总数（与 `logical_total` 同义的显式别名，便于自检表达意图）。
    pub fn logical_count(&self) -> Result<u32, String> {
        self.logical_total()
    }

    /// 取某个逻辑资产的全部位置（按角色、路径长度排序）。
    pub fn locations_of(&self, logical_id: &str) -> Result<Vec<ac_core::AssetLocation>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT location_id, logical_id, source_path, role, last_seen_at, asset_id
                 FROM asset_locations WHERE logical_id = ?1
                 ORDER BY CASE role WHEN 'canonical' THEN 0 ELSE 1 END,
                          LENGTH(source_path), location_id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![logical_id], |r| {
                Ok(ac_core::AssetLocation {
                    location_id: r.get(0)?,
                    logical_id: r.get(1)?,
                    source_path: r.get(2)?,
                    role: ac_core::LocationRole::parse(&r.get::<_, String>(3)?),
                    last_seen_at: r.get(4)?,
                    asset_id: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// 取逻辑资产列表（供 M2 的 logical group 显示）。
    pub fn logical_assets(&self, limit: u32) -> Result<Vec<ac_core::LogicalAsset>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT logical_id, content_hash, normalized_hash, kind, name, version,
                        description, canonical_location_id, location_count
                 FROM logical_assets ORDER BY location_count DESC, name LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(ac_core::LogicalAsset {
                    logical_id: r.get(0)?,
                    content_hash: r.get(1)?,
                    normalized_hash: r.get(2)?,
                    kind: r.get(3)?,
                    name: r.get(4)?,
                    version: r.get(5)?,
                    description: r.get(6)?,
                    canonical_location_id: r.get(7)?,
                    location_count: r.get::<_, i64>(8)? as u32,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    fn count_aliases(&self) -> Result<u32, String> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM asset_locations WHERE role = 'alias'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(n as u32)
    }

    // ── 标签筛选（M1 审计发现的硬缺口）──────────────────────────

    /// 库里出现过的所有标签及其资产数，按数量降序。
    ///
    /// 标签语义见本文件底部 `TAG_SEMANTICS` 注释。
    pub fn all_tags(&self) -> Result<Vec<TagCount>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.value AS tag, COUNT(DISTINCT c.asset_id) AS n
                 FROM capabilities c, json_each(c.tags) t
                 WHERE t.value IS NOT NULL AND t.value <> ''
                 GROUP BY t.value
                 ORDER BY n DESC, t.value ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(TagCount {
                    tag: r.get(0)?,
                    count: r.get::<_, i64>(1)? as u32,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// 委派图环检测。返回导致成环的边（如果有）。
    pub fn find_cycle(&self) -> Result<Option<(String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT src, dst FROM delegation_edges")
            .map_err(|e| e.to_string())?;
        let edges: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;

        // DFS 着色法找环
        let mut adj: std::collections::HashMap<&str, Vec<&str>> = Default::default();
        for (s, d) in &edges {
            adj.entry(s.as_str()).or_default().push(d.as_str());
        }
        let mut state: std::collections::HashMap<&str, u8> = Default::default();
        let mut stack: Vec<&str> = Vec::new();

        fn dfs<'a>(
            node: &'a str,
            adj: &std::collections::HashMap<&'a str, Vec<&'a str>>,
            state: &mut std::collections::HashMap<&'a str, u8>,
            stack: &mut Vec<&'a str>,
        ) -> Option<(String, String)> {
            state.insert(node, 1);
            stack.push(node);
            if let Some(next) = adj.get(node) {
                for &n in next {
                    match state.get(n) {
                        Some(1) => {
                            // 找到回边
                            let pos = stack.iter().position(|x| *x == n).unwrap_or(0);
                            let tail = stack.get(pos).map(|s| s.to_string()).unwrap_or_default();
                            return Some((tail, n.to_string()));
                        }
                        Some(_) => {}
                        None => {
                            if let Some(c) = dfs(n, adj, state, stack) {
                                return Some(c);
                            }
                        }
                    }
                }
            }
            stack.pop();
            state.insert(node, 2);
            None
        }

        let mut nodes: Vec<&str> = adj.keys().copied().collect();
        nodes.sort_unstable();
        for n in nodes {
            if !state.contains_key(n) {
                if let Some(c) = dfs(n, &adj, &mut state, &mut stack) {
                    return Ok(Some(c));
                }
            }
        }
        Ok(None)
    }
}

/// # 标签语义（M1.5 的决定，M1 审计发现的硬缺口）
///
/// `capabilities.tags` 一直存在，但**只写不读**——M1 的
/// `ListQuery` / `AssetQuery` 都没有 tag 字段，检索路径上没有任何地方消费它。
/// M1.5 补上全链路，并在此明确**筛选到底筛的是什么**。
///
/// ## 现在库里实际有哪些标签
///
/// 真实数据（506 资产）共 27 种，可分四类：
///
/// | 类别 | 例子 | 数量 | 语义 | 筛不筛 |
/// |---|---|---|---|---|
/// | **行为标签** | `read_only` `write` `open_world` `hooks` | 少 | "这个能力会不会动手" | **筛** |
/// | **结构标签** | `bundle:skills` `bundle:commands` `bundle:mcpServers` `declared-path` | 12+6+1+4 | "这个插件打包了什么" | **筛** |
/// | **工具名** | `Read` `Write` `Edit` `Bash` `Grep` | 62/54/41/40/24 | 技能声明的可用工具名 | **筛** |
/// | **工具全名** | `mcp__zotero__zotero_search_items` | ~10 | 某个 MCP server 的具体 tool | **筛** |
///
/// ## 作者标签从哪来
///
/// SKILL.md 的 frontmatter **没有 `tags:` 键**——全库 460 个 SKILL.md 实测为 0。
/// 但 `plugin.json` 有：36 个插件里 **10 个**带 `tags:`（`skill` / `education` /
/// `canvas` / `c4` / `toolchain` …），另有 15 个带 `keywords:`。
///
/// 所以 M1.5 顺手把这两处作者标签抽出来（见 `parse.rs` 的 `extract_author_tags`），
/// 否则「按标签筛选」只能筛机器标签，用户自己的分类词一条也用不上。
///
/// ## 结论：按 OR 语义筛，命中任一能力即返回
///
/// `AssetQuery.tag` 是**单值**、**任一命中即返回**：
///
/// - 单值，是因为多选交集在 500 条量级上会让 UI 变成 AND/OR 开关矩阵，
///   而 M1 的完成标志只要求「可按标签筛选」，单值已经闭环；
/// - OR 语义，是因为标签本就挂在**能力**上而清单可能有多个能力，
///   用 EXISTS 而不是 JOIN（否则一条资产会在列表里重复出现）。
///
/// 未被写入本表的位置（如 `identity.dimensions` 里的 `domain`）刻意不作为标签：
/// 它们属于推断维度而非用户可标注的标签，混进来会让"标签"这个词失去意义。
///
/// 拼接可检索文本。**向量检索主要吃 summary，这里只负责 BM25/精确匹配。**
fn build_search_blob(m: &CapabilityManifest) -> String {
    let mut parts: Vec<String> = vec![
        m.id.clone(),
        m.name.clone(),
        m.description.clone(),
        m.identity.keyword_blob(),
    ];
    for c in &m.provides {
        parts.push(c.name.clone());
        parts.push(c.summary.clone());
        parts.extend(c.tags.iter().cloned());
    }
    parts.retain(|s| !s.trim().is_empty());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::{AssetKind, ProvidedCapability};

    fn sample(id: &str, hash: &str) -> CapabilityManifest {
        let mut m = CapabilityManifest::new(id, AssetKind::Skill, "样例");
        m.description = "描述".into();
        m.provides.push(ProvidedCapability {
            capability_id: format!("{id}::run"),
            name: "run".into(),
            summary: "执行".into(),
            input_schema: None,
            tags: vec!["read_only".into()],
        });
        m.provenance.content_hash = Some(hash.into());
        m
    }

    #[test]
    fn migrates_v1_database_without_losing_identity() {
        // 回归：只靠 CREATE TABLE IF NOT EXISTS 时，老库缺列 → 建索引报
        // "no such column"，而这恰好发生在用户第一次升级时。
        let conn = Connection::open_in_memory().unwrap();
        // 造一个 v1 形状的旧库：没有 identity 偏序列，user_version=1
        conn.execute_batch(
            "CREATE TABLE assets (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
                version TEXT NOT NULL DEFAULT '0', description TEXT NOT NULL DEFAULT '',
                manifest TEXT NOT NULL, content_hash TEXT, source_path TEXT,
                source_format TEXT, identity TEXT NOT NULL DEFAULT '{}',
                search_blob TEXT NOT NULL DEFAULT '',
                first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL,
                manual_identity INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE capabilities (
                capability_id TEXT PRIMARY KEY,
                asset_id      TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                name          TEXT NOT NULL,
                summary       TEXT NOT NULL DEFAULT '',
                tags          TEXT NOT NULL DEFAULT '[]');
             CREATE TABLE relations (
                id INTEGER PRIMARY KEY AUTOINCREMENT, src_id TEXT NOT NULL,
                kind TEXT NOT NULL, target_id TEXT NOT NULL,
                narrowing TEXT, max_depth INTEGER);
             CREATE TABLE delegation_edges (
                src TEXT NOT NULL, dst TEXT NOT NULL,
                max_depth INTEGER NOT NULL DEFAULT 8, PRIMARY KEY (src, dst));
             PRAGMA user_version = 1;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO assets (id,kind,name,manifest,identity,search_blob,first_seen,last_seen)
             VALUES ('s::old','skill','旧资产','{}',
                     '{\"role\":\"researcher\",\"authority\":\"destructive\",\"side_effect\":\"irreversible\",\"tool_posture\":\"gated\"}',
                     '旧', 1, 1)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);

        // 关键：必须回填，不能停在默认值。
        // 否则内容未变的资产下次扫描走 "unchanged" 分支，列永远不被刷新。
        let (auth, posture, role): (String, String, Option<String>) = conn
            .query_row(
                "SELECT authority, tool_posture, role FROM assets WHERE id='s::old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(auth, "destructive");
        assert_eq!(posture, "gated");
        assert_eq!(role.as_deref(), Some("researcher"));

        // 迁移后的库要能正常建索引并被 Registry 打开
        let reg = Registry { conn };
        assert_eq!(reg.total().unwrap(), 1);
        let got = reg
            .query_assets(&AssetQuery {
                min_authority: Some("write".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(got.len(), 1, "回填后应能被权限过滤命中");
    }

    #[test]
    fn migrates_legacy_db_with_zero_user_version() {
        // 回归：早期版本从没写过 user_version，老库的版本号也是 0。
        // 拿 0 当"全新库"处理就会 CREATE TABLE 空转，然后建索引时报
        // "no such column: authority"——正好是用户第一次升级时的情形。
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE assets (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
                version TEXT NOT NULL DEFAULT '0', description TEXT NOT NULL DEFAULT '',
                manifest TEXT NOT NULL, content_hash TEXT, source_path TEXT,
                source_format TEXT, identity TEXT NOT NULL DEFAULT '{}',
                search_blob TEXT NOT NULL DEFAULT '',
                first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL,
                manual_identity INTEGER NOT NULL DEFAULT 0);
             INSERT INTO assets (id,kind,name,manifest,identity,search_blob,first_seen,last_seen)
             VALUES ('s::old','skill','旧资产','{}',
                     '{\"authority\":\"write\",\"tool_posture\":\"writer\"}','旧',1,1);",
        )
        .unwrap();
        assert_eq!(user_version(&conn).unwrap(), 0);

        migrate(&conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
        let (auth, posture): (String, String) = conn
            .query_row(
                "SELECT authority, tool_posture FROM assets WHERE id='s::old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(auth, "write");
        assert_eq!(posture, "writer");

        let reg = Registry { conn };
        assert_eq!(reg.total().unwrap(), 1);
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn refuses_newer_schema() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 5)
            .unwrap();
        let err = migrate(&conn).unwrap_err();
        assert!(err.contains("请升级 capability-core"), "{err}");
    }

    #[test]
    fn m15_legacy_db_backfills_normalized_hash_into_existing_rows() {
        // 回归（M1.5 v4 修复的核心用例）。
        //
        // 复现原始 BLOCKER：旧库的 464 行 logical_assets 是 BACKFILL_LOGICAL
        // 建的，normalized_hash 全是 NULL；assets.parser_rev 也停在旧值。
        // 此时若只 +1 PARSER_REV 而不同步补写 normalized_hash：
        // 版本号会修好，但 L2 候选仍然是 0 组——L2 在真实库上依然是死的。
        let r = Registry::open_in_memory().unwrap();
        let mut m = sample("s::a", "h1");
        m.provenance.normalized_hash = Some("N1".into());
        m.provenance.source_path = Some("vendor/skill/a/SKILL.md".into());
        r.upsert(&m).unwrap();
        r.sync_logical(std::slice::from_ref(&m), 1000).unwrap();

        // 模拟老库形态：归一化指纹被抹掉，解析器编号也回退
        r.conn.execute("UPDATE logical_assets SET normalized_hash = NULL", []).unwrap();
        r.conn
            .execute("UPDATE assets SET parser_rev = parser_rev - 1", [])
            .unwrap();

        let before: i64 = r
            .conn
            .query_row(
                "SELECT COUNT(*) FROM logical_assets WHERE normalized_hash IS NOT NULL AND normalized_hash <> ''",
                [],
                |x| x.get(0),
            )
            .unwrap();
        assert_eq!(before, 0, "前置条件：老库确实没有归一化指纹");

        // 重跑同步（源文件没变）
        let synced = r.sync_logical(std::slice::from_ref(&m), 2000).unwrap();
        assert_eq!(
            synced.logical_updated, 1,
            "既有行必须被补写归一化指纹，否则 L2 候选永远是 0 组"
        );

        let after: i64 = r
            .conn
            .query_row(
                "SELECT COUNT(*) FROM logical_assets WHERE normalized_hash IS NOT NULL AND normalized_hash <> ''",
                [],
                |x| x.get(0),
            )
            .unwrap();
        assert_eq!(after, 1, "补写后归一化指纹必须真的落到库里");

        // 幂等：紧接着再扫一次不应再写（否则每次扫描都是一次写入）
        let again = r.sync_logical(std::slice::from_ref(&m), 3000).unwrap();
        assert_eq!(again.logical_updated, 0, "值没变时不应重复写入");
    }

    #[test]
    fn parser_rev_change_forces_rewrite_even_when_content_same() {
        // 回归：改了解析逻辑但源文件没动时，content_hash 完全一致。
        // 若只看 content_hash，资产会被判成 unchanged 永远不刷新，
        // 用户看到的还是旧数据，且没有任何报错。
        let r = Registry::open_in_memory().unwrap();
        let mut m = sample("s::a", "h1");
        m.name = "旧名字".into();
        r.upsert(&m).unwrap();
        assert_eq!(r.upsert(&m).unwrap(), "unchanged");

        // 模拟解析器升级后重跑：同内容，但解析结果变了
        let mut m2 = sample("s::a", "h1");
        m2.name = "新名字".into();
        let rev: i64 = r
            .conn
            .query_row("SELECT parser_rev FROM assets WHERE id='s::a'", [], |x| x.get(0))
            .unwrap();
        assert_eq!(rev, PARSER_REV, "写入时必须记下当时的解析器修订号");
    }

    #[test]
    fn stale_parser_rev_is_treated_as_changed() {
        // 直接把库里的 parser_rev 改旧，模拟"程序升级了但文件没动"
        let r = Registry::open_in_memory().unwrap();
        let m = sample("s::a", "h1");
        r.upsert(&m).unwrap();
        r.conn
            .execute(
                "UPDATE assets SET parser_rev = parser_rev - 1, name = '旧名字'",
                [],
            )
            .unwrap();

        let mut m2 = sample("s::a", "h1");
        m2.name = "新名字".into();
        assert_eq!(r.upsert(&m2).unwrap(), "updated");
        let got = r.get_manifest("s::a").unwrap().unwrap();
        assert_eq!(got.name, "新名字");
    }

    #[test]
    fn insert_then_unchanged_then_updated() {
        let r = Registry::open_in_memory().unwrap();

        assert_eq!(r.upsert(&sample("s::a", "h1")).unwrap(), "inserted");
        assert_eq!(r.upsert(&sample("s::a", "h1")).unwrap(), "unchanged");
        assert_eq!(r.upsert(&sample("s::a", "h2")).unwrap(), "updated");
        assert_eq!(r.total().unwrap(), 1);
    }

    #[test]
    fn capabilities_and_relations_are_rebuilt() {
        let r = Registry::open_in_memory().unwrap();
        let mut m = sample("s::a", "h1");
        m.relations.push(ac_core::AssetRelation::new(
            ac_core::RelationKind::LeaderToSub,
            "s::b",
        ));
        r.upsert(&m).unwrap();

        // 改一次，让内容变化触发重建
        let mut m2 = sample("s::a", "h2");
        m2.provides.clear(); // 清空能力
        r.upsert(&m2).unwrap();

        let n: i64 = r
            .conn
            .query_row("SELECT COUNT(*) FROM capabilities WHERE asset_id='s::a'", [], |x| x.get(0))
            .unwrap();
        assert_eq!(n, 0, "能力表应随 manifest 重建");

        let e: i64 = r
            .conn
            .query_row("SELECT COUNT(*) FROM delegation_edges WHERE src='s::a'", [], |x| x.get(0))
            .unwrap();
        assert_eq!(e, 1, "层级关系应进委派图");
    }

    #[test]
    fn manifest_roundtrip_through_db() {
        let r = Registry::open_in_memory().unwrap();
        let m = sample("s::rt", "h1");
        r.upsert(&m).unwrap();
        let back = r.get_manifest("s::rt").unwrap().unwrap();
        assert_eq!(m.name, back.name);
        assert_eq!(m.provides.len(), back.provides.len());
    }

    // ── M1.5：迁移 ──────────────────────────────────────────────

    #[test]
    fn m15_migration_keeps_old_columns_and_backfills_logical() {
        // 回归：M1.5 只**加**列不删列，且必须把既有资产接进逻辑资产表。
        // 老库升级后旧查询路径（旧 id / content_hash）必须照常工作。
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE assets (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
                version TEXT NOT NULL DEFAULT '0', description TEXT NOT NULL DEFAULT '',
                manifest TEXT NOT NULL, content_hash TEXT, source_path TEXT,
                source_format TEXT, identity TEXT NOT NULL DEFAULT '{}',
                authority TEXT NOT NULL DEFAULT 'read', side_effect TEXT NOT NULL DEFAULT 'none',
                tool_posture TEXT NOT NULL DEFAULT 'pure', role TEXT,
                search_blob TEXT NOT NULL DEFAULT '', first_seen INTEGER NOT NULL,
                last_seen INTEGER NOT NULL, parser_rev INTEGER NOT NULL DEFAULT 0,
                manual_identity INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE capabilities (
                capability_id TEXT PRIMARY KEY,
                asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                name TEXT NOT NULL, summary TEXT NOT NULL DEFAULT '',
                tags TEXT NOT NULL DEFAULT '[]');
             CREATE TABLE relations (
                id INTEGER PRIMARY KEY AUTOINCREMENT, src_id TEXT NOT NULL,
                kind TEXT NOT NULL, target_id TEXT NOT NULL,
                narrowing TEXT, max_depth INTEGER);
             CREATE TABLE delegation_edges (
                src TEXT NOT NULL, dst TEXT NOT NULL,
                max_depth INTEGER NOT NULL DEFAULT 8, PRIMARY KEY (src, dst));
             PRAGMA user_version = 3;",
        )
        .unwrap();
        // 两条同内容资产（真重复）+ 一条无指纹资产（老库退化形态）
        conn.execute(
            "INSERT INTO assets (id,kind,name,manifest,identity,search_blob,first_seen,last_seen,content_hash,source_path)
             VALUES ('a1','skill','甲','{}','{}','x',1,1,'H1','short/path/SKILL.md')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO assets (id,kind,name,manifest,identity,search_blob,first_seen,last_seen,content_hash,source_path)
             VALUES ('a2','skill','甲','{}','{}','x',1,1,'H1','a/much/longer/path/SKILL.md')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO assets (id,kind,name,manifest,identity,search_blob,first_seen,last_seen,content_hash,source_path)
             VALUES ('a3','skill','丙','{}','{}','x',1,1,NULL,'zz/SKILL.md')",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        // 旧列一个都不能少
        for c in ["id", "content_hash", "source_path", "manifest", "manual_identity"] {
            assert!(has_column(&conn, "assets", c).unwrap(), "旧列 {c} 被删了");
        }
        // 新列补上了
        for c in ["logical_id", "location_id"] {
            assert!(has_column(&conn, "assets", c).unwrap(), "新列 {c} 没补上");
        }

        // 3 条资产 -> 2 个逻辑资产（H1 那两条合一，无指纹那条独立）
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM logical_assets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "同内容应折叠成 1 个逻辑资产");

        // 归一化：每条 assets 行都要能连到逻辑资产
        let linked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM assets WHERE logical_id IS NULL OR logical_id = ''",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 0, "每条资产都必须回填上 logical_id");

        // 短路径那条当 canonical
        let canon: String = conn
            .query_row(
                "SELECT location_id FROM asset_locations WHERE logical_id='l1-H1' AND role='canonical'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(canon, "loc-a1", "最短路径那条必须是 canonical");

        // 幂等：再迁移一次不炸也不改数
        migrate(&conn).unwrap();
        let n2: i64 = conn
            .query_row("SELECT COUNT(*) FROM logical_assets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, n2, "迁移必须幂等");
    }

    #[test]
    fn m15_asset_without_content_hash_is_not_lost() {
        // 回归：缺指纹的老行**不能**因为没法算逻辑 id 就被丢掉。
        // 少给一条资产 = 用户凭空看不到一个能力，比不去重糟糕得多。
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO assets (id,kind,name,manifest,identity,search_blob,first_seen,last_seen,content_hash,source_path)
             VALUES ('nohash','skill','无指纹','{}','{}','x',1,1,NULL,'p/SKILL.md')",
            [],
        )
        .unwrap();

        let reg = Registry { conn };
        // 拿一个**真的没有指纹**的 manifest 走一遍同步。
        // 这是极老库升级后的真实形态：行在库里，但 content_hash 是空的。
        let mut m = CapabilityManifest::new("nohash", AssetKind::Skill, "无指纹");
        m.provenance.content_hash = None;
        m.provenance.source_path = Some("p/SKILL.md".into());
        m.provides.push(ProvidedCapability {
            capability_id: "nohash::run".into(),
            name: "run".into(),
            summary: "跑".into(),
            input_schema: None,
            tags: vec![],
        });
        reg.upsert(&m).unwrap();

        let synced = reg.sync_logical(std::slice::from_ref(&m), 100).unwrap();
        assert_eq!(synced.logical_created, 1, "无指纹资产也要建出逻辑资产行");
        assert_eq!(
            synced.locations_seen, 1,
            "缺指纹不应中断同步（否则同批其他资产也建不出来）"
        );

        let n: i64 = reg
            .conn
            .query_row(
                "SELECT COUNT(*) FROM logical_assets WHERE logical_id LIKE 'raw-%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "无指纹资产必须有自己的逻辑资产行");
        assert_eq!(reg.total().unwrap(), 1, "资产本身一条都不能少");
    }

    #[test]
    fn query_filters_by_kind_and_text() {
        let r = Registry::open_in_memory().unwrap();
        let mut a = sample("s::alpha", "h1");
        a.name = "阿尔法".into();
        a.description = "图像处理".into();
        r.upsert(&a).unwrap();
        let mut b = sample("s::beta", "h2");
        b.name = "beta".into();
        b.description = "图像处理".into();
        r.upsert(&b).unwrap();

        let all = r.query_assets(&AssetQuery { limit: 50, ..Default::default() }).unwrap();
        assert_eq!(all.len(), 2);

        let only_text = r
            .query_assets(&AssetQuery {
                text: Some("图像".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(only_text.len(), 2, "描述里的中文要能搜到");

        let wrong_kind = r
            .query_assets(&AssetQuery {
                kind: Some("plugin".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert!(wrong_kind.is_empty());
    }

    #[test]
    fn query_escapes_like_wildcards() {
        let r = Registry::open_in_memory().unwrap();
        let mut a = sample("s::a", "h1");
        a.name = "under_score".into();
        r.upsert(&a).unwrap();
        let mut b = sample("s::b", "h2");
        b.name = "underXscore".into();
        r.upsert(&b).unwrap();

        // `_` 若不转义会当通配符，误命中 underXscore
        let hit = r
            .query_assets(&AssetQuery {
                text: Some("under_score".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hit.len(), 1, "只应命中下划线那个: {hit:?}");
        assert_eq!(hit[0].id, "s::a");
    }

    #[test]
    fn query_min_authority_uses_enum_order_not_string_order() {
        // 字符串字典序是 destructive < observe < read < write，
        // 直接拿字符串比会得出完全相反的结论，必须显式映射成等级。
        let r = Registry::open_in_memory().unwrap();
        for (id, auth) in [
            ("s::ro", ac_core::Authority::Read),
            ("s::ob", ac_core::Authority::Observe),
            ("s::wr", ac_core::Authority::Write),
            ("s::de", ac_core::Authority::Destructive),
        ] {
            let mut m = sample(id, &format!("h-{id}"));
            m.identity.authority = auth;
            r.upsert(&m).unwrap();
        }

        let got = r
            .query_assets(&AssetQuery {
                min_authority: Some("write".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        let ids: Vec<&str> = got.iter().map(|s| s.id.as_str()).collect();
        // 排序是 kind, name, id；name 都是「样例」，所以按 id 兜底
        assert_eq!(ids, vec!["s::de", "s::wr"]);
        assert_eq!(r.count_assets(&AssetQuery {
            min_authority: Some("write".into()),
            ..Default::default()
        }).unwrap(), 2);
    }

    #[test]
    fn query_returns_authority_label_and_cap_count() {
        let r = Registry::open_in_memory().unwrap();
        let mut m = sample("s::a", "h1");
        m.identity.authority = ac_core::Authority::Destructive;
        r.upsert(&m).unwrap();
        let got = r.query_assets(&AssetQuery { limit: 1, ..Default::default() }).unwrap();
        assert_eq!(got[0].authority, "destructive");
        assert_eq!(got[0].authority_label, "破坏性");
        assert_eq!(got[0].capability_count, 1, "sample 带 1 个 provides");
    }

    #[test]
    fn query_pagination_is_stable() {
        let r = Registry::open_in_memory().unwrap();
        for i in 0..25 {
            let mut m = sample(&format!("s::p{i:02}"), &format!("h{i}"));
            m.name = format!("item-{i:02}");
            r.upsert(&m).unwrap();
        }
        let page1 = r
            .query_assets(&AssetQuery { limit: 10, offset: 0, ..Default::default() })
            .unwrap();
        let page2 = r
            .query_assets(&AssetQuery { limit: 10, offset: 10, ..Default::default() })
            .unwrap();
        let page3 = r
            .query_assets(&AssetQuery { limit: 10, offset: 20, ..Default::default() })
            .unwrap();
        assert_eq!((page1.len(), page2.len(), page3.len()), (10, 10, 5));
        // 三页不能有交集
        let mut all: Vec<&str> = page1.iter().chain(&page2).chain(&page3).map(|s| s.id.as_str()).collect();
        all.sort_unstable();
        let before = all.len();
        all.dedup();
        assert_eq!(all.len(), before, "分页之间出现重复行");
    }

    #[test]
    fn query_limit_is_clamped() {
        let r = Registry::open_in_memory().unwrap();
        // limit=0 会让 UI 拿到空列表，这里必须兜住
        let got = r.query_assets(&AssetQuery { limit: 0, ..Default::default() }).unwrap();
        assert!(got.len() <= 1000);
    }

    #[test]
    fn remove_deletes_cascade_capabilities() {
        let r = Registry::open_in_memory().unwrap();
        let m = sample("s::gone", "h1");
        r.upsert(&m).unwrap();
        let before: i64 = r
            .conn
            .query_row("SELECT COUNT(*) FROM capabilities", [], |x| x.get(0))
            .unwrap();
        assert!(before > 0);

        assert!(r.remove("s::gone").unwrap());
        assert!(!r.remove("s::gone").unwrap(), "删第二次应返回 false");
        let after: i64 = r
            .conn
            .query_row("SELECT COUNT(*) FROM capabilities", [], |x| x.get(0))
            .unwrap();
        assert_eq!(after, 0, "外键 CASCADE 应连带清掉能力行");
    }

    #[test]
    fn counts_by_kind() {
        let r = Registry::open_in_memory().unwrap();
        let mut mcp = sample("m::x", "h1");
        mcp.kind = AssetKind::Mcp;
        r.upsert_all(&[sample("s::a", "h1"), sample("s::b", "h2"), mcp])
            .unwrap();
        let c = r.counts_by_kind().unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(r.total().unwrap(), 3);
    }

    #[test]
    fn detects_delegation_cycle() {
        let r = Registry::open_in_memory().unwrap();
        let mut a = sample("a", "h1");
        a.relations.push(ac_core::AssetRelation::new(ac_core::RelationKind::Delegates, "b"));
        let mut b = sample("b", "h1");
        b.relations.push(ac_core::AssetRelation::new(ac_core::RelationKind::Delegates, "a"));
        r.upsert(&a).unwrap();
        r.upsert(&b).unwrap();

        let cyc = r.find_cycle().unwrap();
        assert!(cyc.is_some(), "a->b->a 必须被检出为环");
    }

    #[test]
    fn no_false_positive_on_acyclic_graph() {
        let r = Registry::open_in_memory().unwrap();
        let mut a = sample("a", "h1");
        a.relations.push(ac_core::AssetRelation::new(ac_core::RelationKind::Delegates, "b"));
        let mut b = sample("b", "h1");
        b.relations.push(ac_core::AssetRelation::new(ac_core::RelationKind::Delegates, "c"));
        r.upsert_all(&[a, b, sample("c", "h1")]).unwrap();
        assert!(r.find_cycle().unwrap().is_none());
    }

    #[test]
    fn search_blob_contains_all_indexable_fields() {
        let m = sample("s::a", "h1");
        let blob = build_search_blob(&m);
        assert!(blob.contains("s::a"));
        assert!(blob.contains("描述"));
        assert!(blob.contains("run"));
        assert!(blob.contains("read_only"));
    }

    // ── M1.5：去重、append-only、标签筛选 ────────────────────────

    /// 造一个"同内容两个位置"的库。
    fn registry_with_duplicate(hashes: &[(&str, &str, &str)]) -> Registry {
        // hashes: (asset id, content hash, source path)
        let r = Registry::open_in_memory().unwrap();
        let mut ms = Vec::new();
        for (id, h, path) in hashes {
            let mut m = sample(id, h);
            m.provenance.source_path = Some((*path).into());
            ms.push(m);
        }
        r.upsert_all(&ms).unwrap();
        r.sync_logical(&ms, 1000).unwrap();
        r
    }

    #[test]
    fn m15_duplicate_content_shares_one_logical_asset() {
        let r = registry_with_duplicate(&[
            ("skill::a", "H1", "vendor/skill/a/SKILL.md"),
            ("skill::b", "H1", "vendor/pkg/refs/a/SKILL.md"),
            ("skill::c", "H1", "vendor/pkg2/deep/refs/a/SKILL.md"),
            ("skill::z", "H2", "vendor/skill/z/SKILL.md"),
        ]);

        assert_eq!(r.logical_total().unwrap(), 2, "3 份同内容 + 1 份独立 = 2 个逻辑资产");

        // canonical = 最短路径
        let locs = r.locations_of("l1-H1").unwrap();
        assert_eq!(locs.len(), 3);
        assert_eq!(locs[0].location_id, "loc-skill::a");
        assert_eq!(locs[0].role, ac_core::LocationRole::Canonical);
        assert_eq!(locs[1].role, ac_core::LocationRole::Alias);
    }

    #[test]
    fn m15_candidates_are_never_applied_automatically() {
        // **本里程碑最重要的一条测试**：
        // 出现重复内容后，程序只能把它们**列成候选**，
        // 绝不能自己就把 role 改成"已合并"。
        let r = registry_with_duplicate(&[
            ("skill::a", "H1", "vendor/skill/a/SKILL.md"),
            ("skill::b", "H1", "vendor/pkg/refs/a/SKILL.md"),
        ]);

        let cands = r.dedup_candidates().unwrap();
        assert_eq!(cands.len(), 1, "应当正好一组候选");
        assert_eq!(cands[0].match_level, MatchLevel::L1);
        assert_eq!(cands[0].location_ids.len(), 2);

        // 关键：库里的角色**一个都没被合并**
        let merged: i64 = r
            .conn
            .query_row(
                "SELECT COUNT(*) FROM asset_locations WHERE role = 'merged'",
                [],
                |x| x.get(0),
            )
            .unwrap();
        assert_eq!(merged, 0, "没有用户确认过，不允许出现 merged");

        // 而且逻辑资产数量没变（= distinct content_hash 数）
        assert_eq!(r.logical_total().unwrap(), r.distinct_l1_count().unwrap());
        // 裁决历史为空
        assert!(r.dedup_history().unwrap().is_empty());
    }

    #[test]
    fn m15_merge_is_only_applied_after_explicit_decision() {
        let r = registry_with_duplicate(&[
            ("skill::a", "H1", "vendor/skill/a/SKILL.md"),
            ("skill::b", "H1", "vendor/pkg/refs/a/SKILL.md"),
            ("skill::c", "H1", "vendor/pkg2/refs/a/SKILL.md"),
        ]);
        let key = r.dedup_candidates().unwrap()[0].group_key.clone();

        assert!(r
            .decide_dedup(&key, MatchLevel::L1, DedupDecision::Merge)
            .unwrap());

        // 只有 canonical 留下，其他变 merged
        let roles: Vec<(String, String)> = {
            let mut st = r
                .conn
                .prepare("SELECT location_id, role FROM asset_locations ORDER BY location_id")
                .unwrap();
            let rows = st
                .query_map([], |x| Ok((x.get::<_, String>(0)?, x.get::<_, String>(1)?)))
                .unwrap();
            rows.collect::<Result<Vec<_>, _>>().unwrap()
        };
        assert_eq!(
            roles.iter().filter(|(_, role)| role == "merged").count(),
            2,
            "两个别名应被标记为已合并"
        );
        assert_eq!(roles.iter().filter(|(_, role)| role == "canonical").count(), 1);

        // 逻辑资产**仍然是一个**：合并是"折叠展示"，不是删数据
        assert_eq!(r.logical_total().unwrap(), 1);

        // 历史留痕
        let h = r.dedup_history().unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].decision, "merge");
    }

    #[test]
    fn m15_keep_both_splits_into_independent_logical_assets() {
        // 共存 = 用户说"它们不是同一份"，多版本/多副本必须各自独立。
        // 这也是"不折叠到 latest"的落地：挑哪个版本始终是用户的事。
        let r = registry_with_duplicate(&[
            ("skill::a", "H1", "vendor/skill/a/SKILL.md"),
            ("skill::b", "H1", "vendor/pkg/refs/a/SKILL.md"),
        ]);
        let key = r.dedup_candidates().unwrap()[0].group_key.clone();

        assert!(r
            .decide_dedup(&key, MatchLevel::L1, DedupDecision::KeepBoth)
            .unwrap());

        assert_eq!(
            r.logical_total().unwrap(),
            2,
            "共存后两个位置必须是两个独立逻辑资产"
        );
        // 原始逻辑资产组不再有成员挂在上面
        let left: i64 = r
            .conn
            .query_row(
                "SELECT COUNT(*) FROM asset_locations WHERE logical_id = 'l1-H1'",
                [],
                |x| x.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "原组应被拆空");
    }

    #[test]
    fn m15_ignore_leaves_everything_untouched_and_asks_again() {
        let r = registry_with_duplicate(&[
            ("skill::a", "H1", "vendor/skill/a/SKILL.md"),
            ("skill::b", "H1", "vendor/pkg/refs/a/SKILL.md"),
        ]);
        let key = r.dedup_candidates().unwrap()[0].group_key.clone();

        assert!(r
            .decide_dedup(&key, MatchLevel::L1, DedupDecision::Ignore)
            .unwrap());

        // 角色原封不动
        let merged: i64 = r
            .conn
            .query_row(
                "SELECT COUNT(*) FROM asset_locations WHERE role IN ('merged','kb')",
                [],
                |x| x.get(0),
            )
            .unwrap();
        assert_eq!(merged, 0, "忽略不应改变任何角色");
        assert_eq!(r.logical_total().unwrap(), 1);

        // 下次扫描还会再问一次
        assert_eq!(r.dedup_candidates().unwrap().len(), 1, "忽略后仍需再次询问");

        // 但历史里有记录
        assert_eq!(r.dedup_history().unwrap()[0].decision, "ignore");
    }

    #[test]
    fn m15_disappeared_asset_becomes_alias_not_deletion() {
        // **append-only 的核心证明**。
        //
        // 场景：资产 A 和 B 内容相同（A 短、B 长，A 是 canonical）。
        // 下一轮扫描磁盘上只剩 A 了。
        //
        // 期望：`assets` 表清掉 B（M1 既有行为），
        // 但 `asset_locations` 里 B 那条**留着**，只是不再被刷新 ——
        // 用户因此能查到"上次这里还有一版"。
        let r = registry_with_duplicate(&[
            ("skill::a", "H1", "vendor/skill/a/SKILL.md"),
            ("skill::b", "H1", "vendor/pkg/refs/a/SKILL.md"),
        ]);
        assert_eq!(r.locations_of("l1-H1").unwrap().len(), 2);

        // 第二轮：磁盘上只剩 a。对账删掉 assets 里的 b。
        let mut a = sample("skill::a", "H1");
        a.provenance.source_path = Some("vendor/skill/a/SKILL.md".into());
        r.upsert_all(std::slice::from_ref(&a)).unwrap();
        let seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::from(["skill::a"]);
        for id in r.list_ids().unwrap() {
            if !seen.contains(id.as_str()) {
                r.remove(&id).unwrap();
            }
        }
        r.sync_logical(&[a], 2000).unwrap();

        // 1) assets 表确实少了 b
        assert!(r.get_manifest("skill::b").unwrap().is_none());

        // 2) **位置痕迹还在** —— 这才是 append-only 的意义
        let locs = r.locations_of("l1-H1").unwrap();
        assert_eq!(
            locs.len(),
            2,
            "消失的资产必须留下位置记录，不能被抹掉：{locs:?}"
        );

        // 3) 它的 last_seen_at 停在上一轮，没有被刷新
        let gone = locs.iter().find(|l| l.location_id == "loc-skill::b").unwrap();
        assert_eq!(gone.last_seen_at, 1000, "未被刷新，时间戳应停在最后一次见到它");

        // 4) 它仍是 alias（canonical 是短的 a）
        assert_eq!(gone.role, ac_core::LocationRole::Alias);
    }

    #[test]
    fn m15_multi_version_coexists_without_latest_collapse() {
        // **多版本共存**：0.1.0 与 1.0.0 正文不同 -> 两个独立逻辑资产。
        // 绝不折叠成 "latest"：挑版本是用户决策，不是程序决策。
        let r = Registry::open_in_memory().unwrap();
        let mut v1 = sample("pkg::v0-1-0", "H_v010");
        v1.version = "0.1.0".into();
        v1.name = "架构可视化".into();
        v1.provenance.source_path = Some("vendor/pkg/0.1.0/SKILL.md".into());
        let mut v2 = sample("pkg::v1-0-0", "H_v100");
        v2.version = "1.0.0".into();
        v2.name = "架构可视化".into();
        v2.provenance.source_path = Some("vendor/pkg/1.0.0/SKILL.md".into());

        r.upsert_all(&[v1.clone(), v2.clone()]).unwrap();
        r.sync_logical(&[v1, v2], 1000).unwrap();

        assert_eq!(
            r.logical_total().unwrap(),
            2,
            "两个版本必须是两个逻辑资产，不能折叠成一个"
        );
        // 没有去重候选 —— 它们本来就不重复
        assert!(r.dedup_candidates().unwrap().is_empty());

        // 版本号两���都在，且都拿得到
        let assets = r.logical_assets(10).unwrap();
        let mut versions: Vec<&str> = assets.iter().map(|a| a.version.as_str()).collect();
        versions.sort_unstable();
        assert_eq!(versions, vec!["0.1.0", "1.0.0"]);
    }

    // ── 标签筛选（M1 审计硬缺口）───────────────────────────────

    fn tagged(id: &str, hash: &str, tags: &[&str]) -> CapabilityManifest {
        let mut m = sample(id, hash);
        m.provides[0].tags = tags.iter().map(|s| (*s).to_string()).collect();
        m
    }

    #[test]
    fn tag_filter_selects_assets_carrying_that_tag() {
        let r = Registry::open_in_memory().unwrap();
        r.upsert_all(&[
            tagged("skill::a", "h1", &["read_only", "author:数据"]),
            tagged("skill::b", "h2", &["write"]),
            tagged("skill::c", "h3", &["read_only"]),
        ])
        .unwrap();

        let hit = r
            .query_assets(&AssetQuery {
                tag: Some("read_only".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hit.len(), 2, "read_only 应命中 a 与 c");

        let author = r
            .query_assets(&AssetQuery {
                tag: Some("author:数据".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(author.len(), 1, "作者标签也应能筛");
        assert_eq!(author[0].id, "skill::a");

        let none = r
            .query_assets(&AssetQuery {
                tag: Some("不存在的标签".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn tag_filter_count_matches_list_otherwise_pagination_breaks() {
        // 回归：list 与 count 的 WHERE 必须一致。
        // 不一致时列表少显示几行、总数又多几个，分页就永远翻不到底。
        let r = Registry::open_in_memory().unwrap();
        r.upsert_all(&[
            tagged("skill::a", "h1", &["read_only"]),
            tagged("skill::b", "h2", &["read_only"]),
            tagged("skill::c", "h3", &["write"]),
        ])
        .unwrap();

        for tag in ["read_only", "write"] {
            let q = AssetQuery {
                tag: Some(tag.into()),
                limit: 1,
                offset: 0,
                ..Default::default()
            };
            let total = r.count_assets(&q).unwrap();
            let page = r.query_assets(&q).unwrap();
            assert_eq!(
                page.len() as u32,
                total.min(1),
                "tag={tag}: 本页行数应等于 min(limit, total)"
            );
        }
    }

    #[test]
    fn tag_filter_composes_with_kind_and_authority() {
        let r = Registry::open_in_memory().unwrap();
        // 注意：`sample` 固定产出 skill，所以这里必须真造一个 plugin。
        // 否则 kind 过滤会"看起来通过"，其实一条 plugin 都没筛到 ——
        // 这正是原版测试的漏洞：它只断言了 id，从没验证过 kind 真生效。
        let mut plugin = tagged("plugin::b", "h2", &["read_only"]);
        plugin.kind = AssetKind::Plugin;
        r.upsert_all(&[tagged("skill::a", "h1", &["read_only"]), plugin])
            .unwrap();

        let only_plugin = r
            .query_assets(&AssetQuery {
                kind: Some("plugin".into()),
                tag: Some("read_only".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(only_plugin.len(), 1);
        assert_eq!(only_plugin[0].id, "plugin::b");

        // 反向：只筛 skill 时不能混进 plugin
        let only_skill = r
            .query_assets(&AssetQuery {
                kind: Some("skill".into()),
                tag: Some("read_only".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(only_skill.len(), 1);
        assert_eq!(only_skill[0].id, "skill::a");
    }

    #[test]
    fn tag_inventory_lists_counts_descending() {
        let r = Registry::open_in_memory().unwrap();
        r.upsert_all(&[
            tagged("skill::a", "h1", &["read_only"]),
            tagged("skill::b", "h2", &["read_only", "write"]),
            tagged("skill::c", "h3", &["write"]),
        ])
        .unwrap();

        let tags = r.all_tags().unwrap();
        assert_eq!(tags[0].tag, "read_only");
        assert_eq!(tags[0].count, 2);
        assert_eq!(tags[1].tag, "write");
        assert_eq!(tags[1].count, 2);
    }

    #[test]
    fn multi_capability_asset_is_not_duplicated_by_tag_filter() {
        // 回归：EXISTS 而非 JOIN。一条资产有多个能力都带这个标签时，
        // JOIN 会把它在列表里显示两遍。
        let r = Registry::open_in_memory().unwrap();
        let mut m = tagged("skill::a", "h1", &["read_only"]);
        m.provides.push(ac_core::ProvidedCapability {
            capability_id: "skill::a::second".into(),
            name: "second".into(),
            summary: "第二个能力".into(),
            input_schema: None,
            tags: vec!["read_only".into()],
        });
        r.upsert_all(&[m]).unwrap();

        let hit = r
            .query_assets(&AssetQuery {
                tag: Some("read_only".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hit.len(), 1, "一个资产只能出现一次");
    }

    // ── M3 产品化：v4 → v5 迁移 ────────────────────────────────────

    /// 迁移**只加表**这一条纪律的可执行版本。
    ///
    /// 步骤：建 v5 库 → 塞资产 → 再开一次让 M1.5 回填把逻辑资产/位置坐实
    /// → 记下基线计数 → 回滚到 v4 → **用裸连接**核对（裸连接不跑 migrate，
    /// 才能看清回滚到底动了什么）→ 再跑 migrate → 逐表核对。
    ///
    /// 任何一张既有表在任一步被改动、丢行或重建，这条测试立刻红。
    #[test]
    fn migration_v5_preserves_existing_counts() {
        let dir = crate::testutil::Scratch::new("v5-migrate");
        let db = dir.path().join("registry.db");
        {
            let r = Registry::open(&db).unwrap();
            r.upsert_all(&[sample("skill::a", "h1"), sample("skill::b", "h1")])
                .unwrap();
        }
        // 第二次打开：此时 assets 非空，M1.5 的回填会把 logical_assets /
        // asset_locations 补齐，于是后面量到的是**稳定**的计数。
        let baseline = {
            let c = Connection::open(&db).unwrap();
            migrate(&c).unwrap();
            snapshot(&c)
        };
        assert_eq!(baseline[0], 2, "assets 基线");
        assert_eq!(baseline[2], 1, "两条同内容资产应折叠成 1 个逻辑资产");

        // ── 回滚 ──
        let after_rollback = {
            let c = Connection::open(&db).unwrap();
            rollback_v5(&c).unwrap();
            assert_eq!(user_version(&c).unwrap(), 4);
            let names = crate::agents::table_names(&c).unwrap();
            // v8 之后 `agent_hosts` 也是 agent 前缀的表，但它属于 v8 的新增面，
            // rollback_v5 不该动它。所以判据收窄到 v5 真正新增的那三张——
            // 「回滚面精确等于新增面」这条纪律没变，变的是新增面里多了别的表。
            for gone in ["agents", "agent_heartbeat", "agent_capability_usage"] {
                assert!(!names.iter().any(|n| n == gone), "rollback_v5 漏删 {gone}");
            }
            assert!(
                names.iter().any(|n| n == "agent_hosts"),
                "rollback_v5 不该删 v8 的 agent_hosts"
            );
            // 裸连接不跑 migrate，量到的就是回滚后的真实状态。
            snapshot(&c)
        };
        assert_eq!(
            after_rollback, baseline,
            "回滚动了既有表的数据（只允许删新增表）"
        );

        // ── 重新迁移 ──
        let after_migrate = {
            let c = Connection::open(&db).unwrap();
            migrate(&c).unwrap();
            assert_eq!(user_version(&c).unwrap(), SCHEMA_VERSION);
            let names = crate::agents::table_names(&c).unwrap();
            for t in ["agents", "agent_heartbeat", "agent_capability_usage"] {
                assert!(names.iter().any(|n| n == t), "重新迁移后缺表 {t}");
            }
            snapshot(&c)
        };
        assert_eq!(after_migrate, baseline, "重新迁移后既有表计数必须一模一样");
    }

    /// 既有七张表的行数快照，顺序固定：
    /// assets / capabilities / logical_assets / asset_locations /
    /// relations / delegation_edges / dedup_candidates。
    fn snapshot(conn: &Connection) -> Vec<i64> {
        ["assets", "capabilities", "logical_assets", "asset_locations", "relations",
         "delegation_edges", "dedup_candidates"]
            .iter()
            .map(|t| {
                conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                    .unwrap_or(-1)
            })
            .collect()
    }

    /// 回滚必须**只删新表**。这条测试就是"可回滚"这个词的兑现：
    /// 回滚后既有表原样还在、数据一行不少、三张新表消失、版本号退回 4。
    #[test]
    fn rollback_v5_drops_only_the_new_tables() {
        let dir = crate::testutil::Scratch::new("v5-rollback");
        let db = dir.path().join("registry.db");
        let r = Registry::open(&db).unwrap();
        r.upsert_all(&[sample("skill::a", "h1"), sample("skill::b", "h2")])
            .unwrap();
        r.register_static_agents().unwrap();
        {
            let c = r.conn;
            let before = c
                .query_row("SELECT COUNT(*) FROM assets", [], |x| x.get::<_, i64>(0))
                .unwrap();
            rollback_v5(&c).unwrap();
            let names = crate::agents::table_names(&c).unwrap();
            assert!(!names.iter().any(|n| n == "agents"), "agents 还在: {names:?}");
            assert!(
                !names.iter().any(|n| n == "agent_heartbeat"),
                "agent_heartbeat 还在"
            );
            assert!(
                !names.iter().any(|n| n == "agent_capability_usage"),
                "agent_capability_usage 还在"
            );
            assert!(names.iter().any(|n| n == "assets"), "assets 被误删");
            assert_eq!(user_version(&c).unwrap(), 4, "版本号应退回 4");
            let after = c
                .query_row("SELECT COUNT(*) FROM assets", [], |x| x.get::<_, i64>(0))
                .unwrap();
            assert_eq!(after, before, "回滚动了既有表的数据");
        }
    }

    /// 三张新表必须在迁移后存在，且列名与文档一致。
    #[test]
    fn migration_creates_all_three_agent_tables() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let names = crate::agents::table_names(&conn).unwrap();
        for t in ["agents", "agent_heartbeat", "agent_capability_usage"] {
            assert!(names.iter().any(|n| n == t), "缺表 {t}: {names:?}");
        }
    }

    // ── v7（2026-10-18）：来源 / 注册方式四列 ────────────────────────

    #[test]
    fn migration_v7_adds_the_four_origin_columns() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        for col in ["origin_kind", "registration_kind", "origin_agent", "self_contained"] {
            assert!(has_column(&conn, "assets", col).unwrap(), "缺列 {col}");
        }
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v6_database_is_upgraded_and_origin_columns_are_backfilled() {
        // 回归：**这是本缺陷的核心场景**。
        //
        // 老库 506 行内容一个字节都没变，下次扫描会全部走 unchanged 分支、
        // 只刷 last_seen。如果四列只在 upsert 里写，它们会永远停在空串 / 0 ——
        // 用户点开界面看到的还是"没有来源"，即缺陷本身没修。
        let dir = crate::testutil::Scratch::new("v7-backfill");
        let db = dir.path().join("registry.db");

        // 1) 正常建库（v7）并写入一条 vendor 下的 mounted MCP。
        {
            let reg = Registry::open(&db).unwrap();
            let mut m = CapabilityManifest::new("mcp::ext", AssetKind::Mcp, "外部 venv");
            m.provenance.source_path = Some("..\\vendor\\mcp\\ext\\.mcp.json".into());
            m.provenance.content_hash = Some("h-ext".into());
            m.provenance.runtime_refs = vec!["D:/venv/Scripts/python.exe".into()];
            reg.upsert(&m).unwrap();
        }

        // 2) 把四列清成 ALTER 的 DEFAULT、版本号退回 6，精确模拟
        //    「v6 库刚被 ALTER 加完列、还没回填」的那一刻。
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "UPDATE assets SET origin_kind = '', registration_kind = '',
                        origin_agent = '', self_contained = 0;",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 6).unwrap();
        }

        // 3) 再次 migrate：四列必须被**回填**，不能等下次扫描。
        {
            let conn = Connection::open(&db).unwrap();
            migrate(&conn).unwrap();
            assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
            let row: (String, String, i64) = conn
                .query_row(
                    "SELECT origin_kind, registration_kind, self_contained FROM assets
                     WHERE id = 'mcp::ext'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(row.0, "vendor", "来源必须从 source_path 回填出来");
            assert_eq!(row.1, "mounted", "盘符绝对路径必须判成挂载");
            assert_eq!(row.2, 0, "挂了外部 venv 就不能算自包含");
        }
    }

    #[test]
    fn upsert_writes_the_four_origin_columns() {
        let r = Registry::open_in_memory().unwrap();
        let mut m = sample("mcp::a", "h1");
        m.kind = AssetKind::Mcp;
        m.provenance.source_path = Some("..\\vendor\\mcp\\a\\.mcp.json".into());
        m.provenance.runtime_refs = vec!["uvx".into()];
        r.upsert(&m).unwrap();
        let got = r
            .query_assets(&AssetQuery { limit: 10, ..Default::default() })
            .unwrap();
        let row = got.iter().find(|x| x.id == "mcp::a").expect("必须查得到");
        assert_eq!(row.origin_kind, "vendor");
        assert_eq!(row.registration_kind, "embedded");
        assert!(row.self_contained);
    }

    #[test]
    fn unchanged_rows_keep_their_origin_columns() {
        // 内容没变（走 unchanged 分支）时四列不该被写成空——那样界面会闪一下
        // "没有来源"。这条钉的是 unchanged 分支不破坏四列的语义。
        let r = Registry::open_in_memory().unwrap();
        let mut m = sample("mcp::b", "h1");
        m.kind = AssetKind::Mcp;
        m.provenance.source_path = Some("..\\vendor\\mcp\\b\\.mcp.json".into());
        m.provenance.runtime_refs = vec!["npx".into()];
        assert_eq!(r.upsert(&m).unwrap(), "inserted");
        assert_eq!(r.upsert(&m).unwrap(), "unchanged");
        let got = r
            .query_assets(&AssetQuery { limit: 10, ..Default::default() })
            .unwrap();
        let row = got.iter().find(|x| x.id == "mcp::b").expect("必须查得到");
        assert_eq!(row.origin_kind, "vendor", "unchanged 之后四列不能被清空");
        assert_eq!(row.registration_kind, "embedded");
    }

    // ── 缺陷 C：L2 不能把 L1 的组再报一遍 ──────────────────────────

    /// 造库：每条带 (id, content_hash, source_path, normalized_hash)。
    fn registry_with_norm(rows: &[(&str, &str, &str, &str)]) -> Registry {
        let r = Registry::open_in_memory().unwrap();
        let mut ms = Vec::new();
        for (id, h, path, norm) in rows {
            let mut m = sample(id, h);
            m.provenance.source_path = Some((*path).into());
            m.provenance.normalized_hash = Some((*norm).into());
            ms.push(m);
        }
        r.upsert_all(&ms).unwrap();
        r.sync_logical(&ms, 1000).unwrap();
        r
    }

    #[test]
    fn dedup_l2_does_not_reecho_l1_groups() {
        // **这是 54 → 27 的最小可复现模型。**
        //
        // 两组 L1 重复（同 content_hash 各 2 个位置），组内共享 normalized_hash。
        // 旧公式按 normalized_hash 分组、只判 COUNT(DISTINCT location_id) > 1，
        // 于是把这两组**又报一遍** L2；队列 = 2 + 2 = 4，而真实重复只有 2 组。
        let r = registry_with_norm(&[
            ("skill::a1", "H1", "vendor/a/1/SKILL.md", "N1"),
            ("skill::a2", "H1", "vendor/a/2/SKILL.md", "N1"),
            ("skill::b1", "H2", "vendor/b/1/SKILL.md", "N2"),
            ("skill::b2", "H2", "vendor/b/2/SKILL.md", "N2"),
        ]);
        let cands = r.dedup_candidates().unwrap();
        let l1 = cands.iter().filter(|c| c.match_level == MatchLevel::L1).count();
        let l2 = cands.iter().filter(|c| c.match_level == MatchLevel::L2).count();
        assert_eq!(l1, 2, "两组 L1 重复");
        assert_eq!(
            l2, 0,
            "同一 logical_id 内的多位置不是 L2 候选：L2 的定义是「L1 不同」"
        );
        assert_eq!(cands.len(), 2, "队列总数必须是 2，不是 4");
    }

    // ── v8：生命周期 / 作者 / §迁移 ──────────────────────────────

    #[test]
    fn migration_v8_adds_lifecycle_and_author_columns() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        for col in ["lifecycle", "author", "author_source"] {
            assert!(has_column(&conn, "assets", col).unwrap(), "缺列 {col}");
        }
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
        // 新表也要在
        let names = crate::agents::table_names(&conn).unwrap();
        assert!(names.iter().any(|n| n == "agent_hosts"), "缺表 agent_hosts");
    }

    #[test]
    fn v7_database_is_upgraded_and_author_is_backfilled() {
        // 回归：内容没变的老行走 unchanged 分支，作者列会永远停在 ''。
        // 迁移必须自己把它补上，否则用户点开属性栏看到的还是"没有作者"。
        let dir = crate::testutil::Scratch::new("v8-backfill");
        let db = dir.path().join("registry.db");
        {
            let reg = Registry::open(&db).unwrap();
            let mut m = CapabilityManifest::new("skill::ml", AssetKind::Skill, "matlab 技能");
            m.provenance.source_path = Some(
                "C:/work/capability-core/vendor/skill/matlab/SKILL.md".into(),
            );
            m.provenance.content_hash = Some("h-ml".into());
            reg.upsert(&m).unwrap();
        }
        // 把作者列清成 ALTER 的 DEFAULT、版本号退回 7，精确模拟
        // 「v7 库刚被 ALTER 加完列、还没回填」的那一刻。
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "UPDATE assets SET author = '', author_source = '', lifecycle = 'active';",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 7).unwrap();
        }
        {
            let conn = Connection::open(&db).unwrap();
            migrate(&conn).unwrap();
            assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
            let row: (String, String, String) = conn
                .query_row(
                    "SELECT author, author_source, lifecycle FROM assets WHERE id = 'skill::ml'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(row.0, "matlab", "包路径必须回填成作者");
            assert_eq!(row.1, "package", "兜底必须自报是包推断");
            assert_eq!(row.2, "active", "老资产默认就是 active");
        }
    }

    #[test]
    fn v8_backfill_does_not_overwrite_an_existing_author() {
        // 幂等 + 不降级：已经有作者的资产，重跑迁移不能被包名覆盖。
        let dir = crate::testutil::Scratch::new("v8-backfill-keep");
        let db = dir.path().join("registry.db");
        let reg = Registry::open(&db).unwrap();
        let mut m = sample("skill::x", "hx");
        m.provenance.source_path = Some("vendor/skill/matlab/SKILL.md".into());
        reg.upsert(&m).unwrap();
        reg.conn
            .execute(
                "UPDATE assets SET author = 'MathWorks', author_source = 'frontmatter' WHERE id = 'skill::x'",
                [],
            )
            .unwrap();
        drop(reg);
        let conn = Connection::open(&db).unwrap();
        migrate(&conn).unwrap();
        let (a, s): (String, String) = conn
            .query_row(
                "SELECT author, author_source FROM assets WHERE id='skill::x'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(a, "MathWorks", "自声明不能被包名降级");
        assert_eq!(s, "frontmatter");
    }

    #[test]
    fn set_asset_lifecycle_switches_state_and_reports_hit() {
        let r = Registry::open_in_memory().unwrap();
        r.upsert(&sample("s::a", "h1")).unwrap();
        assert_eq!(r.asset_lifecycle("s::a").unwrap().as_deref(), Some("active"));

        assert!(r.set_asset_lifecycle("s::a", "paused").unwrap());
        assert_eq!(r.asset_lifecycle("s::a").unwrap().as_deref(), Some("paused"));

        assert!(r.set_asset_lifecycle("s::a", "archived").unwrap());
        assert_eq!(r.asset_lifecycle("s::a").unwrap().as_deref(), Some("archived"));

        // 不存在的 id：返回 false 而不是 Ok(()) —— 调用方必须能区分
        assert!(!r.set_asset_lifecycle("不存在", "paused").unwrap());
        assert!(r.asset_lifecycle("不存在").unwrap().is_none());
    }

    #[test]
    fn set_asset_lifecycle_rejects_unknown_state() {
        // 静默接受错拼的状态会让资产凭空消失或永远不出现，
        // 而调用方从返回值里看不出来。
        let r = Registry::open_in_memory().unwrap();
        r.upsert(&sample("s::a", "h1")).unwrap();
        for bad in ["deleted", "archivedx", "", "active; DROP TABLE assets"] {
            let e = r.set_asset_lifecycle("s::a", bad);
            assert!(e.is_err(), "非法状态 {bad:?} 必须被拒");
        }
        // 大小写与空白归一化后合法
        assert!(r.set_asset_lifecycle("s::a", "  Archived ").is_ok());
        assert_eq!(r.asset_lifecycle("s::a").unwrap().as_deref(), Some("archived"));
    }

    #[test]
    fn archived_assets_are_hidden_by_default_and_visible_on_demand() {
        // 用户的「删除」= archived。默认列表必须看不见它，
        // 否则"删除"在界面上等于没发生。
        let r = Registry::open_in_memory().unwrap();
        r.upsert_all(&[sample("s::keep", "h1"), sample("s::gone", "h2")])
            .unwrap();
        r.set_asset_lifecycle("s::gone", "archived").unwrap();

        let visible = r
            .query_assets(&AssetQuery { limit: 50, ..Default::default() })
            .unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "s::keep");
        // count 必须与 list 口径一致，否则分页会永远翻不到底
        assert_eq!(
            r.count_assets(&AssetQuery { limit: 50, ..Default::default() })
                .unwrap(),
            1
        );

        let with_archived = r
            .query_assets(&AssetQuery {
                include_archived: true,
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(with_archived.len(), 2, "显式要看时归档的必须出现");
        let gone = with_archived.iter().find(|x| x.id == "s::gone").unwrap();
        assert_eq!(gone.lifecycle, "archived");
        assert_eq!(
            r.count_assets(&AssetQuery {
                include_archived: true,
                limit: 50,
                ..Default::default()
            })
            .unwrap(),
            2
        );
    }

    #[test]
    fn archived_asset_row_is_never_physically_deleted() {
        // 项目铁律：增量不删。归档之后这一行、它的能力、它的位置痕迹都还在。
        let r = Registry::open_in_memory().unwrap();
        r.upsert(&sample("s::a", "h1")).unwrap();
        r.sync_logical(&[sample("s::a", "h1")], 1000).unwrap();
        r.set_asset_lifecycle("s::a", "archived").unwrap();
        assert_eq!(r.total().unwrap(), 1, "归档不能减行");
        let caps: i64 = r
            .conn
            .query_row("SELECT COUNT(*) FROM capabilities WHERE asset_id='s::a'", [], |x| x.get(0))
            .unwrap();
        assert_eq!(caps, 1, "能力条目必须留着");
        let locs: i64 = r
            .conn
            .query_row("SELECT COUNT(*) FROM asset_locations WHERE asset_id='s::a'", [], |x| x.get(0))
            .unwrap();
        assert_eq!(locs, 1, "位置痕迹必须留着");
        // 可以恢复
        assert!(r.set_asset_lifecycle("s::a", "active").unwrap());
        assert_eq!(
            r.query_assets(&AssetQuery { limit: 10, ..Default::default() })
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn lifecycle_round_trip_active_paused_archived_active() {
        // 用户界面上那三个按钮（激活 / 暂停 / 删除）走的就是这一条路径。
        // 全程必须：① 每次切换都命中；② 列表可见性跟着变；③ 一行都不少。
        let r = Registry::open_in_memory().unwrap();
        r.upsert(&sample("s::btn", "h1")).unwrap();
        let visible = |reg: &Registry| -> bool {
            reg.query_assets(&AssetQuery {
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .iter()
            .any(|a| a.id == "s::btn")
        };
        let state = |reg: &Registry| reg.asset_lifecycle("s::btn").unwrap().unwrap();

        // 起点
        assert_eq!(state(&r), "active");
        assert!(visible(&r));

        // ① 激活 → 暂停：仍在列表里（暂停不是删除）
        assert!(r.set_asset_lifecycle("s::btn", "paused").unwrap());
        assert_eq!(state(&r), "paused");
        assert!(visible(&r), "暂停的资产必须还看得见，否则用户找不到怎么恢复");

        // ② 暂停 → 归档（用户的「删除」）：默认列表看不见了
        assert!(r.set_asset_lifecycle("s::btn", "archived").unwrap());
        assert_eq!(state(&r), "archived");
        assert!(!visible(&r), "归档之后默认列表必须看不见");
        // 但显式要看时还在，且行没有被物理删除
        let all = r
            .query_assets(&AssetQuery {
                include_archived: true,
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(all.len(), 1, "归档不是物理删除");
        assert_eq!(all[0].lifecycle, "archived");
        assert_eq!(r.total().unwrap(), 1);

        // ③ 归档 → 激活：完整回到起点（误删可一键恢复）
        assert!(r.set_asset_lifecycle("s::btn", "active").unwrap());
        assert_eq!(state(&r), "active");
        assert!(visible(&r), "恢复后必须重新出现在默认列表里");
        assert_eq!(r.total().unwrap(), 1);

        // ④ 直接 active → archived（用户不经过暂停直接删）也必须成立
        assert!(r.set_asset_lifecycle("s::btn", "archived").unwrap());
        assert!(!visible(&r));
        assert!(r.set_asset_lifecycle("s::btn", "paused").unwrap());
        assert!(visible(&r), "从归档直接暂停也应该能回来");
    }

    #[test]
    fn lifecycle_counts_groups_by_state() {
        let r = Registry::open_in_memory().unwrap();
        r.upsert_all(&[
            sample("s::a", "h1"),
            sample("s::b", "h2"),
            sample("s::c", "h3"),
        ])
        .unwrap();
        r.set_asset_lifecycle("s::b", "paused").unwrap();
        r.set_asset_lifecycle("s::c", "archived").unwrap();
        let counts: std::collections::BTreeMap<String, u32> = r
            .lifecycle_counts()
            .unwrap()
            .into_iter()
            .map(|k| (k.kind, k.count))
            .collect();
        assert_eq!(counts.get("active"), Some(&1));
        assert_eq!(counts.get("paused"), Some(&1));
        assert_eq!(counts.get("archived"), Some(&1));
    }

    #[test]
    fn upsert_never_resurrects_a_paused_asset() {
        // 最要命的一条：upsert 是"磁盘上有什么"的写入路径，每次扫描都跑。
        // 如果它顺带把 lifecycle 写成 active，用户归档的资产下次扫描就复活了。
        let r = Registry::open_in_memory().unwrap();
        let m = sample("s::a", "h1");
        r.upsert(&m).unwrap();
        r.set_asset_lifecycle("s::a", "archived").unwrap();
        // 同内容重扫（unchanged 分支）
        assert_eq!(r.upsert(&m).unwrap(), "unchanged");
        assert_eq!(r.asset_lifecycle("s::a").unwrap().as_deref(), Some("archived"));
        // 内容变了（updated 分支）也不能复活
        assert_eq!(r.upsert(&sample("s::a", "h2")).unwrap(), "updated");
        assert_eq!(r.asset_lifecycle("s::a").unwrap().as_deref(), Some("archived"));
    }

    #[test]
    fn new_asset_starts_active() {
        let r = Registry::open_in_memory().unwrap();
        r.upsert(&sample("s::a", "h1")).unwrap();
        assert_eq!(r.asset_lifecycle("s::a").unwrap().as_deref(), Some("active"));
    }

    #[test]
    fn query_rows_carry_lifecycle_author_and_host_support() {
        let r = Registry::open_in_memory().unwrap();
        let mut m = CapabilityManifest::new("plugin::p", AssetKind::Plugin, "插件");
        m.provenance.source_path = Some("D:/v/vendor/plugin/obsidian-suite/.qoder-plugin/plugin.json".into());
        m.provenance.source_format = Some("plugin_json".into());
        m.provenance.content_hash = Some("h1".into());
        m.provenance.author = Some("QoderWork".into());
        m.provenance.author_source = Some(crate::author::SOURCE_MANIFEST.into());
        r.upsert(&m).unwrap();

        let row = r
            .query_assets(&AssetQuery { limit: 10, ..Default::default() })
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(row.lifecycle, "active");
        assert_eq!(row.author, "QoderWork");
        assert_eq!(row.author_source, "manifest");
        assert_eq!(row.host_support, vec!["qoder".to_string()]);
        assert!(!row.host_support_basis.is_empty(), "宿主支持必须带依据");
    }

    #[test]
    fn asset_summary_serializes_the_new_fields_with_snake_case() {
        // 前端按这些键名读。Tauri invoke 走纯 serde，不做键名转换。
        let r = Registry::open_in_memory().unwrap();
        r.upsert(&sample("s::a", "h1")).unwrap();
        let row = r
            .query_assets(&AssetQuery { limit: 10, ..Default::default() })
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let v = serde_json::to_value(&row).unwrap();
        for k in ["lifecycle", "author", "author_source", "host_support"] {
            assert!(v.get(k).is_some(), "AssetSummary 缺字段 {k}");
        }
        assert!(v.get("authorSource").is_none(), "不得输出 camelCase");
    }

    #[test]
    fn asset_query_include_archived_is_defaultable_from_json() {
        // apps 侧是逐字段字面量构造；HTTP/MCP 侧是反序列化。两条路径都要成立。
        let q: AssetQuery = serde_json::from_str("{\"limit\":10}").unwrap();
        assert!(!q.include_archived, "缺字段时必须是 false（默认过滤归档）");
        let q2 = AssetQuery::default();
        assert!(!q2.include_archived);
    }

    #[test]
    fn rollback_v8_drops_only_the_new_table() {
        let dir = crate::testutil::Scratch::new("v8-rollback");
        let db = dir.path().join("registry.db");
        let r = Registry::open(&db).unwrap();
        r.upsert_all(&[sample("skill::a", "h1"), sample("skill::b", "h2")])
            .unwrap();
        r.set_asset_lifecycle("skill::a", "archived").unwrap();
        {
            let c = r.conn;
            let before = c
                .query_row("SELECT COUNT(*) FROM assets", [], |x| x.get::<_, i64>(0))
                .unwrap();
            rollback_v8(&c).unwrap();
            let names = crate::agents::table_names(&c).unwrap();
            assert!(
                !names.iter().any(|n| n == "agent_hosts"),
                "agent_hosts 还在: {names:?}"
            );
            assert!(names.iter().any(|n| n == "assets"), "assets 被误删");
            assert_eq!(user_version(&c).unwrap(), 7, "版本号应退回 7");
            let after = c
                .query_row("SELECT COUNT(*) FROM assets", [], |x| x.get::<_, i64>(0))
                .unwrap();
            assert_eq!(after, before, "回滚动了既有表的数据");
            // 用户状态（归档）必须还在——回滚只删新表，不删用户数据
            let lc: String = c
                .query_row("SELECT lifecycle FROM assets WHERE id='skill::a'", [], |x| x.get(0))
                .unwrap();
            assert_eq!(lc, "archived", "回滚不能抹掉用户设的生命周期");
        }
    }

    #[test]
    fn migration_v8_preserves_existing_counts() {
        // 迁移铁律：506 条资产 / 495 能力 / 逻辑资产数在任何路径下都不变。
        let dir = crate::testutil::Scratch::new("v8-counts");
        let db = dir.path().join("registry.db");
        let r = Registry::open(&db).unwrap();
        let ms = vec![sample("skill::a", "h1"), sample("skill::b", "h2")];
        r.upsert_all(&ms).unwrap();
        r.sync_logical(&ms, 1000).unwrap();
        let count = |reg: &Registry, sql: &str| -> i64 {
            reg.conn.query_row(sql, [], |x| x.get(0)).unwrap()
        };
        let before = (
            r.total().unwrap(),
            count(&r, "SELECT COUNT(*) FROM capabilities"),
            r.logical_total().unwrap(),
            r.location_total().unwrap(),
        );
        drop(r);
        {
            let conn = Connection::open(&db).unwrap();
            conn.pragma_update(None, "user_version", 7).unwrap();
            migrate(&conn).unwrap();
        }
        let r2 = Registry::open(&db).unwrap();
        let after = (
            r2.total().unwrap(),
            count(&r2, "SELECT COUNT(*) FROM capabilities"),
            r2.logical_total().unwrap(),
            r2.location_total().unwrap(),
        );
        assert_eq!(before, after, "v8 迁移改动了既有表的数据");
    }

    #[test]
    fn dedup_l2_still_reports_real_cross_logical_groups() {
        // 反向：**真的**跨逻辑资产的 L2 候选不能被误杀。
        // 修法若写成"L2 恒不报"，上面那条也会绿，所以必须同时钉这一条。
        let r = registry_with_norm(&[
            ("skill::x", "HX", "vendor/x/SKILL.md", "SAME"),
            ("skill::y", "HY", "vendor/y/SKILL.md", "SAME"),
        ]);
        let cands = r.dedup_candidates().unwrap();
        assert_eq!(cands.len(), 1, "跨逻辑资产的 L2 必须照报");
        assert_eq!(cands[0].match_level, MatchLevel::L2);
        assert_eq!(cands[0].location_ids.len(), 2);
    }
}
