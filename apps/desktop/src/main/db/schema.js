// 能力核心（Capability Core）数据库模式（SQLite 3.5x / node:sqlite）
// 设计要点：
// 1) capabilities 是「逻辑能力」，多个安装实例（versions/installations）归一到同一逻辑能力；
// 2) records 保存每次扫描到的物理实体（含指纹），便于增量比对与故障定位；
// 3) 去重仲裁、维修工单、更新检查、智能体会话、事件流均为独立表，便于审计与回放。

export const SCHEMA_VERSION = 13;

export const SCHEMA = `
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sources (
  id            TEXT PRIMARY KEY,
  kind          TEXT NOT NULL,           -- skill | mcp | plugin | prompt | agent-config | builtin
  host          TEXT NOT NULL,           -- dsh | codex | claude | minimax | gemini | workbuddy | cursor | asset-center | project | custom
  label         TEXT NOT NULL,
  root_path     TEXT NOT NULL,
  format        TEXT NOT NULL DEFAULT 'auto',
  enabled       INTEGER NOT NULL DEFAULT 1,
  builtin       INTEGER NOT NULL DEFAULT 0,
  scan_interval INTEGER NOT NULL DEFAULT 900,
  last_scan_at  INTEGER,
  last_scan_status TEXT NOT NULL DEFAULT 'never',   -- never | ok | warn | error
  last_scan_message TEXT NOT NULL DEFAULT '',
  last_scan_count  INTEGER NOT NULL DEFAULT 0,
  last_scan_ms     INTEGER NOT NULL DEFAULT 0,
  config        TEXT NOT NULL DEFAULT '{}',
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sources_root ON sources(root_path, kind);
CREATE INDEX IF NOT EXISTS idx_sources_host ON sources(host);

CREATE TABLE IF NOT EXISTS capabilities (
  id             TEXT PRIMARY KEY,
  kind           TEXT NOT NULL,          -- skill | mcp | plugin | prompt | agent
  canonical_name TEXT NOT NULL,
  display_name   TEXT NOT NULL,
  summary        TEXT NOT NULL DEFAULT '',
  description    TEXT NOT NULL DEFAULT '',
  author         TEXT NOT NULL DEFAULT '',
  author_source  TEXT NOT NULL DEFAULT 'unknown',
  homepage       TEXT NOT NULL DEFAULT '',
  license        TEXT NOT NULL DEFAULT '',
  tags           TEXT NOT NULL DEFAULT '[]',
  version        TEXT NOT NULL DEFAULT '',
  protocols      TEXT NOT NULL DEFAULT '[]',
  hosts          TEXT NOT NULL DEFAULT '[]',
  identity       TEXT NOT NULL DEFAULT '{}',
  identity_key   TEXT NOT NULL,
  fingerprint    TEXT NOT NULL DEFAULT '',
  body_hash      TEXT NOT NULL DEFAULT '',
  scope          TEXT NOT NULL DEFAULT 'host',     -- own（能力资产库，自有） | host（宿主能力库，外部扫描登记）
  status         TEXT NOT NULL DEFAULT 'active',   -- active | disabled | quarantined | archived
  health         TEXT NOT NULL DEFAULT 'unknown',  -- healthy | warning | broken | untriaged | unknown
  verify_state   TEXT NOT NULL DEFAULT 'unverified', -- verified | unverified | invalid
  verify_message TEXT NOT NULL DEFAULT '',
  repair_state   TEXT NOT NULL DEFAULT 'none',     -- none | diagnosing | dispatching | patching | resolved
  archive_path   TEXT NOT NULL DEFAULT '',
  managed        INTEGER NOT NULL DEFAULT 0,
  pinned         INTEGER NOT NULL DEFAULT 0,
  rating         INTEGER NOT NULL DEFAULT 0,
  usage_count    INTEGER NOT NULL DEFAULT 0,
  last_used_at   INTEGER,
  first_seen     INTEGER NOT NULL,
  last_seen      INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  metadata       TEXT NOT NULL DEFAULT '{}'
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_cap_identity ON capabilities(identity_key);
-- 注意：idx_cap_scope 依赖 migrate() 中的列级迁移（老库无 scope 列），
-- 因此不在本模板内创建，而在 db.js 的 migrate() 确认列存在后创建。
CREATE INDEX IF NOT EXISTS idx_cap_kind ON capabilities(kind, status);
CREATE INDEX IF NOT EXISTS idx_cap_health ON capabilities(health);
CREATE INDEX IF NOT EXISTS idx_cap_name ON capabilities(canonical_name);
CREATE INDEX IF NOT EXISTS idx_cap_updated ON capabilities(updated_at DESC);

CREATE TABLE IF NOT EXISTS installations (
  id             TEXT PRIMARY KEY,
  capability_id  TEXT NOT NULL REFERENCES capabilities(id) ON DELETE CASCADE,
  source_id      TEXT REFERENCES sources(id) ON DELETE SET NULL,
  host           TEXT NOT NULL DEFAULT '',
  origin_kind    TEXT NOT NULL DEFAULT 'discovered', -- discovered | managed | generated | imported | builtin
  path           TEXT NOT NULL,
  entry          TEXT NOT NULL DEFAULT '',
  glob           TEXT NOT NULL DEFAULT '',
  version        TEXT NOT NULL DEFAULT '',
  format         TEXT NOT NULL DEFAULT '',
  fingerprint    TEXT NOT NULL,
  body_hash      TEXT NOT NULL DEFAULT '',
  size           INTEGER NOT NULL DEFAULT 0,
  mtime          INTEGER NOT NULL DEFAULT 0,
  content        TEXT NOT NULL DEFAULT '',
  manifest       TEXT NOT NULL DEFAULT '{}',
  visibility     TEXT NOT NULL DEFAULT 'visible',  -- visible | shadowed | primary
  duplicate_of   TEXT,
  drift          INTEGER NOT NULL DEFAULT 0,
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_inst_cap ON installations(capability_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_inst_path ON installations(path, source_id);
CREATE INDEX IF NOT EXISTS idx_inst_fp ON installations(fingerprint);

CREATE TABLE IF NOT EXISTS search_docs (
  rowid         INTEGER PRIMARY KEY AUTOINCREMENT,
  capability_id TEXT NOT NULL REFERENCES capabilities(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL,
  name          TEXT NOT NULL,
  summary       TEXT NOT NULL DEFAULT '',
  body          TEXT NOT NULL DEFAULT '',
  keywords      TEXT NOT NULL DEFAULT '',
  author        TEXT NOT NULL DEFAULT '',
  hosts         TEXT NOT NULL DEFAULT '',
  updated_at    INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_search_cap ON search_docs(capability_id);

-- FTS 索引列写入的是「已切分文本」（CJK 逐字加空格，见 text-index.js），
-- 因为 SQLite 的 unicode61 不切分中文；切分后中文按字命中，再由应用层做短语近似匹配。
CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(
  name, summary, body, keywords, author, hosts,
  content='search_docs', content_rowid='rowid',
  tokenize='unicode61 remove_diacritics 2'
);

-- trigram 索引用于中英文子串匹配（仅正文，独立维护）。
CREATE VIRTUAL TABLE IF NOT EXISTS search_trigram USING fts5(
  body,
  content='',
  tokenize='trigram'
);

CREATE TRIGGER IF NOT EXISTS trg_search_ai AFTER INSERT ON search_docs BEGIN
  INSERT INTO search_fts(rowid, name, summary, body, keywords, author, hosts)
  VALUES (new.rowid, new.name, new.summary, new.body, new.keywords, new.author, new.hosts);
END;
CREATE TRIGGER IF NOT EXISTS trg_search_ad AFTER DELETE ON search_docs BEGIN
  INSERT INTO search_fts(search_fts, rowid, name, summary, body, keywords, author, hosts)
  VALUES ('delete', old.rowid, old.name, old.summary, old.body, old.keywords, old.author, old.hosts);
END;
CREATE TRIGGER IF NOT EXISTS trg_search_au AFTER UPDATE ON search_docs BEGIN
  INSERT INTO search_fts(search_fts, rowid, name, summary, body, keywords, author, hosts)
  VALUES ('delete', old.rowid, old.name, old.summary, old.body, old.keywords, old.author, old.hosts);
  INSERT INTO search_fts(rowid, name, summary, body, keywords, author, hosts)
  VALUES (new.rowid, new.name, new.summary, new.body, new.keywords, new.author, new.hosts);
END;

CREATE TABLE IF NOT EXISTS duplicate_groups (
  id             TEXT PRIMARY KEY,
  kind           TEXT NOT NULL,
  canonical_name TEXT NOT NULL,
  similarity     REAL NOT NULL DEFAULT 0,
  reason         TEXT NOT NULL DEFAULT '',
  recommended    TEXT NOT NULL DEFAULT '',
  decision       TEXT NOT NULL DEFAULT 'pending',  -- pending | keep_latest | merge | coexist | ignore
  chosen_id      TEXT,
  member_ids     TEXT NOT NULL DEFAULT '[]',
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_dupg_decision ON duplicate_groups(decision);

CREATE TABLE IF NOT EXISTS agents (
  id            TEXT PRIMARY KEY,
  host          TEXT NOT NULL,
  label         TEXT NOT NULL,
  kind          TEXT NOT NULL DEFAULT 'process',  -- process | config | mcp-session
  pid           INTEGER,
  exe_path      TEXT NOT NULL DEFAULT '',
  command_line  TEXT NOT NULL DEFAULT '',
  status        TEXT NOT NULL DEFAULT 'stopped',  -- running | idle | stopped
  protocol      TEXT NOT NULL DEFAULT '',
  endpoint      TEXT NOT NULL DEFAULT '',
  first_seen    INTEGER NOT NULL,
  last_seen     INTEGER NOT NULL,
  last_heartbeat INTEGER,
  metadata      TEXT NOT NULL DEFAULT '{}'
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_host ON agents(host, id);

CREATE TABLE IF NOT EXISTS agent_sessions (
  id            TEXT PRIMARY KEY,
  agent_id      TEXT,
  host          TEXT NOT NULL,
  protocol      TEXT NOT NULL DEFAULT 'mcp',
  client_info   TEXT NOT NULL DEFAULT '{}',
  started_at    INTEGER NOT NULL,
  last_seen_at  INTEGER NOT NULL,
  ended_at      INTEGER,
  requests      INTEGER NOT NULL DEFAULT 0,
  tool_calls    TEXT NOT NULL DEFAULT '{}',
  ip            TEXT NOT NULL DEFAULT '',
  status        TEXT NOT NULL DEFAULT 'open'
);
CREATE INDEX IF NOT EXISTS idx_sessions_started ON agent_sessions(started_at DESC);

CREATE TABLE IF NOT EXISTS heartbeat_files (
  path       TEXT PRIMARY KEY,
  host       TEXT NOT NULL,
  pid        INTEGER,
  payload    TEXT NOT NULL DEFAULT '{}',
  seen_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS faults (
  id             TEXT PRIMARY KEY,
  capability_id  TEXT,
  installation_id TEXT,
  severity       TEXT NOT NULL DEFAULT 'warning',  -- info | warning | error | critical
  code           TEXT NOT NULL,
  title          TEXT NOT NULL,
  detail         TEXT NOT NULL DEFAULT '',
  evidence       TEXT NOT NULL DEFAULT '{}',
  status         TEXT NOT NULL DEFAULT 'open',     -- open | triaged | dispatched | fixing | resolved | ignored
  auto_fixable   INTEGER NOT NULL DEFAULT 0,
  fix_kind       TEXT NOT NULL DEFAULT '',
  dispatch_agent TEXT,
  dispatch_note  TEXT NOT NULL DEFAULT '',
  repair_log     TEXT NOT NULL DEFAULT '[]',
  detected_at    INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  resolved_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_fault_status ON faults(status, severity);
CREATE INDEX IF NOT EXISTS idx_fault_cap ON faults(capability_id);

CREATE TABLE IF NOT EXISTS repairs (
  id            TEXT PRIMARY KEY,
  fault_id      TEXT NOT NULL REFERENCES faults(id) ON DELETE CASCADE,
  capability_id TEXT,
  actor         TEXT NOT NULL DEFAULT 'system',  -- system | agent | user
  agent_id      TEXT,
  action        TEXT NOT NULL,
  patch         TEXT NOT NULL DEFAULT '{}',
  before_hash   TEXT NOT NULL DEFAULT '',
  after_hash    TEXT NOT NULL DEFAULT '',
  result        TEXT NOT NULL DEFAULT 'pending',  -- pending | applied | failed | reverted | verified
  message       TEXT NOT NULL DEFAULT '',
  created_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repair_fault ON repairs(fault_id);

CREATE TABLE IF NOT EXISTS backup_snapshots (
  id            TEXT PRIMARY KEY,
  capability_id TEXT,
  kind          TEXT NOT NULL DEFAULT 'file',   -- file | dir | db | package
  path          TEXT NOT NULL,
  store_path    TEXT NOT NULL,
  hash          TEXT NOT NULL,
  size          INTEGER NOT NULL DEFAULT 0,
  reason        TEXT NOT NULL DEFAULT '',
  created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS updates (
  id             TEXT PRIMARY KEY,
  capability_id  TEXT NOT NULL REFERENCES capabilities(id) ON DELETE CASCADE,
  source_kind    TEXT NOT NULL DEFAULT 'unknown',  -- registry | git | npm | local | unknown
  current_version TEXT NOT NULL DEFAULT '',
  latest_version  TEXT NOT NULL DEFAULT '',
  status         TEXT NOT NULL DEFAULT 'unknown',  -- unknown | current | outdated | ahead | check-failed
  detail         TEXT NOT NULL DEFAULT '',
  checked_at     INTEGER NOT NULL,
  applied_at     INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_update_cap ON updates(capability_id);
CREATE INDEX IF NOT EXISTS idx_update_status ON updates(status);

CREATE TABLE IF NOT EXISTS workflows (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  kind        TEXT NOT NULL DEFAULT 'seed',      -- seed | user | generated
  description TEXT NOT NULL DEFAULT '',
  steps       TEXT NOT NULL DEFAULT '[]',
  schedule    TEXT NOT NULL DEFAULT '',
  enabled     INTEGER NOT NULL DEFAULT 1,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS workflow_runs (
  id          TEXT PRIMARY KEY,
  workflow_id TEXT NOT NULL,
  status      TEXT NOT NULL DEFAULT 'running',  -- running | completed | failed | cancelled
  trigger     TEXT NOT NULL DEFAULT 'manual',
  started_at  INTEGER NOT NULL,
  ended_at    INTEGER,
  steps       TEXT NOT NULL DEFAULT '[]',
  summary     TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_wfruns_started ON workflow_runs(started_at DESC);

CREATE TABLE IF NOT EXISTS events (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  ts            INTEGER NOT NULL,
  level         TEXT NOT NULL DEFAULT 'info',   -- debug | info | warn | error
  topic         TEXT NOT NULL,
  message       TEXT NOT NULL,
  capability_id TEXT,
  agent_id      TEXT,
  data          TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts DESC);
CREATE INDEX IF NOT EXISTS idx_events_topic ON events(topic, ts DESC);

CREATE TABLE IF NOT EXISTS settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- 上游能力市场目录（DSH Market / npm 等），用于「安装 / 更新 / 探索」；
-- 与 capabilities 分离：目录是「可获取的」，能力库是「已纳管的」。
CREATE TABLE IF NOT EXISTS registry_catalog (
  id            TEXT PRIMARY KEY,
  registry      TEXT NOT NULL DEFAULT 'dsh-market',
  package_name  TEXT NOT NULL,
  version       TEXT NOT NULL DEFAULT '',
  engines_dsh   TEXT NOT NULL DEFAULT '',
  peer_count    INTEGER NOT NULL DEFAULT 0,
  checked_at    INTEGER NOT NULL,
  installed     INTEGER NOT NULL DEFAULT 0,
  capability_id TEXT,
  metadata      TEXT NOT NULL DEFAULT '{}'
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_catalog_pkg ON registry_catalog(registry, package_name);
CREATE INDEX IF NOT EXISTS idx_catalog_installed ON registry_catalog(installed, registry);
CREATE INDEX IF NOT EXISTS idx_catalog_name ON registry_catalog(package_name);

CREATE TABLE IF NOT EXISTS capability_usage (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  capability_id TEXT NOT NULL,
  agent_id      TEXT,
  session_id    TEXT,
  action        TEXT NOT NULL,
  detail        TEXT NOT NULL DEFAULT '{}',
  ts            INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_usage_cap ON capability_usage(capability_id, ts DESC);
-- 模型接入：提供商 / 角色路由 / 资产向量 / 调用审计（基线 §8）
-- 密钥仅存本地 SQLite；任何对外返回一律打码（见 services/models.js 的 maskKey）。
CREATE TABLE IF NOT EXISTS model_providers (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'openai-compatible',
  base_url TEXT NOT NULL,
  api_key TEXT NOT NULL DEFAULT '',
  enabled INTEGER NOT NULL DEFAULT 1,
  is_default INTEGER NOT NULL DEFAULT 0,
  extra_headers TEXT NOT NULL DEFAULT '{}',
  config TEXT NOT NULL DEFAULT '{}',
  last_check_at INTEGER,
  last_check_status TEXT NOT NULL DEFAULT 'never',
  last_check_message TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS model_routes (
  role TEXT PRIMARY KEY,
  provider_id TEXT NOT NULL,
  model TEXT NOT NULL,
  dim INTEGER,
  params TEXT NOT NULL DEFAULT '{}',
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS asset_vectors (
  capability_id TEXT PRIMARY KEY,
  provider_id TEXT NOT NULL,
  model TEXT NOT NULL,
  dim INTEGER NOT NULL,
  vector BLOB NOT NULL,
  source_hash TEXT NOT NULL DEFAULT '',
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS model_calls (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,
  provider_id TEXT,
  role TEXT,
  model TEXT,
  action TEXT NOT NULL,
  latency_ms INTEGER NOT NULL DEFAULT 0,
  ok INTEGER NOT NULL DEFAULT 1,
  tokens INTEGER NOT NULL DEFAULT 0,
  error TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_model_calls_ts ON model_calls(ts DESC);
`;

