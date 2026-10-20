import { useEffect, useState } from "react";
import { command } from "../operations";
interface Report { observed_at: number; processes_examined: number; agents: {host: string; pid: number; state: string; confidence: string; evidence: string[]}[]; limitations: string[]; }
export function PresenceCard() {
  const [report, setReport] = useState<Report | null>(null); const [error, setError] = useState(""); const [busy, setBusy] = useState(false);
  async function refresh() {setBusy(true);try {setReport(await command<Report>("agent_presence"));setError("");}catch(e){setError(String(e));}finally{setBusy(false);}}
  useEffect(()=>{void refresh();},[]);
  return <section className="card presence-card"><div className="card-head"><h3>本机运行证据</h3><button disabled={busy} onClick={()=>void refresh()}>{busy?"探测中…":"重新探测"}</button></div>
    <p className="note">运行状态与中枢连接分别核验；普通 Node / Python、安装目录和静态资产归属不会被当作在线智能体。</p>
    {error && <p className="note">{error}</p>}
    {report && <><div className="presence-grid">{report.agents.map(a=><article key={a.host+a.pid}><strong>{a.host}</strong><span className="sev-tag info">进程运行中 · PID {a.pid}</span><p>{a.evidence.join("；")}</p></article>)}</div><p className="note">检查 {report.processes_examined} 个进程，识别 {report.agents.length} 个实例。{report.agents.length===0?"没有取得满足规则的进程证据。":""}</p><details><summary>证据边界</summary>{report.limitations.map(s=><p key={s}>{s}</p>)}</details></>}
  </section>;
}
