import { useEffect, useState } from "react";
import { command, runWorkflow, publishCapability, workflowStatus, type Workflow, type WorkflowResult, type ImportPlan } from "../operations";
import { listAssets } from "../bridge";
import type { AssetSummary } from "../types";

export function WorkflowWorkbench({ only }: { only?: string[] }) {
  const [catalog, setCatalog] = useState<Workflow[]>([]);
  const [history, setHistory] = useState<WorkflowResult[]>([]);
  const [assets, setAssets] = useState<AssetSummary[]>([]);
  const [selected, setSelected] = useState(only?.[0] ?? "r1");
  const [asset, setAsset] = useState(""); const [other, setOther] = useState("");
  const [requirement, setRequirement] = useState(""); const [revision, setRevision] = useState("");
  const [result, setResult] = useState<WorkflowResult | null>(null);
  const [error, setError] = useState(""); const [busy, setBusy] = useState(false);
  const refresh = () => command<WorkflowResult[]>("workflow_history").then(setHistory);
  useEffect(() => { let alive = true;
    void (async () => {
      const results = await Promise.allSettled([
        command<Workflow[]>("workflow_catalog"), command<WorkflowResult[]>("workflow_history"),
        Promise.resolve().then(() => listAssets({ limit: 2000 })),
      ]);
      if (!alive) return;
      const [c, h, a] = results;
      if (c.status === "fulfilled") setCatalog(c.value);
      if (h.status === "fulfilled") setHistory(h.value);
      if (a.status === "fulfilled") setAssets(a.value.items);
      setError(results.filter(r => r.status === "rejected").map(r => String((r as PromiseRejectedResult).reason)).join("；"));
    })();
    return () => { alive = false; };
  }, []);
  const choices = only ? catalog.filter(w => only.includes(w.id)) : catalog;
  const current = catalog.find(w => w.id === selected);
  async function run() { setBusy(true); setError(""); setResult(null);
    try {setResult(await runWorkflow(selected, { asset_id: asset, other_asset_id: other, requirement, revision })); await refresh();}
    catch (e) {setError(String(e));} finally {setBusy(false);}
  }
  const plan = result?.result.plan as ImportPlan | undefined;
  async function publish() {if (!plan) return; setBusy(true);setError("");
    try {const note=await publishCapability(plan);setResult(null);await refresh();setError(note);}
    catch(e) {setError(String(e));} finally {setBusy(false);}
  }
  return <section className="workflow-workbench card">
    <div className="card-head"><h3>{only ? "执行工作流" : "工作流执行与验收"}</h3><span className="card-num">6 项核心需求 + 10 项扩展</span></div>
    {!only && <div className="workflow-grid">{catalog.map(w => { const latest = history.find(h => h.workflow === w.id); return <button key={w.id} className={"workflow-card " + (w.id === selected ? "selected" : "")} onClick={() => {setSelected(w.id);setResult(null);}}>
      <span className="workflow-id">{w.id.toUpperCase()} · {w.model_slot ? "模型协作" : "规则执行"}</span><strong>{w.title}</strong><span>{w.steps.join(" → ")}</span><em>{latest ? workflowStatus[latest.status] ?? latest.status : "未运行"}</em>
    </button>;})}</div>}
    <div className="operation-form">
      <label>选择工作流<select value={selected} onChange={e => {setSelected(e.target.value); setResult(null);}}>{choices.map(w => <option key={w.id} value={w.id}>{w.id.toUpperCase()} · {w.title}</option>)}{only?.includes("dedup") && <><option value="dedup">语义比较与合并建议</option><option value="merge">生成技能合并草稿（保留原资产）</option></>}</select></label>
      <p className="note">{current?.input_hint ?? "选择两项资产，对比其功能重叠和差异"}</p>
      <div className="operation-fields"><label>目标资产<select value={asset} onChange={e => setAsset(e.target.value)}><option value="">全部 / 不指定</option>{assets.map(a => <option key={a.id} value={a.id}>{a.name} · {a.kind}</option>)}</select></label>
      {["dedup", "merge"].includes(selected) && <label>比较资产<select value={other} onChange={e => setOther(e.target.value)}><option value="">选择另一项资产</option>{assets.filter(a => a.id !== asset).map(a => <option key={a.id} value={a.id}>{a.name}</option>)}</select></label>}
      {selected === "r10" && <label>回滚版本摘要<input value={revision} onChange={e => setRevision(e.target.value)} placeholder="留空查询历史版本" /></label>}</div>
      {["r5", "r6", "r14", "r16"].includes(selected) && <label>需求与约束<textarea rows={4} value={requirement} onChange={e => setRequirement(e.target.value)} placeholder="说明能力目标、输入输出和验收条件" /></label>}
      <div className="actions"><button className="primary" disabled={busy || (!catalog.length && selected !== "dedup")} onClick={() => void run()}>{busy ? "正在执行…" : selected === "r10" && revision ? "回滚到此版本" : "执行并记录"}</button><span className="note">每次运行记录实际结果；云端未配置会明确显示未完成。</span></div>
    </div>
    {error && <div role="status" className="alert">{error}</div>}
    {result && <div className="workflow-result"><b>{workflowStatus[result.status] ?? result.status}</b><pre>{JSON.stringify(result.result, null, 2)}</pre>
      {plan && <button className="primary" disabled={busy} onClick={() => void publish()}>审核以上草稿并发布版本</button>}</div>}
    <details className="workflow-history"><summary>运行历史 · 最近 {history.length} 次</summary>{history.filter(h => !only || only.includes(h.workflow)).slice(0, 20).map(h => <details key={h.id}><summary>{h.workflow.toUpperCase()} · {workflowStatus[h.status] ?? h.status}</summary><pre>{JSON.stringify(h.result, null, 2)}</pre></details>)}</details>
  </section>;
}
