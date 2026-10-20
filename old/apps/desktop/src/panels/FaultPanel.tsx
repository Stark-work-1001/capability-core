import { WorkflowWorkbench } from "../components/WorkflowWorkbench";
import { StoredFaults } from "../components/StoredFaults";
import { useCallback, useEffect, useMemo, useState } from "react";
import { diagnostics, runtime } from "../bridge";
import type { Diagnostic, DiagnosticReport } from "../panel-types";

/**
 * 维修站分区 ·「故障清单」侧栏。
 *
 * 数据通路：`diagnostics`（只读诊断，本地算法优先，不调用任何模型）。
 * 这里只回答「哪里坏了」：
 *  - 按七类故障分组
 *  - 每条标出严重度、主体（哪个资产 / 哪个阶段 / 哪个文件）、错误原文
 *  - 能否自修的三档在这里只做计数，逐条计划在「故障解决方案」侧栏里展开
 *
 * 空态必须解释原因：「没有故障」与「诊断没跑起来」在界面上不能长得一样。
 */

const CATEGORY_LABEL: Record<string, string> = {
  structured_store: "结构化存储不可用",
  full_text_index: "全文索引缺失或损坏",
  vector_layer: "向量层不可用",
  llm_layer: "LLM 层不可用",
  asset_parse: "资产解析失败",
  missing_dependency: "依赖缺失",
  db_version_ahead: "DB 版本高于程序",
};

const SEV_LABEL: Record<string, string> = {
  critical: "严重",
  error: "错误",
  warning: "警告",
  info: "正常",
};

const FIX_LABEL: Record<string, string> = {
  auto: "可自动修",
  confirm: "需你确认",
  manual: "不可自修",
};

/** 从修复计划推出三档档位。作用域与破坏性由内核判定，界面只负责显示。 */
function fixOf(d: Diagnostic): "auto" | "confirm" | "manual" {
  if (d.plan.action === "none") return "manual";
  if (d.plan.scope === "user_files") return "confirm";
  if (d.plan.scope === "none") return "manual";
  return d.plan.destructive ? "confirm" : "auto";
}

export interface FaultPanelProps {
  /** 诊断结果变化时通知父组件（总览的紧凑故障计数用同一个数据源） */
  onCounts?: (c: { critical: number; error: number; warning: number }) => void;
}

export function FaultPanel({ onCounts }: FaultPanelProps) {
  const [rep, setRep] = useState<DiagnosticReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const run = useCallback(async () => {
    setBusy(true);
    try {
      const r = await diagnostics();
      setRep(r);
      setErr(null);
      if (onCounts) {
        onCounts({ critical: r.critical, error: r.error, warning: r.warning });
      }
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }, [onCounts]);

  useEffect(() => {
    void run();
  }, [run]);

  const byCategory = useMemo(() => {
    const m = new Map<string, Diagnostic[]>();
    for (const d of rep ? rep.diagnostics : []) {
      const arr = m.get(d.category) ?? [];
      arr.push(d);
      m.set(d.category, arr);
    }
    return m;
  }, [rep]);

  const preview = runtime === "browser-preview";
  const real = rep !== null ? rep.diagnostics.filter((d) => d.severity !== "info").length : 0;

  return (
    <div className="panel-scroll">
      <WorkflowWorkbench only={["r3", "r9"]} />
      <StoredFaults />
      <div className="card-head sticky-head">
        <h3>故障清单</h3>
        <div className="actions inline">
          <button onClick={() => void run()} disabled={busy || preview}>
            {busy ? "诊断中…" : "重新诊断"}
          </button>
        </div>
      </div>

      {err !== null && <div className="alert error">{err}</div>}
      {preview && (
        <div className="empty">
          预览模式：没有内核，无法运行诊断。请在 Tauri 宿主里打开本程序。
        </div>
      )}

      {!preview && rep !== null && (
        <div className="kv-grid four">
          <div className="kv-cell sev-critical">
            <span>严重</span>
            <b>{rep.critical}</b>
          </div>
          <div className="kv-cell sev-error">
            <span>错误</span>
            <b>{rep.error}</b>
          </div>
          <div className="kv-cell sev-warning">
            <span>警告</span>
            <b>{rep.warning}</b>
          </div>
          <div className="kv-cell">
            <span>正常</span>
            <b>{rep.info}</b>
          </div>
          <div className="kv-cell fix-auto">
            <span>可自动修</span>
            <b>{rep.auto_fixable}</b>
          </div>
          <div className="kv-cell fix-confirm">
            <span>需你确认</span>
            <b>{rep.needs_confirm}</b>
          </div>
          <div className="kv-cell fix-manual">
            <span>不可自修</span>
            <b>{rep.manual_only}</b>
          </div>
          <div className="kv-cell fix-llm">
            <span>建议求援 LLM</span>
            <b>{rep.needs_llm}</b>
          </div>
        </div>
      )}

      {!preview && rep !== null && real === 0 && (
        <div className="empty">
          暂无故障：诊断跑完了，没有任何 warning / error / critical 条目。
          <br />
          下面若仍有「正常」条目，那是诊断器的自检记录，不是故障。
        </div>
      )}

      {rep !== null &&
        [...byCategory.entries()].map(([cat, items]) => (
          <section className="card" key={cat}>
            <div className="card-head">
              <h3>{CATEGORY_LABEL[cat] || cat}</h3>
              <span className="card-num">{items.length} 条</span>
            </div>
            <ul className="diag-list">
              {items.map((d) => (
                <li key={d.id} className={"diag sev-" + d.severity}>
                  <div className="diag-head">
                    <span className={"sev-tag " + d.severity}>
                      {SEV_LABEL[d.severity] || d.severity}
                    </span>
                    <b className="diag-title">{d.title}</b>
                    <span className={"fix-tag " + fixOf(d)}>{FIX_LABEL[fixOf(d)]}</span>
                    {d.needs_llm && <span className="llm-tag">建议求援 LLM</span>}
                  </div>
                  <div className="diag-subject">{d.subject}</div>
                  <div className="diag-detail">{d.detail}</div>
                </li>
              ))}
            </ul>
          </section>
        ))}
    </div>
  );
}
