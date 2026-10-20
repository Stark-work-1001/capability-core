import { WorkflowWorkbench } from "../components/WorkflowWorkbench";
import { command } from "../operations";
import { useCallback, useEffect, useState } from "react";
import { diagnostics, runtime } from "../bridge";
import type { Diagnostic, DiagnosticReport } from "../panel-types";

/** Hub repairs execute locally; model changes are staged and reviewed. */
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

export function FixPanel() {
  const [rep, setRep] = useState<DiagnosticReport | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const load = useCallback(async () => {
    try {
      setRep(await diagnostics());
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

  const preview = runtime === "browser-preview";
  const items: Diagnostic[] = (rep?.diagnostics ?? []).filter(
    (d) => d.plan.action !== "none",
  );

  return (
    <div className="panel-scroll">
      <div className="card-head sticky-head">
        <h3>故障修复</h3>
        <span className="card-num">本地修复 · 模型方案 · 审计记录</span>
      </div>

      <WorkflowWorkbench only={["r5", "r4", "r10"]} />

      {err !== null && <div className="alert error">{err}</div>}
      {preview && (
        <div className="empty">预览模式：没有内核，读不到诊断结果。</div>
      )}

      {!preview && loaded && items.length === 0 && (
        <div className="empty">
          暂无待修项：诊断没有给出任何可执行计划。
          <br />
          （这可能是真的没有故障，也可能是 <code>diagnostics</code> 返回空 ——
          两者界面无法区分，所以这里不写「一切正常」的下结论。）
        </div>
      )}

      {items.length > 0 && (
        <ul className="cap-list fix-plans">
          {items.map((d) => (
            <li key={d.id}>
              <div className="fix-plan-head">
                <b>{d.title}</b>
                <span className={"sev-tag " + d.severity}>
                  {ACTION_LABEL[d.plan.action] || d.plan.action}
                </span>
              </div>
              <div className="cap-sum">{d.plan.summary}</div>
              <div className="fix-plan-scope">
                作用域：{SCOPE_LABEL[d.plan.scope] || d.plan.scope}
                {d.plan.destructive ? "（破坏性）" : ""}
              </div>
              {d.plan.changes.length > 0 && (
                <ul className="plan-changes">
                  {d.plan.changes.map((c, i) => (
                    <li key={i}>{c}</li>
                  ))}
                </ul>
              )}
              <div className="actions">
                <button disabled={!['rebuild_search_index','rescan_assets'].includes(d.plan.action)} onClick={async () => {
                  try { await command('repair_hub', {action:d.plan.action}); await load(); }
                  catch (e) {setErr(String(e));}
                }}>{['rebuild_search_index','rescan_assets'].includes(d.plan.action) ? '执行中枢修复' : '需生成修复方案'}</button>
                <span className="muted small">执行后重新诊断；原始资产修订通过上方工作流预览。</span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
