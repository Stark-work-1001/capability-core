// 前端 <-> Rust 内核的调用桥。
//
// 开发态支持：在浏览器中直接打开 Vite 时并不存在 Tauri host，
// 此时 invoke 会抛错或永久挂起。这里加超时降级，让 UI 在任何情况下
// 都能离开 loading 态，并且不必每次重编 Rust 就能迭代界面。
//
// 注意：浏览器预览路径**不使用任何计时器**。后台标签页会冻结
// setTimeout，用它做模拟延迟会让界面永久卡在 loading（P08 踩坑）。

import { invoke } from "@tauri-apps/api/core";
import type {
  AssetSummary,
  CapabilityManifest,
  DedupCandidate,
  DedupDecision,
  DedupRecord,
  IngestSummary,
  KernelInfo,
  ListQuery,
  ListResult,
  RegistryStats,
  TagCount,
} from "./types";
import type {
  AgentHost,
  TaskCatalog,
  AgentRecord,
  DiagnosticReport,
  HeartbeatScanReport,
  LlmProviderReport,
  ProtocolMatrix,
  StaticRegistrationReport,
  UsageRow,
  ModelSlotConfig,
  ModelSlotProbeResult,
  ModelSlotView,
} from "./panel-types";

export const isTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export type Runtime = "tauri" | "browser-preview";

/** 握手超时。超过则判定为无内核环境。 */
const HANDSHAKE_TIMEOUT_MS = 2500;
/** 扫描 508 个资产要十几秒，超时必须给得比握手宽松。 */
const INGEST_TIMEOUT_MS = 120_000;

const MOCK: KernelInfo = {
  app_version: "0.1.0",
  schema_version: "1.0",
  rustc_version: "rustc 1.99.1 (58cf8f9c3 2026-10-13)",
  os: "browser-preview",
  arch: "x86_64",
};

/** 预览模式的假数据，让界面在没有内核时也能看出布局是否合理。 */
const MOCK_STATS: RegistryStats = {
  db_path: "（预览模式，无数据库）",
  scan_root: "（预览模式）",
  total: 0,
  counts: [],
  ready: false,
};

interface PreviewItem extends AssetSummary {
  manifest: CapabilityManifest;
}

interface PreviewSnapshot {
  generated_at_unix: number;
  db_path: string;
  scan_root: string;
  total: number;
  counts: RegistryStats["counts"];
  items: PreviewItem[];
  /** M1.5：预览模式没有 IPC，去重候选随快照一起带过来 */
  dedup_candidates?: DedupCandidate[];
  logical_total?: number;
  alias_count?: number;
}

let previewCache: PreviewSnapshot | null = null;

/**
 * 浏览器预览模式读真实数据快照。
 *
 * `ingest_vendor` 例程会把库里的真实数据导出成
 * `apps/desktop/public/preview.json`，这里直接 fetch 它。
 * 这样不启 Tauri 也能在浏览器里按真实数据调界面。
 *
 * 用 fetch 而不是 setTimeout 做模拟延迟——后台标签页会冻结计时器。
 */
async function loadPreview(): Promise<PreviewSnapshot> {
  if (previewCache) return previewCache;
  try {
    const r = await fetch("preview.json", { cache: "no-store" });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    previewCache = (await r.json()) as PreviewSnapshot;
  } catch {
    previewCache = {
      generated_at_unix: 0,
      db_path: MOCK_STATS.db_path,
      scan_root: MOCK_STATS.scan_root,
      total: 0,
      counts: [],
      items: [],
      dedup_candidates: [],
      logical_total: 0,
      alias_count: 0,
    };
  }
  return previewCache;
}

function withTimeout<T>(p: Promise<T>, ms: number, label: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`${label} 超时 ${ms}ms`)), ms);
    p.then(
      (v) => {
        clearTimeout(t);
        resolve(v);
      },
      (e) => {
        clearTimeout(t);
        reject(e);
      },
    );
  });
}

/**
 * 获取内核信息。
 * - Tauri 环境下走真实 IPC
 * - 浏览器环境下返回 mock，**并标明是预览数据**
 * - 任何异常/挂起都不会让 UI 卡在 loading
 */
export async function kernelInfo(): Promise<KernelInfo> {
  if (!isTauri) {
    return MOCK;
  }
  try {
    return await withTimeout(
      invoke<KernelInfo>("kernel_info"),
      HANDSHAKE_TIMEOUT_MS,
      "kernel_info",
    );
  } catch (e) {
    // IPC 不可用（host 异常或版本不匹配）时退回预览态，
    // 但把 os 字段改掉，让用户一眼看出这不是真实内核数据。
    return { ...MOCK, os: `ipc-failed: ${String(e).slice(0, 60)}` };
  }
}

/** 打开数据库并扫描入库。返回入库摘要与最新统计。 */
export async function rescan(
  scanRoot?: string,
): Promise<{ summary: IngestSummary; stats: RegistryStats }> {
  if (!isTauri) {
    const p = await loadPreview();
    return {
      summary: {
        parsed: p.total,
        inserted: 0,
        updated: 0,
        unchanged: 0,
        removed: 0,
        skipped: 0,
        has_changes: false,
        has_cycle: false,
        elapsed_ms: 0,
        logical_created: 0,
        alias_count: 0,
        dedup_pending: 0,
      },
      stats: {
        db_path: p.db_path,
        scan_root: p.scan_root,
        total: p.total,
        counts: p.counts,
        ready: p.total > 0,
      },
    };
  }
  const res = await withTimeout(
    invoke<[IngestSummary, RegistryStats]>("rescan", { scanRoot: scanRoot ?? null }),
    INGEST_TIMEOUT_MS,
    "rescan",
  );
  return { summary: res[0], stats: res[1] };
}

/**
 * 打开注册表。**这是启动的第一件事**，必须先于 rescan / listAssets / stats。
 *
 * 之前前端漏了这一步：App 启动只调 kernelInfo + rescan，
 * 而后端的 `open_registry` 是一个显式命令——引擎在 setup 里不自动打开。
 * 结果是每次启动都显示「数据库未打开」，而库本身完全正常
 * （integrity_check ok、506 条资产俱在）。
 *
 * 幂等：库已经打开时后端直接返回当前统计，重复调用安全。
 */
export async function openRegistry(): Promise<RegistryStats> {
  if (!isTauri) {
    throw new Error("预览模式：无内核，不能打开注册表");
  }
  return withTimeout(
    invoke<RegistryStats>("open_registry"),
    HANDSHAKE_TIMEOUT_MS * 6,
    "open_registry",
  );
}

export async function registryStats(): Promise<RegistryStats> {
  if (!isTauri) {
    const p = await loadPreview();
    return {
      db_path: p.db_path,
      scan_root: p.scan_root,
      total: p.total,
      counts: p.counts,
      ready: p.total > 0,
    };
  }
  return invoke<RegistryStats>("registry_stats");
}

export async function listAssets(query: ListQuery): Promise<ListResult> {
  if (!isTauri) {
    const p = await loadPreview();
    let items = p.items;
    if (query.kind) items = items.filter((i) => i.kind === query.kind);
    if (query.min_authority) {
      const order = ["read", "observe", "write", "destructive"];
      const min = order.indexOf(query.min_authority);
      if (min >= 0) {
        items = items.filter((i) => order.indexOf(i.authority) >= min);
      }
    }
    if (query.tag) {
      // 预览模式按 manifest.provides[].tags 过滤，
      // 与 SQLite 的 json_each(tags) 语义保持一致（任一命中即返回）。
      //
      // 注意 `provides` 可能整个键都不存在：Rust 侧对空数组用了
      // `skip_serializing_if`，506 条里有 7 条是零能力资产。
      // 直接 `.provides.some()` 会在这些行上抛 TypeError。
      const want = query.tag;
      items = items.filter((i) =>
        (i.manifest?.provides ?? []).some((p) =>
          (p.tags ?? []).includes(want),
        ),
      );
    }
    if (query.text) {
      const q = query.text.toLowerCase();
      items = items.filter((i) =>
        `${i.id} ${i.name} ${i.description} ${i.role ?? ""}`
          .toLowerCase()
          .includes(q),
      );
    }
    const offset = query.offset ?? 0;
    const limit = query.limit ?? 200;
    const page = items.slice(offset, offset + limit);
    // 列表不需要 manifest，剥掉省内存
    return {
      total: items.length,
      items: page.map(({ manifest: _m, ...summary }) => summary),
    };
  }
  return withTimeout(
    invoke<ListResult>("list_assets", { query }),
    HANDSHAKE_TIMEOUT_MS * 4,
    "list_assets",
  );
}

export async function getAsset(id: string): Promise<CapabilityManifest | null> {
  if (!isTauri) {
    const p = await loadPreview();
    return p.items.find((i) => i.id === id)?.manifest ?? null;
  }
  return invoke<CapabilityManifest | null>("get_asset", { id });
}

// ── M1.5：标签与去重 ─────────────────────────────────────────

/** 库内标签清单，供筛选控件渲染。 */
export async function listTags(): Promise<TagCount[]> {
  if (!isTauri) {
    const p = await loadPreview();
    const m = new Map<string, number>();
    for (const it of p.items) {
      const seen = new Set<string>();
      // provides 可能是 undefined（零能力资产，Rust 侧跳过了空数组）
      for (const cap of it.manifest?.provides ?? []) {
        for (const t of cap.tags ?? []) {
          if (!seen.has(t)) {
            seen.add(t);
            m.set(t, (m.get(t) ?? 0) + 1);
          }
        }
      }
    }
    return [...m.entries()]
      .map(([tag, count]) => ({ tag, count }))
      .sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag));
  }
  return invoke<TagCount[]>("list_tags");
}

/**
 * 待用户确认的去重候选。
 *
 * 纯只读。**这里不会、也不允许合并任何东西**——
 * 合并必须由用户点出来，走 {@link decideDedup}。
 */
export async function listDedupCandidates(): Promise<DedupCandidate[]> {
  if (!isTauri) {
    // 预览模式读快照里的候选。不带的话面板会显示"没有待确认的重复项"，
    // 而那只是因为拿不到数据——会让人误以为去重没生效。
    const p = await loadPreview();
    return p.dedup_candidates ?? [];
  }
  return withTimeout(
    invoke<DedupCandidate[]>("list_dedup_candidates"),
    HANDSHAKE_TIMEOUT_MS * 4,
    "list_dedup_candidates",
  );
}

/**
 * 用户裁决一组去重候选。
 *
 * merge     合并：丢弃别名，保留 canonical
 * keep_both 共存：全部升级为独立逻辑资产（多版本/多副本各自保留）
 * ignore    忽略：维持别名角色，下次扫描再问
 */
export async function decideDedup(
  groupKey: string,
  matchLevel: "l1" | "l2",
  decision: DedupDecision,
): Promise<boolean> {
  if (!isTauri) return false;
  return withTimeout(
    invoke<boolean>("decide_dedup", {
      groupKey,
      matchLevel,
      decision,
    }),
    HANDSHAKE_TIMEOUT_MS * 4,
    "decide_dedup",
  );
}

/** 已裁决历史（审计）。 */
export async function dedupHistory(): Promise<DedupRecord[]> {
  if (!isTauri) return [];
  return invoke<DedupRecord[]>("dedup_history");
}


// ═══════════════════════════════════════════════════════════════
// M3：总览 / 维修站 / 模型接入
// ═══════════════════════════════════════════════════════════════

//
// **浏览器预览一律返回空数据，不造假数字。**
// 这些面板的数字全部来自内核查询；预览模式没有内核，拿不到就是拿不到，
// 界面据此显示「暂无数据 + 原因」。编一组占位数字挂上去，
// 会让人以为"已经有智能体接入了"，那比空着更糟。

/** 心跳目录约定位置（仅用于预览态的说明文案） */
const HEARTBEAT_DIR = "data/heartbeat/";
const LLM_CONFIG = "data/llm-providers.json";

/** 全部智能体 + 心跳 + 用量摘要 */
export async function listAgents(): Promise<AgentRecord[]> {
  if (!isTauri) return [];
  return withTimeout(
    invoke<AgentRecord[]>("list_agents"),
    HANDSHAKE_TIMEOUT_MS * 4,
    "list_agents",
  );
}

/** C 通道：静态登记（幂等）。用户点「静态登记」时才调，不自动跑。 */
export async function registerStaticAgents(): Promise<StaticRegistrationReport> {
  if (!isTauri) {
    return {
      ecosystems: 0,
      capability_rows: 0,
      assets: 0,
      unassigned_assets: 0,
      per_ecosystem: [],
    };
  }
  return invoke<StaticRegistrationReport>("register_static_agents");
}

/** B 通道：扫描共享心跳目录。 */
export async function scanHeartbeats(): Promise<HeartbeatScanReport> {
  if (!isTauri) {
    return {
      dir: HEARTBEAT_DIR,
      dir_exists: false,
      files_seen: 0,
      hosts_registered: 0,
      rejected: [],
    };
  }
  return withTimeout(
    invoke<HeartbeatScanReport>("scan_heartbeats"),
    HANDSHAKE_TIMEOUT_MS * 4,
    "scan_heartbeats",
  );
}

/** A 通道：谁拉了什么能力 */
export async function capabilityUsage(
  limit?: number,
): Promise<UsageRow[]> {
  if (!isTauri) return [];
  return withTimeout(
    invoke<UsageRow[]>("capability_usage", { limit: limit ?? null }),
    HANDSHAKE_TIMEOUT_MS * 4,
    "capability_usage",
  );
}

/** 维修站诊断（只读） */
export async function diagnostics(): Promise<DiagnosticReport> {
  if (!isTauri) {
    return {
      diagnostics: [],
      critical: 0,
      error: 0,
      warning: 0,
      info: 0,
      auto_fixable: 0,
      needs_confirm: 0,
      manual_only: 0,
      needs_llm: 0,
    };
  }
  return withTimeout(
    invoke<DiagnosticReport>("diagnostics"),
    HANDSHAKE_TIMEOUT_MS * 4,
    "diagnostics",
  );
}

/** 协议支持矩阵 */
export async function protocolMatrix(): Promise<ProtocolMatrix> {
  if (!isTauri) {
    return { rows: [], total_assets: 0, total_capabilities: 0, total_passed: 0, total_failed: 0 };
  }
  return withTimeout(
    invoke<ProtocolMatrix>("protocol_matrix"),
    HANDSHAKE_TIMEOUT_MS * 4,
    "protocol_matrix",
  );
}

/** provider 接入状况（本轮 implemented 恒为 0） */
export async function llmStatus(): Promise<LlmProviderReport> {
  if (!isTauri) {
    return {
      config_path: LLM_CONFIG,
      config_exists: false,
      providers: [],
      implemented: 0,
      note: "预览模式：无内核，未读取 provider 配置",
    };
  }
  return withTimeout(invoke<LlmProviderReport>("llm_status"), HANDSHAKE_TIMEOUT_MS * 4, "llm_status");
}

/** 保存（或按 id 覆盖）一条 provider 配置。
 *
 * **api_key 语义**：传 null 表示「不改动现有 key」；传空串表示「清空」。
 * 区别很重要——界面上拿不到明文，编辑表单默认不回填，
 * 若 null 被当成清空，用户改个模型名就会把 key 抹掉。
 */
export async function saveProvider(cfg: {
  id: string;
  name: string;
  kind: string;
  protocol: string;
  endpoint: string;
  model: string;
  api_key: string | null;
  enabled: boolean;
}): Promise<string> {
  if (!isTauri) {
    throw new Error("预览模式：无内核，不能保存 provider");
  }
  return withTimeout(
    invoke<string>("save_provider", { cfg }),
    HANDSHAKE_TIMEOUT_MS * 4,
    "save_provider",
  );
}

/** A 通道：宿主主动注册（中枢侧服务，本轮没有宿主调用它） */
export async function registerAgent(registration: {
  agent_id: string;
  display_name?: string;
  ecosystem?: string;
  kind?: string;
  endpoint?: string;
  meta?: string;
}): Promise<string> {
  if (!isTauri) return "";
  return invoke<string>("register_agent", { registration });
}

/** A 通道：宿主实报一次能力调用 */
export async function reportCapabilityUsage(
  agentId: string,
  capabilityId: string,
  ok: boolean,
  error?: string,
): Promise<number> {
  if (!isTauri) return 0;
  return invoke<number>("report_capability_usage", {
    agentId,
    capabilityId,
    ok,
    error: error ?? null,
  });
}
// ═══════════════════════════════════════════════════════════════
// 第二轮：资产生命周期 / 协议支持矩阵
//
// 这两条命令的**后端本轮才接入**（kernel schema v7→v8 + lib.rs 新命令）。
// 前端这里的纪律：命令不存在时**绝不静默失败**，也绝不用假数据顶替，
// 而是把「命令还没上线」这件事原样交给界面显示。
// ═══════════════════════════════════════════════════════════════

/**
 * 判断一个错误是不是「命令还没注册」。
 *
 * Tauri 对未注册的命令会 reject 一个字符串，形如：
 *   `Command set_asset_lifecycle not found`
 *   `Unknown command ...`
 * 也可能被包装成 Error。两种情况都算 —— 这不是用户操作失败，
 * 是版本没对上，界面要给的是「等后端」而不是「你点错了」。
 */
export function isMissingCommandError(e: unknown): boolean {
  const text =
    (e instanceof Error ? e.message : typeof e === "string" ? e : "") +
    (typeof e === "object" && e !== null
      ? " " + JSON.stringify(e)
      : "");
  return /not found|unknown command|does not exist|not registered/i.test(text);
}

/** 资产生命周期三态。软状态，绝不物理删除。 */
export type AssetLifecycle = "active" | "paused" | "archived";

/**
 * 设置一个资产的生命周期：active / paused / archived。
 *
 * 命令名 `set_asset_lifecycle`，参数 `{ id, state }`
 * （Tauri 2 把 JS 的 camelCase 键映射到 Rust 的 snake_case 参数，
 * 与 saveProvider 的 `{ cfg }`、decideDedup 的 `{ groupKey }` 写法一致）。
 *
 * 返回 true = 后端确认已落库。**命令不存在时抛的错要能被
 * {@link isMissingCommandError} 识别**，由调用方降级成一行提示。
 */
export async function setAssetLifecycle(
  id: string,
  state: AssetLifecycle,
): Promise<boolean> {
  if (!isTauri) {
    throw new Error("预览模式：没有内核，不能改资产生命周期");
  }
  return withTimeout(
    invoke<boolean>("set_asset_lifecycle", { id, state }),
    HANDSHAKE_TIMEOUT_MS * 4,
    "set_asset_lifecycle",
  );
}

/**
 * 智能体宿主协议清单（总览分区 ·「协议支持矩阵」侧栏）。
 *
 * 后端未就绪时**抛错**而不是返回空数组：空数组在界面上会被读成
 * 「一个宿主都没有」，而真实情况是「这条命令还没上线」。
 * 两者的处置完全不同（前者要去扫磁盘，后者只需要等版本）。
 */
export async function listAgentHosts(): Promise<AgentHost[]> {
  if (!isTauri) {
    throw new Error("预览模式：没有内核，读不到智能体协议注册表");
  }
  return withTimeout(
    invoke<AgentHost[]>("list_agent_hosts"),
    HANDSHAKE_TIMEOUT_MS * 4,
    "list_agent_hosts",
  );
}

// ── 模型分工（任务路由表）─────────────────────────────────────

/**
 * 内核的任务路由表（静态目录，不读库、不调模型）。
 *
 * 命令 `list_capability_tasks` 返回内核 `ac_registry::tasks::catalog()` 的
 * 完整结果：25 个任务节点 + 六项需求映射 + 计数 + 说明。
 *
 * **命令不存在时抛错而不是返回空表**：一张空的任务表在界面上会被读成
 * 「内核里一个可执行的任务都没有」，而真实情况是「这条命令还没注册」。
 * 两者的处置完全不同（前者要去查内核，后者只需要等后端版本）。
 *
 * 浏览器预览模式同样抛错：这张表的每个字都来自内核静态目录，
 * 前端没有理由自己造一份副本 —— 那会立刻与内核漂移，而且没人看得出来。
 */
export async function listCapabilityTasks(): Promise<TaskCatalog> {
  if (!isTauri) {
    throw new Error("预览模式：没有内核，读不到任务路由表（这张表由内核生成，前端不复制副本）");
  }
  return withTimeout(
    invoke<TaskCatalog>("list_capability_tasks"),
    HANDSHAKE_TIMEOUT_MS * 4,
    "list_capability_tasks",
  );
}

// ── 模型接入三槽位（Embedding / Judge / Rerank）────────────────

/**
 * 三个槽位的配置视图。**密钥只回掩码**（`****mqunn`），明文永不出内核。
 *
 * 这是刻意的：一旦明文进了前端，它就会顺带进入 DevTools、崩溃转储与用户截图。
 */
export async function modelSlots(): Promise<ModelSlotView[]> {
  if (!isTauri) {
    throw new Error("预览模式：没有内核，读不到模型槽位配置");
  }
  return withTimeout(invoke<ModelSlotView[]>("model_slots"), HANDSHAKE_TIMEOUT_MS * 2, "model_slots");
}

/**
 * 保存一个槽位。返回落盘路径。
 *
 * `apiKey` 语义：
 * - 传字符串 → 用这个 key
 * - 传空串 → 清空
 * - 传 `null` / 不传 → **不改动现有 key**（前端只拿到掩码，用户没动输入框就不该覆盖）
 */
export async function saveModelSlot(slot: ModelSlotConfig): Promise<string> {
  if (!isTauri) {
    throw new Error("预览模式：没有内核，无法保存模型槽位");
  }
  return withTimeout(
    invoke<string>("save_model_slot", { slot }),
    HANDSHAKE_TIMEOUT_MS * 4,
    "save_model_slot",
  );
}

/**
 * **真实调用**一次该槽位的模型（「测试连接」按钮）。
 *
 * 超时给得比较宽：judge 槽的 27B 模型实测响应可达 45 秒。
 * 这里不是「宁可降级」的场景 —— 用户就是在等一个真实结论。
 */
export async function testModelSlot(slot: string): Promise<ModelSlotProbeResult> {
  if (!isTauri) {
    throw new Error("预览模式：没有内核，无法测试模型连接");
  }
  return withTimeout(
    invoke<ModelSlotProbeResult>("test_model_slot", { slot }),
    120_000,
    "test_model_slot",
  );
}

export const runtime: Runtime = isTauri ? "tauri" : "browser-preview";