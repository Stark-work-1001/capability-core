import { useEffect, useState } from "react";
import { command } from "../operations";
interface Protocol { name: string; status: string; scope: string; source: string; }
export function ProtocolCatalog() {
 const [items,setItems]=useState<Protocol[]>([]);const [error,setError]=useState("");
 useEffect(()=>{let alive=true;command<Protocol[]>("protocol_catalog").then(v=>{if(alive)setItems(v);}).catch(e=>{if(alive)setError(String(e));});return()=>{alive=false;};},[]);
 return <section className="card"><div className="card-head"><h3>协议实现范围</h3><span className="card-num">官方资料 · 2026-10-18 核对</span></div>{error&&<p className="note">{error}</p>}<div className="workflow-grid">{items.map(p=><article className="workflow-card" key={p.name}><strong>{p.name}</strong><em>{{implemented:"已实现",partial:"部分支持",researched:"已调研 · 待实现"}[p.status]}</em><span>{p.scope}</span><a href={p.source} target="_blank" rel="noreferrer">官方协议资料 ↗</a></article>)}</div></section>;
}
