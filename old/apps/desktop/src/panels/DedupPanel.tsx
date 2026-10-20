import { WorkflowWorkbench } from "../components/WorkflowWorkbench";
import { useCallback, useEffect, useMemo, useState } from "react";
import { decideDedup, dedupHistory, listDedupCandidates, runtime } from "../bridge";
import type { DedupCandidate, DedupDecision, DedupRecord } from "../types";
import { kindColor, kindLabel } from "../types";

/**
 * 维修站分区 ·「去重」侧栏。
 *
 * 用户原话：「维修站分区新增去重类别侧栏，列出待处理资产，并表明其类型，
 * 将左下角待确认去重的具体内容放入该界面」。
 *
 * ## 界面上的裁决按钮只有这一份
 *
 * 原来这套裁决逻辑在 SolutionsPanel 里，本轮**整体搬到这里**：
 * SolutionsPanel 不再渲染任何合并 / 共存 / 忽略按钮，只留一句指向本面板的说明。
 * 两份可点的裁决按钮会让人不知道点了哪个生效 —— 所以是搬，不是复制。
 *
 * ## 「类型」是怎么判出来的
 *
 * `list_dedup_candidates` 返回的每组只有 group_key / match_level /
 * suggested_canonical / location_ids，**没有 kind 字段**。
 * 而 location_id 的构造规则（kernel ac-registry/.../store.rs）是
 * `'loc-' || assets.id`，assets.id 又稳定带 `skill::` / `mcp::` / `plugin::` 前缀。
 *
 * 所以类型是**从位置 id 前缀真真切切读出来的**，不是猜的：
 *   - 组内所有位置同类型 → 显示该类型；
 *   - 组内跨类型（L2 规范化命中才可能）→ 显示全部类型；
 *   - 前缀不认识 → 显示「未识别」，绝不当成某一类。
 */

/** `loc-skill::foo` → `skill`；认不出来返回 null。 */
export function kindOfLocationId(locationId: string): string | null {
  const bare = locationId.replace(/^loc-/, "");
  const m = /^([a-z][a-z0-9_]*?)::/.exec(bare);
  return m ? m[1] : null;
}

/** 一组候选里出现的全部 kind（保序、去重）。 */
export function kindsOfCandidate(c: DedupCandidate): string[] {
  const out: string[] = [];
  for (const loc of c.location_ids ?? []) {
    const k = kindOfLocationId(loc);
    if (k !== null && !out.includes(k)) out.push(k);
  }
  return out;
}

export interface DedupPanelProps {
  /** 裁决后通知父组件刷新侧栏徽标（成功/失败都通知，失败时数字不变） */
  onDecided?: () => void;
  /**
   * 嵌入模式：被旧版 RepairPanel 复用时不要自带滚动容器
   * （它已经被套在一个 .card 里了，再套一层 .panel-scroll 会出现双滚动条）。
   */
  flat?: boolean;
}

export function DedupPanel({ onDecided, flat = false }: DedupPanelProps) {
  const [candidates, setCandidates] = useState<DedupCandidate[]>([]);
  const [history, setHistory] = useState<DedupRecord[]>([]);
  const [busyGroup, setBusyGroup] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [loaded, setLoaded] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [c, h] = await Promise.all([listDedupCandidates(), dedupHistory()]);
      setCandidates(c);
      setHistory(h);
      setErr(null);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /**
   * 用户裁决一组去重候选。
   *
   * **这是界面上唯一能改变去重结果的入口。** 三个选项都没有默认值，
   * 也没有超时自动合并 —— 程序绝不替用户做这个决定。
   */
  const decide = useCallback(
    async (c: DedupCandidate, d: DedupDecision) => {
      const key = c.match_level + ":" + c.group_key;
      setBusyGroup(key);
      setNote(null);
      try {
        await decideDedup(c.group_key, c.match_level, d);
        setNote("已裁决 " + key + " → " + decisionLabel(d) + "，库已更新");
        await load();
        onDecided?.();
      } catch (e) {
        setNote("裁决失败：" + String(e));
      } finally {
        setBusyGroup(null);
      }
    },
    [load, onDecided],
  );

  const preview = runtime === "browser-preview";
  const kindSummary = useMemo(() => {
    const m = new Map<string, number>();
    for (const c of candidates) {
      for (const k of kindsOfCandidate(c)) m.set(k, (m.get(k) ?? 0) + 1);
    }
    return [...m.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
  }, [candidates]);

  return (
    <div className={"dedup-panel-page" + (flat ? " dedup-panel-flat" : " panel-scroll")}>
      <WorkflowWorkbench only={["dedup"]} />
      <div className="card-head sticky-head">
        <h3>去重 · 待处理资产</h3>
        <div className="actions inline">
          <span className="card-num">{candidates.length} 组待确认</span>
          <button className="link" onClick={() => void load()} disabled={loading || preview}>
            {loading ? "读取中…" : "刷新"}
          </button>
        </div>
      </div>

      {preview && (
        <div className="empty">
          预览模式：没有内核，读不到去重候选。这里不会填占位分组。
        </div>
      )}
      {err !== null && <div className="alert error">{err}</div>}

      {!preview && loaded && (
        <div className="kv-grid three">
          <div className="kv-cell">
            <span>待确认组</span>
            <b>{candidates.length}</b>
          </div>
          <div className="kv-cell">
            <span>涉及位置</span>
            <b>
              {candidates.reduce((n, c) => n + (c.location_ids?.length ?? 0), 0)}
            </b>
          </div>
          <div className="kv-cell">
            <span>已裁决</span>
            <b>{history.length}</b>
          </div>
        </div>
      )}

      {!preview && kindSummary.length > 0 && (
        <div className="dedup-kinds">
          <span className="muted small">涉及类型：</span>
          <span className="chips">
            {kindSummary.map(([k, n]) => (
              <span
                key={k}
                className="chip"
                style={{
                  color: kindColor(k),
                  borderColor: kindColor(k) + "55",
                }}
                title={"该类型出现在 " + n + " 组候选里"}
              >
                {kindLabel(k)} · {n} 组
              </span>
            ))}
          </span>
        </div>
      )}

      {!preview &&
        (candidates.length === 0 && history.length === 0 ? (
          <div className="empty">
            暂无去重候选。库内每个逻辑资产都只有一个磁盘位置。
            <br />
            原因：去重候选由扫描按指纹（L1 字节相同 / L2 规范化后相同）生成；
            <br />
            没有候选 = 没有发现重复内容，不是功能没生效。
          </div>
        ) : (
          <p className="dedup-note">
            下列内容的指纹相同或规范化后相同。<b>程序没有合并任何一条</b>，
            <br />
            怎么处理由你决定 —— 三个选项都没有默认值，也没有超时自动合并。
            <br />
            每组的类型由位置 id 的资产前缀读出（<code>loc-skill::…</code> → Skill）。
          </p>
        ))}

      <ul className="dedup-list">
        {candidates.map((c) => {
          const key = c.match_level + ":" + c.group_key;
          const busy = busyGroup === key;
          const level = (c.match_level || "?").toString();
          const locs = c.location_ids || [];
          const kinds = kindsOfCandidate(c);
          return (
            <li key={key} className="dedup-item">
              <div className="dedup-head">
                <span className={"lvl " + level.toLowerCase()}>{level.toUpperCase()}</span>
                {kinds.length === 0 ? (
                  <span className="dedup-kind unknown" title="位置 id 里读不出资产类型前缀">
                    类型未识别
                  </span>
                ) : (
                  <span className="dedup-kinds-inline">
                    {kinds.map((k) => (
                      <span
                        key={k}
                        className="dedup-kind"
                        style={{
                          color: kindColor(k),
                          borderColor: kindColor(k) + "55",
                          background: kindColor(k) + "18",
                        }}
                        title={"该组位置属于 " + kindLabel(k) + " 类资产"}
                      >
                        {kindLabel(k)}
                      </span>
                    ))}
                  </span>
                )}
                <code className="dedup-key">{c.group_key || "（无分组键）"}</code>
                <span className="dedup-n">{locs.length} 个位置</span>
              </div>
              <ul className="dedup-locs">
                {locs.map((id) => (
                  <li
                    key={id}
                    className={id === c.suggested_canonical ? "canon" : ""}
                  >
                    {id === c.suggested_canonical && <b>规范 </b>}
                    <code>{id.replace(/^loc-/, "")}</code>
                  </li>
                ))}
              </ul>
              <div className="dedup-actions">
                <button
                  disabled={busy}
                  onClick={() => void decide(c, "merge")}
                  title="丢弃别名，只保留规范位置"
                >
                  合并
                </button>
                <button
                  disabled={busy}
                  onClick={() => void decide(c, "keep_both")}
                  title="全部升为独立逻辑资产，各算各的"
                >
                  共存
                </button>
                <button
                  disabled={busy}
                  onClick={() => void decide(c, "ignore")}
                  title="维持别名角色，下次扫描再问"
                >
                  忽略
                </button>
              </div>
            </li>
          );
        })}
      </ul>

      {note !== null && <div className="note">{note}</div>}

      {history.length > 0 && (
        <details className="dedup-history">
          <summary>已裁决记录（{history.length}）</summary>
          <ul>
            {history.map((h) => (
              <li key={h.match_level + ":" + h.group_key}>
                <code>{h.group_key}</code>
                <span className="dec">{decisionLabel(h.decision)}</span>
                <span className="muted small">
                  {(h.location_ids || []).length} 个位置
                </span>
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}

function decisionLabel(d: string): string {
  switch (d) {
    case "merge":
      return "合并";
    case "keep_both":
      return "共存";
    case "ignore":
      return "忽略";
    default:
      return d || "-";
  }
}
