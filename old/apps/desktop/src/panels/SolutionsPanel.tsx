import { WorkflowWorkbench } from "../components/WorkflowWorkbench";
import { useCallback, useEffect, useMemo, useState } from "react";
import { diagnostics, runtime } from "../bridge";
import type { Diagnostic } from "../panel-types";

/**
 * 维修站分区 ·「故障解决方案」侧栏。
 *
 * 数据通路：`diagnostics` 的 plan —— 每条故障给出动作 / 作用域 / 逐条改动。
 *
 * ## 去重裁决已经搬走了（本轮）
 *
 * 用户要求「维修站分区新增去重类别侧栏……将左下角待确认去重的具体内容放入该界面」。
 * 所以原先塞在本面板里的 list_dedup_candidates / decide_dedup / dedup_history
 * 整段**搬到了 {@link ./DedupPanel.tsx}**，并挂成维修站分区的第四条侧栏。
 *
 * 这里是「搬走」不是「复制」：本面板不再渲染任何合并 / 共存 / 忽略按钮。
 * 两份可点的裁决按钮会让用户不知道点哪个生效 —— 界面上只能有一份。
 */

const ACTION_LABEL: Record<string, string> = {
  none: "无可执行修复",
  rebuild_search_index: "重建全文索引",
  clear_preview_cache: "清理预览缓存",
  rescan_assets: "重扫资产",
  repair_source_file: "修复源文件",
  upgrade_program: "升级程序",
};

const SCOPE_LABEL: Record<string, string> = {
  none: "不涉及文件",
  hub_owned: "中枢自己的数据",
  user_files: "用户的源文件",
};

export interface SolutionsPanelProps {
  /** 用户点「去重裁决已移到『去重』侧栏」时切过去 */
  onGoDedup?: () => void;
}

export function SolutionsPanel({ onGoDedup }: SolutionsPanelProps) {
  const [items, setItems] = useState<Diagnostic[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const load = useCallback(async () => {
    try {
      const r = await diagnostics();
      setItems(r.diagnostics);
      setErr(null);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const plans = useMemo(
    () => items.filter((d) => d.plan.action !== "none" || d.needs_llm),
    [items],
  );
  const preview = runtime === "browser-preview";

  return (
    <div className="panel-scroll">
      <WorkflowWorkbench only={["r4"]} />
      <div className="card-head sticky-head">
        <h3>故障解决方案</h3>
        <button className="link" onClick={() => void load()} disabled={preview}>
          刷新
        </button>
      </div>

      {err !== null && <div className="alert error">{err}</div>}
      {preview && (
        <div className="empty">预览模式：没有内核，读不到诊断结果。</div>
      )}

      <section className="card">
        <div className="card-head">
          <h3>修复计划</h3>
          <span className="card-num">{plans.length} 条</span>
        </div>
        {!preview && loaded && plans.length === 0 && (
          <div className="empty">
            暂无修复计划：诊断没有给出任何可执行动作，也没有需要求援 LLM 的条目。
          </div>
        )}
        <ul className="cap-list">
          {plans.map((d) => (
            <li key={d.id}>
              <b>{d.title}</b>
              <span className="cap-sum">{d.subject}</span>
              <div className="plan-row">
                <span className="plan-action">
                  {ACTION_LABEL[d.plan.action] || d.plan.action}
                </span>
                <span className="plan-scope">
                  作用域：{SCOPE_LABEL[d.plan.scope] || d.plan.scope}
                  {d.plan.destructive ? "（破坏性）" : ""}
                </span>
              </div>
              {d.plan.changes.length > 0 && (
                <ul className="plan-changes">
                  {d.plan.changes.map((c, i) => (
                    <li key={i}>{c}</li>
                  ))}
                </ul>
              )}
              {d.needs_llm && d.needs_llm_reason !== "" && (
                <div className="llm-reason">为什么求援：{d.needs_llm_reason}</div>
              )}
            </li>
          ))}
        </ul>
      </section>

      {/* 去重裁决已搬到独立侧栏：这里只留指路，不放第二份按钮 */}
      <section className="card" id="dedup-moved">
        <div className="card-head">
          <h3>去重裁决</h3>
          <span className="card-num">已移到「去重」侧栏</span>
        </div>
        <div className="note">
          去重裁决（合并 / 共存 / 忽略）已移到维修站分区的
          <b>「去重」</b>侧栏 —— 待处理资产、每条的类型与三选一按钮都在那里。
          <br />
          界面上只有一份裁决入口：这里不再渲染按钮，避免两处都能点、
          <br />
          却不知道哪一处生效。
        </div>
        {onGoDedup && (
          <div className="actions">
            <button onClick={onGoDedup}>前往「去重」侧栏 →</button>
          </div>
        )}
      </section>
    </div>
  );
}
