import { useState } from "react";
import { command, publishCapability, type ImportPlan, type ImportRequest } from "../operations";
import { WorkflowWorkbench } from "../components/WorkflowWorkbench";
export function CreatePanel() {
  const [id, setId] = useState(""); const [kind, setKind] = useState("skill");
  const [name, setName] = useState(""); const [description, setDescription] = useState("");
  const [content, setContent] = useState(""); const [author, setAuthor] = useState("");
  const [repository, setRepository] = useState(""); const [license, setLicense] = useState("");
  const [plan, setPlan] = useState<ImportPlan | null>(null); const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  async function preview() {setBusy(true);setMessage("");setPlan(null);
    const entry = kind === "skill" ? "SKILL.md" : kind === "mcp" ? "mcp.json" : "manifest.json";
    const request: ImportRequest = {id,kind,name,description,entry,files:{[entry]:content},author,repository,license};
    try {setPlan(await command<ImportPlan>("prepare_capability", {request}));} catch(e) {setMessage(String(e));} finally {setBusy(false);}
  }
  async function publish() {if (!plan) return;setBusy(true);setMessage("");try {setMessage(await publishCapability(plan));setPlan(null);}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  return <div className="panel-scroll"><div className="card-head sticky-head"><h3>新增能力</h3><span className="card-num">预览 → 发布 → 索引 → 回滚</span></div>
    <section className="card operation-form" onChange={() => setPlan(null)}><div className="card-head"><h3>收录能力</h3><span className="card-num">写入当前版本的数据目录</span></div>
      <div className="operation-fields"><label>标识<input value={id} onChange={e=>setId(e.target.value)} placeholder="my-capability" /></label><label>类型<select value={kind} onChange={e=>setKind(e.target.value)}>{["skill","mcp","plugin","ghost","organization"].map(k=><option key={k} value={k}>{k}</option>)}</select></label><label>名称<input value={name} onChange={e=>setName(e.target.value)} /></label></div>
      <label>功能介绍<textarea rows={2} value={description} onChange={e=>setDescription(e.target.value)} placeholder="何时使用、输入输出、依赖与限制" /></label>
      <div className="operation-fields"><label>作者<input value={author} onChange={e=>setAuthor(e.target.value)} /></label><label>来源仓库<input value={repository} onChange={e=>setRepository(e.target.value)} /></label><label>许可证<input value={license} onChange={e=>setLicense(e.target.value)} /></label></div>
      <label>{kind === "skill" ? "SKILL.md 正文" : "入口 JSON"}<textarea className="code-input" rows={10} value={content} onChange={e=>setContent(e.target.value)} placeholder={kind === "skill" ? "---\nname: my-capability\ndescription: 功能说明\n---\n执行步骤与验收条件" : '{"name":"my-capability"}'} /></label>
      <p className="note">原始来源保留；每次发布生成独立版本。包含脚本的包不会在导入时自动执行。</p>
      <button className="primary" onClick={()=>void preview()} disabled={busy || !id || !name || !description || !content}>{busy?"处理中…":"检查并生成收录预览"}</button>
    </section>
    {message && <div role="status" className="alert">{message}</div>}
    {plan && <section className="card workflow-result"><h3>待发布预览</h3><p>{plan.asset_id} · {plan.bytes} 字节 · {plan.files.join("、")}</p><code>{plan.digest}</code>{plan.warnings.map(w=><p key={w} className="note">{w}</p>)}<button className="primary" disabled={busy} onClick={()=>void publish()}>确认收录此版本</button></section>}
    <WorkflowWorkbench only={["r16","r10","r7","r15"]} />
  </div>;
}
