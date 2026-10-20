import { useCallback, useEffect, useMemo, useState } from "react";
import { capabilityUsage, runtime } from "../bridge";
import type { UsageRow } from "../panel-types";
import type { IngestSummary, RegistryStats } from "../types";

/**
 * 总览分区 ·「能力拉取」侧栏。
 *
 * 数据通路：`capability_usage`（A 通道实报）+ `registry_stats` / `rescan` 摘要。
 * 三件事：
 *  1. 谁拉了什么能力（逐条 usage 行）
 *  2. 跨宿主来源分布（按 ecosystem 聚合）
 *  3. 库内现状（kind 计数 + 上次扫描摘要）
 *
 * **推断与实测必须分开**：C 通道（静态登记）的调用次数恒为 0，
 * 那是"按磁盘路径推断出的归属"，不是宿主报上来的调用。面板单独标出来。
 */

const TRANSPORT_LABEL: Record<string, string> = {
  static: "C 静态登记",
  heartbeat: "B 心跳",
  mcp: "A 端点",
};

export interface PullPanelProps {
  stats: RegistryStats | null;
  summary: IngestSummary | null;
}

export function PullPanel({ stats, summary }: PullPanelProps) {
  const [usage, setUsage] = useState<UsageRow[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const load = useCallback(async () => {
    try {
      setUsage(await capabilityUsage(400));
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

  const dist = useMemo(() => {
    const m = new Map<string, { ecosystem: string; assets: number; rows: number; reported: number }>();
    for (const u of usage) {
      const e = m.get(u.ecosystem) ?? { ecosystem: u.ecosystem, assets: 0, rows: 0, reported: 0 };
      e.rows += 1;
      e.assets += 1;
      if (u.source === "mcp") e.reported += 1;
      m.set(u.ecosystem, e);
    }
    return [...m.values()].sort((a, b) => b.rows - a.rows);
  }, [usage]);

  const maxRows = Math.max(1, ...dist.map((d) => d.rows));
  const reportedTotal = usage.filter((u) => u.source === "mcp").length;
  const preview = runtime === "browser-preview";

  return (
    <div className="panel-scroll">
      <div className="card-head sticky-head">
        <h3>能力拉取</h3>
        <button className="link" onClick={() => void load()} disabled={preview}>
          刷新
        </button>
      </div>

      {err !== null && <div className="alert error">{err}</div>}
      {preview && (
        <div className="empty">
          预览模式：没有内核，无法查询能力用量。列表保持为空，不填占位行。
        </div>
      )}

      <section className="card">
        <div className="card-head">
          <h3>哪些智能体拉了什么能力</h3>
          <span className="card-num">{usage.length} 条</span>
        </div>
        {!preview && loaded && usage.length === 0 && (
          <div className="empty">
            暂无数据：还没有任何智能体声明或拉取能力。
            <br />
            原因：C 通道要先跑静态登记；B 通道要宿主在心跳文件里自报；
            <br />
            A 通道要宿主调用 register_agent 上报。三个通道都还没有数据。
          </div>
        )}
        <ul className="usage-list">
          {usage.slice(0, 120).map((u, i) => (
            <li key={u.agent_id + ":" + u.capability_id + ":" + i}>
              <span className={"src src-" + u.source}>
                {TRANSPORT_LABEL[u.source] || u.source}
              </span>
              <span className="usage-main">
                <b>{u.agent_name || u.agent_id}</b>
                <span className="usage-cap">
                  {u.capability_id}
                  {u.asset_name !== "" && <em> · {u.asset_name}</em>}
                  {u.call_count > 0 && <em> · 实报 {u.call_count} 次</em>}
                </span>
              </span>
            </li>
          ))}
        </ul>
        {usage.length > 120 && (
          <div className="muted small">仅显示前 120 条，共 {usage.length} 条</div>
        )}
      </section>

      <section className="card">
        <div className="card-head">
          <h3>跨宿主来源分布</h3>
          <span className="card-num">{dist.length} 个生态</span>
        </div>
        {dist.length === 0 ? (
          <div className="empty">
            暂无数据：还没有任何能力完成宿主归属。
            <br />
            归属依据是 vendor 路径上的宿主标记目录（如 .qoder-plugin）
            <br />
            与包名约定；没有标记的资产归入 unassigned，不会被硬塞给某个宿主。
          </div>
        ) : (
          <ul className="bars">
            {dist.map((d) => (
              <li key={d.ecosystem}>
                <span className="bar-label">{d.ecosystem === "unassigned" ? "未归属" : d.ecosystem}</span>
                <span className="bar-track">
                  <span
                    className="bar-fill"
                    style={{ width: (d.rows / maxRows) * 100 + "%" }}
                  />
                </span>
                <span className="bar-n">
                  {d.rows} 条 / {d.assets} 资产
                  {d.reported > 0 && <em> · 实报 {d.reported}</em>}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="card">
        <div className="card-head">
          <h3>库内现状</h3>
          <span className="card-num">{stats ? stats.total : 0} 条记录</span>
        </div>
        <div className="kv-grid">
          {(stats ? stats.counts : []).map((c) => (
            <div key={c.kind} className="kv-cell">
              <span>{c.kind}</span>
              <b>{c.count}</b>
            </div>
          ))}
        </div>
        {summary !== null && (
          <div className="kv-grid">
            <div className="kv-cell">
              <span>上次扫描解析</span>
              <b>{summary.parsed}</b>
            </div>
            <div className="kv-cell">
              <span>待确认去重组</span>
              <b>{summary.dedup_pending}</b>
            </div>
            <div className="kv-cell">
              <span>重复位置</span>
              <b>{summary.alias_count}</b>
            </div>
            <div className="kv-cell">
              <span>扫描耗时</span>
              <b>{summary.elapsed_ms} ms</b>
            </div>
          </div>
        )}
        {summary !== null && summary.has_cycle && (
          <div className="alert error">委派关系存在环</div>
        )}
      </section>

      <div className="muted small foot-note">
        A / B / C 三个通道的实报调用合计 {reportedTotal} 条。
        <br />
        C 通道的归属来自磁盘路径推断，因此调用次数恒为 0 ——
        <br />
        面板把它单独标出来，就是为了让「推断」和「实测」永远不会被混为一谈。
      </div>
    </div>
  );
}
