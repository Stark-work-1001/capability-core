import { useState } from "react";
import { AssetResearch } from "./AssetResearch";
import type { AssetLifecycle } from "../bridge";
import { isMissingCommandError, setAssetLifecycle } from "../bridge";
import type { CapabilityManifest, NormalizedManifest } from "../types";
import {
  ASSET_LIFECYCLES,
  AUTHORITY_META,
  authorLabel,
  authorSourceMeta,
  kindColor,
  kindLabel,
  lifecycleMeta,
  normalizeManifest,
} from "../types";

/**
 * 右侧通用属性栏。
 *
 * 从 App.tsx 里内联的 `<Detail>` 抽出来，供资产各类侧栏共用：
 * 选中的资产 id 变化 → 父组件重新 getAsset → 这里按资产类型渲染字段。
 *
 * **BUG 1 的修复点仍然是 normalizeManifest**：入参是原始 manifest
 * （可选键随时可能整个缺失），第一件事就是补齐，之后所有访问都作用在
 * {@link NormalizedManifest} 上。之前直接 `manifest.provides.length`
 * 在零能力资产（7 条缺 provides）和绝大多数资产（488 条缺 relations）
 * 上抛 TypeError → React 渲染中断 → 整个窗口黑屏。
 *
 * 资产「来源 / 自建 / 自包含 / 兼容性」这类状态字段，
 * 内核 manifest 里真实存在的是 identity.* 与 provenance.* —— 这里照实渲染，
 * 不存在的维度写「未提供」，不编。
 *
 * ## 本轮新增（用户逐条点名）
 *
 * 1. **作者 + 出处** —— `summary.author` 配 `summary.author_source`：
 *    自声明 / frontmatter / 包推断三档分开标注。空串不渲染（不写 unknown）。
 * 2. **支持的智能体类型** —— `summary.host_support` 数组；
 *    为空显示「未判出归属宿主（不是错误）」，绝不填一个假宿主。
 * 3. **是否正在使用** —— 由 `capability_usage` 反查（来源智能体那一段已有数据），
 *    有则显示调用次数与最后使用时间，无则「暂无使用记录」。
 * 4. **三态切换按钮**（激活 / 暂停 / 归档）—— 调新命令 `set_asset_lifecycle`。
 *    命令不存在（后端本轮才接）时**不白屏、不静默失败**，
 *    在按钮下方留一行明确提示，并把按钮禁用。
 */

export interface DetailPaneProps {
  manifest: CapabilityManifest | null;
  loading: boolean;
  /** 没选中时的提示（默认文案见下） */
  emptyText?: string;
  /** 选中的资产摘要（列表行），用于展示 manifest 里没有、但列表里有的状态 */
  summary?: {
    id: string;
    kind: string;
    name: string;
    authority_label: string;
    tool_posture: string;
    side_effect: string;
    source_path: string | null;
    capability_count: number;
    location_count?: number;
    location_role?: string | null;
    /** 作者（后端推断；空串 = 判不出来） */
    author?: string | null;
    /** 作者出处：manifest / frontmatter / package / 空串 */
    author_source?: string | null;
    /** 支持的宿主智能体（空数组 / null = 未判出归属宿主，不是错误） */
    host_support?: string[] | null;
    /** 生命周期 active / paused / archived */
    lifecycle?: string | null;
  } | null;
  /** 来源智能体（从 capability_usage 关联而来） */
  sources?: { agentId: string; agentName: string; calls: number; lastUsedAt?: number }[];
  /** 来源智能体数据加载失败 / 未参与时的说明 */
  sourcesNote?: string;
  /** 生命周期切换成功后的回调（父组件据此刷新列表与徽标） */
  onLifecycleChanged?: (state: AssetLifecycle) => void;
}

export function DetailPane({
  manifest,
  loading,
  emptyText,
  summary,
  sources,
  sourcesNote,
  onLifecycleChanged,
}: DetailPaneProps) {
  if (loading) {
    return <div className="placeholder muted">读取资产详情…</div>;
  }
  if (!manifest) {
    return (
      <div className="placeholder muted">
        {emptyText ?? "在左侧选中一条资产，这里显示它的身份维度、能力与溯源。"}
      </div>
    );
  }

  const m: NormalizedManifest = normalizeManifest(manifest);
  const id = m.identity;
  const kindTone = kindColor(m.kind);
  const hosts = summary?.host_support ?? [];
  const inUse = (sources ?? []).length > 0;
  const totalCalls = (sources ?? []).reduce((n, s) => n + s.calls, 0);
  // 作者行只算一次：没有作者时返回 null，整行不渲染。
  const author = authorRow(summary);

  return (
    <div className="detail-body">
      <div className="detail-title">
        <span
          className="tag"
          style={{
            background: kindTone + "22",
            color: kindTone,
            borderColor: kindTone + "55",
          }}
        >
          {kindLabel(m.kind)}
        </span>
        <h3>{m.name}</h3>
        <code className="detail-id">{m.id}</code>
      </div>

      {m.description && <p className="detail-desc">{m.description}</p>}
      <AssetResearch id={m.id} />

      {/* 已归档 / 已删除的资产：它此刻不在默认列表里，属性栏必须说出来，
          否则用户会觉得「我删了它怎么还在这儿」，或者以为自己删失败了。 */}
      {(summary?.lifecycle ?? "") === "archived" && (
        <div className="note archive-banner">
          该资产当前是<b>已归档</b>（= 删除）：默认资产列表已不再列出它，
          但<b>行仍在库里</b>，上方的「激活 / 暂停」随时可以恢复。
        </div>
      )}

      {/* ── 生命周期：三态切换（软删除，绝不物理删除）── */}
      <LifecycleSection
        assetId={summary?.id ?? m.id}
        current={summary?.lifecycle ?? null}
        onChanged={onLifecycleChanged}
      />

      <h4>状态 / 来源</h4>
      <dl className="kv">
        <dt>权限</dt>
        <dd style={{ color: AUTHORITY_META[id.authority]?.color }}>
          {AUTHORITY_META[id.authority]?.label ?? id.authority}
        </dd>
        <dt>副作用</dt>
        <dd>{sideEffectLabel(id.side_effect)}</dd>
        <dt>工具姿态</dt>
        <dd>{toolPostureLabel(id.tool_posture)}</dd>
        <dt>幂等</dt>
        <dd>{id.idempotent ? "是" : "否"}</dd>
        <dt>开放世界</dt>
        <dd>{id.open_world ? "是" : "否"}</dd>
        {id.role && (
          <>
            <dt>角色</dt>
            <dd>{id.role}</dd>
          </>
        )}
        {/* 作者 + 出处：空串 = 后端判不出来，这一行直接不出现（不写 unknown）。
            「作者是谁」和「这个值从哪来」是两个信息：前者是字符串，
            后者决定可信度（作者自声明 > frontmatter > 包路径反推）。
            所以这里写「作者（自声明）」这种带出处的标签，而不是光写「作者」。 */}
        {author !== null && (
          <>
            <dt>{author.label}</dt>
            <dd title={author.hint}>{summary?.author}</dd>
          </>
        )}
        <dt>来源</dt>
        <dd>
          {m.provenance.source_format || "未知格式"}
          {m.provenance.source_path ? (
            <span className="detail-src"> · {m.provenance.source_path}</span>
          ) : (
            " · 未提供路径"
          )}
        </dd>
        {summary && (
          <>
            <dt>磁盘位置</dt>
            <dd>
              {summary.location_count ?? 1} 个
              {summary.location_role ? `（${summary.location_role}）` : ""}
            </dd>
            <dt>能力条数</dt>
            <dd>{summary.capability_count}</dd>
          </>
        )}
      </dl>

      {/* ── 支持的智能体类型 ── */}
      <h4>支持的智能体类型</h4>
      {hosts.length > 0 ? (
        <div className="chips host-support-chips">
          {hosts.map((h) => (
            <span className="chip host-chip" key={h} title="该资产被这个宿主支持">
              {h}
            </span>
          ))}
        </div>
      ) : (
        <p className="muted small host-none">
          未判出归属宿主（不是错误）—— 该资产的路径与 manifest 里都没有可识别的宿主标记。
        </p>
      )}

      {/* ── 是否正在使用 ── */}
      <h4>是否正在使用</h4>
      {inUse ? (
        <div className="in-use">
          <span className="use-dot" />
          <b>正在使用</b>
          <span className="muted small">
            实报调用 {totalCalls} 次
            {lastUsedText(sources) !== null
              ? " · 最后使用 " + lastUsedText(sources)
              : ""}
          </span>
        </div>
      ) : (
        <p className="muted small cap-none">
          暂无使用记录：capability_usage 里没有该资产的调用记录（这只说明没有实报，不代表资产不可用）。
        </p>
      )}

      <h4>来源智能体（谁拉过它）</h4>
      {sourcesNote ? (
        <p className="muted small">{sourcesNote}</p>
      ) : sources && sources.length > 0 ? (
        <ul className="cap-list">
          {sources.map((s) => (
            <li key={s.agentId}>
              <b>{s.agentName || s.agentId}</b>
              <span className="cap-sum">
                实报调用 {s.calls} 次
                {s.lastUsedAt !== undefined && s.lastUsedAt > 0
                  ? " · 最后使用 " + formatUnix(s.lastUsedAt)
                  : ""}
              </span>
            </li>
          ))}
        </ul>
      ) : (
        <p className="muted small cap-none">
          暂无实报：capability_usage 里没有该资产的调用记录。
        </p>
      )}

      {Object.keys(id.dimensions).length > 0 && (
        <>
          <h4>推断维度</h4>
          <div className="chips">
            {Object.entries(id.dimensions).map(([k, v]) => (
              <span className="chip" key={k}>
                {k}: {v}
              </span>
            ))}
          </div>
        </>
      )}

      <h4>能力（{m.provides.length}）</h4>
      {m.provides.length === 0 ? (
        <p className="muted small cap-none">该资产未声明任何能力。</p>
      ) : (
        <ul className="cap-list">
          {m.provides.map((p, i) => (
            <li key={p.capability_id || `cap-${i}`}>
              <b>{p.name}</b>
              {/* capability_id 是宿主上报用量时用的键（capability_usage.capability_id），
                  必须显示出来，否则界面上对不上账 */}
              {p.capability_id && p.capability_id !== p.name && (
                <code className="cap-id">{p.capability_id}</code>
              )}
              {p.tags.length > 0 && (
                <span className="chips cap-tags">
                  {p.tags.map((t) => (
                    <span className="chip" key={t}>
                      {t}
                    </span>
                  ))}
                </span>
              )}
              {p.summary && <span className="cap-sum">{p.summary}</span>}
            </li>
          ))}
        </ul>
      )}

      {m.relations.length > 0 && (
        <>
          <h4>关系（{m.relations.length}）</h4>
          <ul className="rel-list">
            {m.relations.map((r, i) => (
              <li key={`${r.target_id}-${i}`}>
                <span className="rel-kind">{r.kind}</span>
                <code>{r.target_id}</code>
              </li>
            ))}
          </ul>
        </>
      )}

      {m.provenance.content_hash && (
        <>
          <h4>溯源</h4>
          <code className="detail-id" title={m.provenance.source_path ?? ""}>
            {m.provenance.content_hash}
          </code>
        </>
      )}
    </div>
  );
}

/**
 * 生命周期：三态切换 + **删除**。
 *
 * 三个状态按钮**并列**而不是一个开关：active / paused / archived 是三种状态，
 * 不是「开 / 关」。当前状态那个按钮高亮并禁用（点它没有意义）。
 *
 * ## 「删除」为什么是第四个按钮、却又不是第四种状态
 *
 * 项目铁律是「增量不删」：任何操作都不物理删行。所以这里的「删除」
 * **不新增状态**，它的落点就是 `archived` —— 但它是用户表达「我不要了」，
 * 与「归档」的「暂时下架」是两种意图，界面必须分开给按钮：
 *
 *  - 归档：我暂时不想让它参与分派，回头还会用；
 *  - 删除：我认为这条不该再出现，请从默认列表里拿掉。
 *
 * 两个按钮都调 `set_asset_lifecycle(id, 'archived')`，但删除多一道
 * **二次确认**，并在界面上写明「删除 = 归档（不物理删除，可在归档视图中恢复）」——
 * 绝不能让用户以为磁盘上的文件被删了。
 *
 * 备选方案 B（后端加 deleted 状态）不做：那要改 kernel，而「多一个状态」
 * 并不比「同一个 archived 的两种意图」表达得更清楚，反而多一处迁移风险。
 *
 * 后端 `set_asset_lifecycle` 命令不存在时：
 *  - 每个按钮禁用；
 *  - 下方留一行明确的「后端尚未提供…」提示；
 *  - **不弹窗、不白屏、不假装点成功了**。
 */
function LifecycleSection({
  assetId,
  current,
  onChanged,
}: {
  assetId: string;
  current: string | null;
  onChanged?: (state: AssetLifecycle) => void;
}) {
  const [busy, setBusy] = useState<AssetLifecycle | null>(null);
  const [state, setState] = useState<string | null>(current);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  /** 后端明确没有这条命令 → 三个按钮置灰，只留一行提示 */
  const [missing, setMissing] = useState(false);
  /** 「删除」的二次确认：第一次点只把按钮变成「确认删除？」，再点才真调后端。 */
  const [confirmDelete, setConfirmDelete] = useState(false);

  // props 里的 current 变了（切换了资产）就同步过来。
  // 用 key 重置更干净，但这里的 setState-in-render 会在同一次提交里完成，
  // 不会产生额外渲染 —— 比 useEffect 少一帧闪旧值。
  const [seen, setSeen] = useState<string | null>(current);
  if (current !== seen) {
    setSeen(current);
    setState(current);
    setMsg(null);
    setMissing(false);
    setConfirmDelete(false);
  }

  const meta = lifecycleMeta(state);

  const apply = async (next: AssetLifecycle, opts?: { viaDelete?: boolean }) => {
    if (next === state) return;
    setBusy(next);
    setMsg(null);
    try {
      await setAssetLifecycle(assetId, next);
      setState(next);
      setMissing(false);
      setConfirmDelete(false);
      setMsg({
        ok: true,
        text: opts?.viaDelete
          ? "已从默认列表移除（= 归档，未物理删除任何文件；可在归档视图中恢复）"
          : "已切到「" + lifecycleLabel(next) + "」，库已更新（软状态，未删任何行）",
      });
      onChanged?.(next);
    } catch (e) {
      setConfirmDelete(false);
      if (isMissingCommandError(e)) {
        setMissing(true);
        setMsg({
          ok: false,
          text:
            "内核尚未提供 set_asset_lifecycle 命令（后端本轮正在接入）—— " +
            "生命周期与删除按钮已置灰，不会改动任何数据。原始错误：" + String(e),
        });
      } else {
        setMsg({ ok: false, text: "操作失败：" + String(e) });
      }
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="lifecycle">
      <div className="lifecycle-head">
        <h4>生命周期</h4>
        {meta && (
          <span className="lifecycle-current" style={{ color: meta.color }}>
            当前：{meta.short}
          </span>
        )}
      </div>
      <div className="lifecycle-actions" role="group" aria-label="资产生命周期">
        {ASSET_LIFECYCLES.map((l) => {
          const active = state === l.key;
          return (
            <button
              key={l.key}
              type="button"
              className={"life-btn " + l.key + (active ? " is-current" : "")}
              style={
                active
                  ? { color: l.color, borderColor: l.color + "88", background: l.color + "18" }
                  : undefined
              }
              disabled={active || busy !== null || missing}
              aria-pressed={active}
              onClick={() => void apply(l.key)}
              title={l.hint}
            >
              {busy === l.key ? "切换中…" : l.label}
              {active && " ✓"}
            </button>
          );
        })}
      </div>
      {/* ── 删除（第四颗按钮，但**不新增第四种状态**）──
          删除 = 置 archived + 二次确认。项目铁律「增量不删」：
          这里绝不物理删行，界面上也必须把这句话写出来，不能让用户
          以为磁盘文件被删了。 */}
      <div className="lifecycle-delete">
        {!confirmDelete ? (
          <button
            type="button"
            className="life-btn delete"
            disabled={busy !== null || missing || state === "archived"}
            onClick={() => {
              setMsg(null);
              setConfirmDelete(true);
            }}
            title="删除 = 归档（不物理删除，可在归档视图中恢复）"
          >
            删除
          </button>
        ) : (
          <>
            <button
              type="button"
              className="life-btn delete confirm"
              disabled={busy !== null || missing}
              onClick={() => void apply("archived", { viaDelete: true })}
              title="再点一次执行：置为 archived（软删除，不删任何行）"
            >
              {busy === "archived" ? "删除中…" : "确认删除？"}
            </button>
            <button
              type="button"
              className="life-btn cancel"
              disabled={busy !== null}
              onClick={() => setConfirmDelete(false)}
              title="取消，不做任何改动"
            >
              取消
            </button>
          </>
        )}
      </div>
      <p className="muted small lifecycle-note lifecycle-delete-note">
        <b>删除 = 归档</b>：不物理删除、不删任何文件，只是从默认列表里拿掉，
        随时可以切回「激活」或「暂停」恢复。与「归档」的区别只是意图 ——
        归档是暂时下架，删除是「我不要再看到它」。当前：
        {meta ? meta.short : state ?? "未提供（后端本轮才落 lifecycle 字段）"}
      </p>
      <p className="muted small lifecycle-note">
        三挡都是<b>软状态</b>：归档不会删除任何行，随时可以切回来。
      </p>
      {msg !== null && (
        <div className={"alert " + (msg.ok ? "ok" : "error")}>{msg.text}</div>
      )}
    </section>
  );
}

function lifecycleLabel(v: string): string {
  return ASSET_LIFECYCLES.find((l) => l.key === v)?.label ?? v;
}

/**
 * 属性栏里作者那一行的标签与提示。
 *
 * 三种情况：
 *  - 没有作者 → 返回 null，整行不渲染（不写「未知作者」占位）；
 *  - 有作者 + 有 author_source → 「作者（自声明）/（frontmatter）/（包推断）」+ 出处说明；
 *  - 有作者 + 没有 author_source → 退回「作者」，**不编出处**（后端本轮才落该字段）。
 */
function authorRow(
  summary:
    | { author?: string | null; author_source?: string | null }
    | null
    | undefined,
): { label: string; hint: string } | null {
  if (!summary) return null;
  const label = authorLabel(summary.author, summary.author_source);
  if (label === null) return null;
  const meta = authorSourceMeta(summary.author_source);
  return {
    label,
    hint: meta
      ? meta.hint
      : "后端未提供作者出处（author_source），因此只标「作者」而不猜来源",
  };
}

/** 摘要里的最后使用时间 → 人類可读。没有数据返回 null。 */
function lastUsedText(
  sources: { lastUsedAt?: number }[] | undefined,
): string | null {
  if (!sources || sources.length === 0) return null;
  let max = 0;
  for (const s of sources) {
    if (typeof s.lastUsedAt === "number" && s.lastUsedAt > max) max = s.lastUsedAt;
  }
  return max > 0 ? formatUnix(max) : null;
}

/** unix 秒 → 本地时间。只用 Intl，不引依赖。 */
function formatUnix(sec: number): string {
  if (!Number.isFinite(sec) || sec <= 0) return "—";
  try {
    return new Date(sec * 1000).toLocaleString();
  } catch {
    return String(sec);
  }
}

const SIDE_EFFECT_LABEL: Record<string, string> = {
  none: "无（none）",
  local_artifact: "本地副产物（local_artifact）",
  mutating: "改变外部（mutating）",
  irreversible: "不可回滚（irreversible）",
};

const TOOL_POSTURE_LABEL: Record<string, string> = {
  pure: "纯计算（pure）",
  reader: "只读（reader）",
  writer: "写入（writer）",
  gated: "需审批（gated）",
};

function sideEffectLabel(v: string): string {
  return SIDE_EFFECT_LABEL[v] ?? v;
}

function toolPostureLabel(v: string): string {
  return TOOL_POSTURE_LABEL[v] ?? v;
}
