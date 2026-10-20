import { useEffect, useState } from "react";
import { command } from "../operations";

export function AssetResearch({ id }: { id: string }) {
  const [row, setRow] = useState<Record<string, unknown> | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {let alive = true;setRow(null);setError("");
    void command<Record<string, unknown>>("asset_research", {id}).then(v => {if(alive)setRow(v);}).catch(e => {if(alive)setError(String(e));});
    return () => {alive = false;};
  }, [id]);
  const label = (value: unknown) => value == null || value === "" ? "未声明" : typeof value === "string" ? value : JSON.stringify(value);
  const repo = typeof row?.repository === "string" ? row.repository : (row?.repository as {url?: string})?.url;
  const safeRepo = repo?.replace(/^git\+/, "");
  return <details><summary>仓库、作者与许可证证据</summary>
    {error && <p className="note">{error}</p>}
    {row && <><p>作者：{label(row.author)}</p><p>许可证：{label(row.license)}</p>
      <p>仓库：{safeRepo && /^https:\/\//.test(safeRepo) ? <a href={safeRepo} target="_blank" rel="noreferrer">{safeRepo}</a> : label(repo)}</p>
      <p>上游仓库许可证：{label(row.upstream_license)}</p>
      <p className="note">{row.verified_online ? "已核对上游仓库" : "来自本地声明，尚未在线核验"} · {row.content_verified ? "正文指纹与上游一致" : "本地正文版本尚未匹配"}</p>
      <p className="note">技能声明的许可证与上游仓库许可证分别保留。{label(row.metadata_source)}</p>
      <details><summary>查看证据</summary><pre>{JSON.stringify(row.evidence ?? [],null,2)}</pre></details></>}
  </details>;
}
