import { useCallback, useEffect, useState } from "react";
import { command, runWorkflow, workflowStatus } from "../operations";
type Fault = {fault_id: string; asset_id: string; asset_name: string; server: string | null; failure_class: string; status: string; location: {field: string; value: string; source_file: string}; prerequisites: string[]};
export function StoredFaults() {
  const [rows,setRows]=useState<Fault[]>([]);const [error,setError]=useState("");
  const [detail,setDetail]=useState<unknown>(null);const [busy,setBusy]=useState(false);
  const load=useCallback(async()=>{try{setRows(await command<Fault[]>("stored_faults"));setError("");}catch(e){setError(String(e));}},[]);
  useEffect(()=>{void load();},[load]);
  async function inspect(id:string){try{setDetail(await command("fault_detail",{id}));}catch(e){setError(String(e));}}
  async function sweep(){setBusy(true);try{const r=await runWorkflow("r3");if(r.status!=="completed")setError(`${workflowStatus[r.status]}：${JSON.stringify(r.result)}`);else await load();}catch(e){setError(String(e));}finally{setBusy(false);}}
  return <section className="card"><div className="card-head"><h3>已登记的能力故障</h3><button disabled={busy} onClick={()=>void sweep()}>{busy?"巡检中…":"巡检并同步数据库"}</button></div>
    {error&&<p role="status" className="note">{error}</p>}
    {!error&&!rows.length&&<p className="note">尚无已登记故障。运行巡检以检查 MCP 配置、路径和依赖。</p>}
    <ul className="cap-list">{rows.map(f=><li key={f.fault_id}><b>{f.asset_name}{f.server?` · ${f.server}`:""}</b><span>{f.failure_class} · {f.status}</span><code>{f.asset_id}</code><p>定位：{f.location.field} = {f.location.value}</p><p className="note">{f.location.source_file}</p><p>{f.prerequisites.join("；")}</p><button onClick={()=>void inspect(f.fault_id)}>证据与历次维修记录</button></li>)}</ul>
    {detail!=null&&<div className="workflow-result"><pre>{JSON.stringify(detail,null,2)}</pre></div>}
  </section>;
}
