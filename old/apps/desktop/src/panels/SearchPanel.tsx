import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listAssets, runtime } from "../bridge";
import { command } from "../operations";
import type { AssetSummary } from "../types";
import { kindColor, kindLabel } from "../types";

/** Five real execution paths; unavailable models report an error. */
export type SearchMode =
  | "keyword"
  | "python"
  | "rust"
  | "local-vector"
  | "cloud-vector";

interface ModeDef {
  id: SearchMode;
  label: string;
  hint: string;
  /** 接的哪条通路 —— 属性栏与侧栏都会显示原文 */
  path: string;
  state: "live" | "stub";
}

export const SEARCH_MODES: ModeDef[] = [
  {
    id: "keyword",
    label: "关键词",
    hint: "SQLite LIKE，按名称 / 描述 / ID / 角色匹配",
    path: "list_assets({ text }) · 内核结构化过滤",
    state: "live",
  },
  {
    id: "python",
    label: "Python",
    hint: "本地 Python 侧检索器",
    path: "Python BM25 + 中文二元分词",
    state: "live",
  },
  {
    id: "rust",
    label: "Rust",
    hint: "Rust 原生检索（BM25 / tantivy）",
    path: "Rust Tantivy BM25 · 自动同步索引",
    state: "live",
  },
  {
    id: "local-vector",
    label: "本地向量",
    hint: "本机嵌入模型 + 向量库",
    path: "本机嵌入槽位 · 向量缓存 · 余弦召回",
    state: "live",
  },
  {
    id: "cloud-vector",
    label: "云端向量",
    hint: "云端嵌入模型 + 向量库",
    path: "云端嵌入槽位 · 向量缓存 · 可选 Reranker",
    state: "live",
  },
];

export interface SearchPanelProps {
  /**
   * 选中一条检索结果。
   *
   * 传整条 {@link AssetSummary} 而不只是 id：`author` /
   * `host_support` / `lifecycle` 只在列表行里有（manifest 里没有），
   * 属性栏要靠它显示作者、支持的智能体与生命周期按钮。
   * 与 AssetKindPanel 的回调签名保持一致 —— 同一个属性栏不该因为
   * 从哪个侧栏点进来而少显示几项。
   */
  onSelect: (item: AssetSummary) => void;
  /** 顶栏全局检索带过来的关键词（回车跳转时注入） */
  initialQuery?: string;
}

export function SearchPanel({ onSelect, initialQuery = "" }: SearchPanelProps) {
  const [mode, setMode] = useState<SearchMode>("keyword");
  const [text, setText] = useState(initialQuery);
  const [items, setItems] = useState<AssetSummary[]>([]);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [notes, setNotes] = useState<string[]>([]);
  const [ran, setRan] = useState(false);

  const active = useMemo(
    () => SEARCH_MODES.find((m) => m.id === mode) ?? SEARCH_MODES[0],
    [mode],
  );

  const run = useCallback(async () => {
    if (active.state !== "live") return;
    setLoading(true);
    try {
      const r = mode === "keyword" ? await listAssets({ text: text.trim() || null, limit: 80, offset: 0 }) : await command<{items: AssetSummary[]; total: number; notes: string[]}>("search_capabilities", {mode, query: text.trim()});
      setItems(r.items);
      setTotal(r.total);
      setNotes("notes" in r ? r.notes as string[] : []);
      setErr(null);
      setRan(true);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  }, [text, active, mode]);

  useEffect(() => {
    setItems([]);
    setTotal(0);
    setRan(false);
  }, [mode]);

  // 顶栏带过来的关键词：进侧栏就自动跑一次，省得用户再点一次「检索」。
  const seedRan = useRef(false);
  useEffect(() => {
    if (seedRan.current || initialQuery.trim() === "") return;
    seedRan.current = true;
    setText(initialQuery);
    void run();
  }, [initialQuery, run]);

  const preview = runtime === "browser-preview";

  return (
    <div className="panel-scroll">
      <div className="card-head sticky-head">
        <h3>能力检索</h3>
        <span className="card-num">{SEARCH_MODES.length} 种检索方式</span>
      </div>

      <section className="card">
        <div className="card-head">
          <h3>检索方式</h3>
          <span className="card-num">5 种执行通路 · 向量按配置启用</span>
        </div>
        <div className="mode-grid">
          {SEARCH_MODES.map((m) => (
            <button
              key={m.id}
              type="button"
              className={
                "mode-card" +
                (mode === m.id ? " active" : "") +
                (m.state === "stub" ? " stub" : "")
              }
              aria-pressed={mode === m.id}
              onClick={() => setMode(m.id)}
            >
              <span className="mode-name">{m.label}</span>
              <span className="mode-hint">{m.hint}</span>
              <span className={"mode-state " + m.state}>
                {m.state === "live" ? "已接" : "未接"}
              </span>
            </button>
          ))}
        </div>
        <div className="note">
          当前选中：<b>{active.label}</b> · 数据通路：{active.path}
        </div>
      </section>

      {active.state === "stub" && (
        <div className="empty warn-empty">
          <b>「{active.label}」本轮未接后端。</b>
          <br />
          {active.path}
          <br />
          所以这里不显示任何结果 —— 拿关键词检索的结果填进来会让人误以为
          <br />
          语义检索已经可用，那比空着更糟。选「关键词」可以用真实检索。
        </div>
      )}

      {active.state === "live" && (
        <>
          <div className="search-row">
            <input
              className="kind-search"
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void run();
              }}
              placeholder="输入名称 / 描述 / ID 关键词"
              aria-label="检索关键词"
              disabled={preview}
            />
            <button onClick={() => void run()} disabled={loading || preview}>
              {loading ? "检索中…" : "检索"}
            </button>
          </div>

          {notes.map((note,i) => <div className="note" key={i}>{note}</div>)}
          {err !== null && <div className="alert error">{err}</div>}
          {preview && (
            <div className="empty">预览模式：没有内核，检索不可用。</div>
          )}

          {ran && !preview && (
            <div className="results-head inline-head">
              <span>
                {loading ? "检索中…" : total + " 项命中" + (text ? " · 「" + text + "」" : "")}
              </span>
              {total > items.length && (
                <span className="more">仅显示前 {items.length} 项</span>
              )}
            </div>
          )}

          {ran && !preview && total === 0 && (
            <div className="empty">没有匹配的资产。</div>
          )}

          <ul className="asset-list">
            {items.map((it) => (
              <li key={it.id}>
                <button className="asset-row" onClick={() => onSelect(it)}>
                  <span
                    className="tag"
                    style={{
                      background: kindColor(it.kind) + "22",
                      color: kindColor(it.kind),
                      borderColor: kindColor(it.kind) + "55",
                    }}
                  >
                    {kindLabel(it.kind)}
                  </span>
                  <span className="asset-main">
                    <span className="asset-name">{it.name}</span>
                    <span className="asset-desc">{it.description || "（无描述）"}</span>
                  </span>
                  <span className="asset-meta">
                    <span className="caps">⚙ {it.capability_count ?? 0}</span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}
