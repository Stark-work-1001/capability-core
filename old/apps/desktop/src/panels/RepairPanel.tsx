/**
 * @deprecated M3.5 起界面不再挂载本组件，功能拆到 FaultPanel（故障清单）+ SolutionsPanel（解决方案）+ FixPanel（修复）。
 *
 * 文件保留只有一个原因：panels.test.tsx 里针对它的断言是这批 UI 的回归网，
 * 本轮不删断言（删断言凑绿是被明确禁止的）。新代码请用上面那几个面板。
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { diagnostics, runtime } from "../bridge";
import type { Diagnostic, DiagnosticReport } from "../panel-types";
import { DedupPanel } from "./DedupPanel";

/**
 * 维修站侧栏。用户点名要装去重裁决（「这个部分放在维修站中」）。
 *
 * **本轮去重裁决进一步拆成独立侧栏**（维修站 →「去重」，见 DedupPanel.tsx）：
 * 本组件不再自己实现裁决逻辑，而是直接复用 {@link DedupPanel}。
 * 这是「搬」不是「复制」—— 裁决代码在仓库里只有一份，
 * 界面上可点的裁决按钮也只有一个来源。
 *
 * 维修站还要回答三件事：
 *
 * 1. 故障分类（七类）
 * 2. 故障内容（哪个资产 / 哪个阶段 / 错误原文）
 * 3. 能否自修（三档）+ 是否需要求援主对话模型
 *
 * ## 「自修」的边界在界面上也是明示的
 *
 * 每条诊断都标出作用域：中枢自己的数据（可自动修 / 破坏性要确认）
 * vs 用户的源文件（**永远**要确认，并逐条列出将要改什么）。
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

const FIX_LABEL: Record<string, string> = {
  auto: "可自动修",
  confirm: "需你确认",
  manual: "不可自修",
};

const SCOPE_LABEL: Record<string, string> = {
  none: "不涉及文件",
  hub_owned: "中枢自己的数据",
  user_files: "用户的源文件",
};

const ACTION_LABEL: Record<string, string> = {
  none: "无可执行修复",
  rebuild_search_index: "重建全文索引",
  clear_preview_cache: "清理预览缓存",
  rescan_assets: "重扫资产",
  repair_source_file: "修复源文件",
  upgrade_program: "升级程序",
};

const SEV_LABEL: Record<string, string> = {
  critical: "严重",
  error: "错误",
  warning: "警告",
  info: "正常",
};

export interface RepairPanelProps {
  /** 诊断结果变化时通知父组件，用于总览的故障计数 */
  onCounts?: (c: {
    critical: number;
    error: number;
    warning: number;
  }) => void;
}

export function RepairPanel({ onCounts }: RepairPanelProps) {
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
        onCounts({
          critical: r.critical,
          error: r.error,
          warning: r.warning,
        });
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
  return (
    <div className="panel-scroll">
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

      {!preview && rep !== null && (
        <div className="legend">
          自修边界：
          <b>可自动修</b> = 只动中枢自己的数据且非破坏性；
          <b>需你确认</b> = 改配置 / 装依赖 / 动你的源文件 / 破坏性重建；
          <b>不可自修</b> = 缺文件、协议不支持、需升级程序。
          <br />
          中枢**绝不**自动修改用户的插件 / 技能源文件 —— 那一律降级为「需你确认」，
          <br />
          并逐条列出将要改什么。
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
                    <span className={"fix-tag " + fixOf(d)}>
                      {FIX_LABEL[fixOf(d)]}
                    </span>
                    {d.needs_llm && <span className="llm-tag">建议求援 LLM</span>}
                  </div>
                  <div className="diag-subject">{d.subject}</div>
                  <div className="diag-detail">{d.detail}</div>
                  <div className="diag-plan">
                    <span className="plan-action">
                      {ACTION_LABEL[d.plan.action] || d.plan.action}
                    </span>
                    <span className="plan-scope">
                      作用域：{SCOPE_LABEL[d.plan.scope] || d.plan.scope}
                      {d.plan.destructive ? "（破坏性）" : ""}
                    </span>
                    <span className="plan-sum">{d.plan.summary}</span>
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
        ))}
      {/* 去重裁决：用户点名要装在这里。
          本轮它已进一步拆成维修站分区的独立侧栏（「去重」），
          但这里仍然渲染**同一个**组件（不是复制品）：
          RepairPanel 是旧版合并面板，回归测试仍然跑它，
          所以它的去重区必须与「去重」侧栏行为一致。 */}
      <section className="card" id="dedup">
        <div className="card-head">
          <h3>去重裁决</h3>
          <span className="card-num">数据源与「去重」侧栏同一份</span>
        </div>
        <DedupPanel flat />
      </section>
    </div>
  );
}

/** 从修复计划推出三档档位。作用域与破坏性由内核判定，界面只负责显示。 */
function fixOf(d: Diagnostic): "auto" | "confirm" | "manual" {
  if (d.plan.action === "none") return "manual";
  if (d.plan.scope === "user_files") return "confirm";
  if (d.plan.scope === "none") return "manual";
  return d.plan.destructive ? "confirm" : "auto";
}
