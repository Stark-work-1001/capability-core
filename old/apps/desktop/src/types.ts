/** 内核握手信息，由 Rust 侧 kernel_info 命令返回 */
export interface KernelInfo {
  app_version: string;
  schema_version: string;
  rustc_version: string;
  os: string;
  arch: string;
  /**
   * 版本标识：`devel`（开发测试版）/ `standalone`（免安装运行版）。
   *
   * **可选**：后端本轮才接入这个字段。取不到就不渲染徽标 ——
   * 界面上宁可少一个标签，也不能猜一个版本写上去。
   */
  edition?: string | null;
  review_candidate?: boolean;
  /**
   * 内核内置 HTTP 服务的端口（devel 18197 / standalone 18198）。
   *
   * 同样可选：老版本内核没有这个字段，界面上就不显示端口行。
   */
  http_port?: number | null;
  /**
   * 本次运行 HTTP 服务是否真的起来了。
   *
   * false = 端口被占用等原因没绑上。界面**不能假装在线** ——
   * 显示端口时一并标出「未监听」。
   */
  http_listening?: boolean | null;
}

/** 库内现状，由 registry_stats / open_registry 返回 */
export interface RegistryStats {
  db_path: string;
  scan_root: string;
  total: number;
  counts: KindCount[];
  ready: boolean;
}

export interface KindCount {
  kind: string;
  count: number;
}

/** 入库结果，由 rescan 返回 */
export interface IngestSummary {
  parsed: number;
  inserted: number;
  updated: number;
  unchanged: number;
  removed: number;
  skipped: number;
  has_changes: boolean;
  has_cycle: boolean;
  elapsed_ms: number;
  /** M1.5：本次新建的逻辑资产数 */
  logical_created: number;
  /** M1.5：全库 alias 位置数（内容重复的位置） */
  alias_count: number;
  /** M1.5：待用户确认的去重候选组数 */
  dedup_pending: number;
}

/** M1.5：库内一个标签及其资产数 */
export interface TagCount {
  tag: string;
  count: number;
}

/** M1.5：指纹命中级别 */
export type MatchLevel = "l1" | "l2";

/**
 * M1.5：待用户确认的去重候选组。
 *
 * **程序绝不自动合并。** 这一组只有在用户显式选择
 * 合并 / 共存 / 忽略 之后才会改变。
 */
export interface DedupCandidate {
  group_key: string;
  match_level: MatchLevel;
  suggested_canonical: string;
  location_ids: string[];
}

/** M1.5：用户对一组候选的裁决 */
export type DedupDecision = "merge" | "keep_both" | "ignore";

/** M1.5：已裁决记录（审计用） */
export interface DedupRecord {
  group_key: string;
  match_level: string;
  decision: string;
  decided_at: number;
  location_ids: string[];
}

/** 资产列表行，与 Rust 侧 AssetSummary 一一对应 */
export interface AssetSummary {
  id: string;
  kind: string;
  name: string;
  version: string;
  description: string;
  authority: string;
  authority_label: string;
  tool_posture: string;
  side_effect: string;
  role: string | null;
  capability_count: number;
  source_path: string | null;
  /** M1.5：所属逻辑资产（内容相同即同一份） */
  logical_id?: string | null;
  /** M1.5：同一逻辑资产下的磁盘位置数（>1 说明内容重复） */
  location_count?: number;
  /** M1.5：本位置的角色 canonical / alias */
  location_role?: string | null;
  /**
   * 来源：vendor（厂商随包）/ self_built（中枢自建）/ nurtured（本机自长）。
   * 由内核 origin.rs 判定并落库（schema v7 起），判不出来是空串。
   */
  origin_kind?: string | null;
  /**
   * 注册方式：embedded（自包含）/ mounted（挂载包外路径）/ remote（远程端点）。
   * 与 origin_kind 是两个正交维度，不要合并。
   */
  registration_kind?: string | null;
  /** 归属宿主智能体（如 qoder / claude）；判不出来是空串，不是 "unknown" */
  origin_agent?: string | null;
  /** 删掉包外的东西它还能不能跑。mounted/remote 一定是 false */
  self_contained?: boolean | null;
  /**
   * 作者。可从 manifest.author / SKILL.md frontmatter / 包目录推断出来；
   * **判不出来是空串或 null，界面就不显示这一行**，不写 "unknown"。
   */
  author?: string | null;
  /**
   * 作者信息的**出处**：`manifest` / `frontmatter` / `package` / 空串。
   *
   * 用户原话要求「建立相应依赖来表明这些能力的作者」——
   * 作者不能只是目录里推出来的一个字符串，界面必须能说清这个值是
   * 作者自己声明的、还是从 frontmatter 读的、还是从包路径反推的。
   * 三者的可信度不同，显示时必须分开标注。
   *
   * 空串 / null = 后端本轮还没落这个字段，或判不出来 —— 显示时降级为不标出处。
   */
  author_source?: string | null;
  /**
   * 这份资产被哪些宿主智能体支持（qoder / claude / codex …）。
   *
   * 空数组 / null = 未判出归属宿主。**那不是错误**，界面照实说明。
   */
  host_support?: string[] | null;
  /**
   * 生命周期：active / paused / archived。
   * 软状态，绝不物理删除（项目铁律）。null = 后端本轮还没落这个字段。
   */
  lifecycle?: string | null;
}

export interface ListQuery {
  kind?: string | null;
  text?: string | null;
  min_authority?: string | null;
  /** M1.5：按标签过滤（任一能力带此标签即命中） */
  tag?: string | null;
  limit?: number;
  offset?: number;
}

export interface ListResult {
  total: number;
  items: AssetSummary[];
}

/**
 * 完整 manifest，由 get_asset 返回。
 *
 * **所有非标量字段都声明成可选**，这不是偷懒，是内核的既有契约：
 * Rust 侧对空集合 / 空映射 / None 一律用
 * `skip_serializing_if` **省略整个键**（见 ac-core/src/manifest.rs、
 * identity.rs、relation.rs、dedup.rs）。库里 506 条资产的实测频率：
 *
 * | 键 | 出现的行数 | 缺失 |
 * |---|---|---|
 * | `provides` | 499 | 7 |
 * | `relations` | 18 | 488 |
 * | `identity.role` | 216 | 290 |
 *
 * 所以"类型写必填"只是骗 TS，运行时照样 undefined。
 * 渲染前一律走 {@link normalizeManifest} 补齐，不要直接访问本接口的字段。
 */
export interface CapabilityManifest {
  schema_version?: string;
  id?: string;
  kind?: string;
  name?: string;
  version?: string;
  description?: string;
  identity?: {
    role?: string | null;
    authority?: string;
    side_effect?: string;
    tool_posture?: string;
    idempotent?: boolean;
    open_world?: boolean;
    dimensions?: Record<string, string>;
  };
  provides?: {
    capability_id?: string;
    name?: string;
    summary?: string;
    tags?: string[];
  }[];
  relations?: { src_id?: string; kind?: string; target_id?: string }[];
  provenance?: {
    source_path?: string | null;
    imported_at?: string | null;
    content_hash?: string | null;
    source_format?: string | null;
  };
}

/**
 * 补齐后的 manifest：字段全部存在，**可以直接访问**。
 *
 * 由 {@link normalizeManifest} 产出，是渲染层唯一应该依赖的形状。
 */
export interface NormalizedManifest {
  schema_version: string;
  id: string;
  kind: string;
  name: string;
  version: string;
  description: string;
  identity: {
    role: string | null;
    authority: string;
    side_effect: string;
    tool_posture: string;
    idempotent: boolean;
    open_world: boolean;
    dimensions: Record<string, string>;
  };
  provides: {
    capability_id: string;
    name: string;
    summary: string;
    tags: string[];
  }[];
  relations: { src_id: string; kind: string; target_id: string }[];
  provenance: {
    source_path: string | null;
    imported_at: string | null;
    content_hash: string | null;
    source_format: string | null;
  };
}

function asRecord(v: unknown): Record<string, unknown> {
  return v && typeof v === "object" && !Array.isArray(v)
    ? (v as Record<string, unknown>)
    : {};
}

function asString(v: unknown, fallback = ""): string {
  return typeof v === "string" ? v : fallback;
}

function asArray(v: unknown): unknown[] {
  return Array.isArray(v) ? v : [];
}

function asNullableString(v: unknown): string | null {
  return typeof v === "string" && v.length > 0 ? v : null;
}

/**
 * 把任意来源的 manifest 归一化成字段齐全的形状。
 *
 * 这是 BUG 1（点击详情黑屏）的**单一修复点**：渲染层从此不再对
 * 可选字段做无防护访问，也不用在每个 JSX 表达式上散落 `?? []`。
 *
 * 对畸形输入也安全：null / undefined / 非对象 / 数组里塞了非对象，
 * 全部降级为默认值，绝不抛 TypeError。
 */
export function normalizeManifest(
  m: CapabilityManifest | null | undefined,
): NormalizedManifest {
  const r = asRecord(m);
  const identity = asRecord(r.identity);
  const provenance = asRecord(r.provenance);

  const dimensions: Record<string, string> = {};
  for (const [k, v] of Object.entries(asRecord(identity.dimensions))) {
    dimensions[k] = typeof v === "string" ? v : String(v);
  }

  return {
    schema_version: asString(r.schema_version),
    id: asString(r.id, "（未知 ID）"),
    kind: asString(r.kind, "unknown"),
    name: asString(r.name, "（未命名资产）"),
    version: asString(r.version),
    description: asString(r.description),
    identity: {
      role: asNullableString(identity.role),
      authority: asString(identity.authority, "unknown"),
      side_effect: asString(identity.side_effect, "unknown"),
      tool_posture: asString(identity.tool_posture, "unknown"),
      idempotent: identity.idempotent === true,
      open_world: identity.open_world === true,
      dimensions,
    },
    // 非对象条目直接丢弃，而不是经 asRecord 变成空对象：
    // 那样会凭空造出 capability_id 为空串的「幽灵能力」，
    // 详情面板会渲染出空白行——比不渲染更糟，因为它看起来像数据。
    provides: asArray(r.provides)
      .filter((p) => p !== null && typeof p === "object" && !Array.isArray(p))
      .map((p) => {
        const c = asRecord(p);
        const cid = asString(c.capability_id);
        return {
          capability_id: cid,
          name: asString(c.name, cid || "（未命名能力）"),
          summary: asString(c.summary),
          tags: asArray(c.tags).filter((t): t is string => typeof t === "string"),
        };
      }),
    relations: asArray(r.relations)
      .filter((x) => x !== null && typeof x === "object" && !Array.isArray(x))
      .map((x) => {
        const q = asRecord(x);
        return {
          src_id: asString(q.src_id),
          kind: asString(q.kind, "related"),
          target_id: asString(q.target_id),
        };
      }),
    provenance: {
      source_path: asNullableString(provenance.source_path),
      imported_at: asNullableString(provenance.imported_at),
      content_hash: asNullableString(provenance.content_hash),
      source_format: asNullableString(provenance.source_format),
    },
  };
}

/** 六类资产的展示元数据 */
export const ASSET_KINDS = [
  { key: "skill", label: "Skill", color: "#38bdf8" },
  { key: "mcp", label: "MCP", color: "#a78bfa" },
  { key: "plugin", label: "Plugin", color: "#34d399" },
  { key: "suite", label: "Suite", color: "#fbbf24" },
  { key: "ghost", label: "Ghost", color: "#94a3b8" },
  { key: "organization", label: "Org", color: "#f472b6" },
] as const;

export type AssetKindKey = (typeof ASSET_KINDS)[number]["key"];

export function kindColor(kind: string): string {
  return ASSET_KINDS.find((k) => k.key === kind)?.color ?? "#64748b";
}

export function kindLabel(kind: string): string {
  return ASSET_KINDS.find((k) => k.key === kind)?.label ?? kind;
}

/**
 * 资产生命周期三态。
 *
 * **软删除**：archived 只是不再被推荐，行还在库里，随时可恢复。
 * 这是项目铁律（绝不物理删除）在界面上的对应物 —— 所以界面上
 * 没有「删除」按钮，只有这三挡。
 */
export const ASSET_LIFECYCLES = [
  {
    key: "active" as const,
    label: "激活",
    short: "使用中",
    color: "#34d399",
    hint: "默认状态：正常参与检索与分派",
  },
  {
    key: "paused" as const,
    label: "暂停",
    short: "已暂停",
    color: "#fbbf24",
    hint: "暂时不参与分派，但仍留在库内、随时可恢复",
  },
  {
    key: "archived" as const,
    label: "归档",
    short: "已归档",
    color: "#94a3b8",
    hint: "软隐藏：行仍在库里（绝不物理删除），只是不再推荐",
  },
] as const;

export type AssetLifecycleKey = (typeof ASSET_LIFECYCLES)[number]["key"];

export function lifecycleMeta(v: string | null | undefined) {
  return ASSET_LIFECYCLES.find((l) => l.key === v) ?? null;
}

/**
 * 作者信息的出处 → 界面标签。
 *
 * 用户要求「建立相应依赖来表明这些能力的作者」：作者值本身只是个字符串，
 * **它从哪来决定了它有多可信**。所以这里把三档分开标注，而不是统一写成「作者」。
 *
 * 顺序即可信度：作者自己声明的 > frontmatter 里写的 > 从包路径反推的。
 */
export const AUTHOR_SOURCES = [
  {
    key: "manifest",
    label: "自声明",
    /** 属性栏里的完整写法 */
    full: "作者（自声明）",
    hint: "来自资产 manifest 的 author 字段：作者自己声明的，最可信",
  },
  {
    key: "frontmatter",
    label: "frontmatter",
    full: "作者（frontmatter）",
    hint: "来自 SKILL.md frontmatter 的 author 字段",
  },
  {
    key: "package",
    label: "包推断",
    full: "作者（包推断）",
    hint: "从包路径反推（如 vendor/plugin/obsidian-suite/ → obsidian-suite 包），可信度最低",
  },
] as const;

export type AuthorSourceKey = (typeof AUTHOR_SOURCES)[number]["key"];

export function authorSourceMeta(v: string | null | undefined) {
  if (!v) return null;
  return AUTHOR_SOURCES.find((s) => s.key === v) ?? null;
}

/**
 * 作者行的完整展示文本。
 *
 * 有 author 但**没有** author_source（后端本轮才落这个字段）时，
 * 退回旧行为：只写「作者」，不编一个出处。
 * 这就是降级，而不是假装知道来源。
 */
export function authorLabel(
  author: string | null | undefined,
  source: string | null | undefined,
): string | null {
  const a = (author ?? "").trim();
  if (a === "") return null;
  const meta = authorSourceMeta(source);
  return meta ? meta.full : "作者";
}

/**
 * 来源维度（origin_kind）的展示元数据。
 *
 * 三档的语义不同，配色也不同：vendor 是别人给的（中性），self_built 是
 * 中枢自己注册的（强调色），nurtured 是本机自己长出来的（青绿）。
 * 判不出来（空串）**没有条目** —— 界面上就不打标签，不写 unknown。
 */
export const ORIGIN_META = [
  {
    key: "vendor",
    label: "厂商",
    filterLabel: "厂商随包",
    cls: "origin-vendor",
    hint: "随厂商包一起发过来的资产",
  },
  {
    key: "self_built",
    label: "自建",
    filterLabel: "中枢自建",
    cls: "origin-self",
    hint: "中枢自己注册/生成的资产",
  },
  {
    key: "nurtured",
    label: "自长",
    filterLabel: "本机自长",
    cls: "origin-nurtured",
    hint: "本机在使用中自己长出来的资产",
  },
] as const;

export function originMeta(v: string | null | undefined) {
  if (!v) return null;
  return ORIGIN_META.find((o) => o.key === v) ?? null;
}

/**
 * 注册方式维度（registration_kind）的展示元数据。
 *
 * 与 {@link ORIGIN_META} 正交：来源说「谁给的」，注册方式说「怎么挂进来的」。
 */
export const REGISTRATION_META = [
  {
    key: "embedded",
    label: "自包含",
    filterLabel: "自包含",
    hint: "所有依赖都在包内，删掉外部依赖也能跑",
  },
  {
    key: "mounted",
    label: "挂载",
    filterLabel: "挂载",
    hint: "引用包外路径，外部文件被删就跑不起来",
  },
  {
    key: "remote",
    label: "远程",
    filterLabel: "远程",
    hint: "指向远程端点，本机没有实体文件",
  },
] as const;

export function registrationMeta(v: string | null | undefined) {
  if (!v) return null;
  return REGISTRATION_META.find((r) => r.key === v) ?? null;
}

/** 权限等级的中文说明与配色 */
export const AUTHORITY_META: Record<string, { label: string; color: string }> = {
  read: { label: "只读", color: "#64748b" },
  observe: { label: "观察", color: "#38bdf8" },
  write: { label: "可写", color: "#fbbf24" },
  destructive: { label: "破坏性", color: "#f87171" },
};
