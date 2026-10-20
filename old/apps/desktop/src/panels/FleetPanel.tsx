import { PresenceCard } from "../components/PresenceCard";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  capabilityUsage,
  listAgents,
  registerStaticAgents,
  runtime,
  scanHeartbeats,
} from "../bridge";
import type {
  AgentRecord,
  HeartbeatScanReport,
  StaticRegistrationReport,
  UsageRow,
} from "../panel-types";
import {
  buildFleetGraph,
  FLEET_GEOMETRY,
  layoutFleet,
  type FleetAgentNode,
  type FleetGraph,
} from "../fleet-model";

/**
 * 总览分区 ·「在线智能体」侧栏。
 *
 * 两种视图可切换：
 *  - 图形化：智能体分派给哪些能力（按 ecosystem 分组，SVG 贝塞尔连线 + chip）
 *  - 列表：同数据的表格形态，带通道 / 状态 / 调用次数
 *
 * **图必须真的从数据来**：没有任何智能体时一个节点都不画，
 * 只显示「暂无数据 + 原因」。视觉形态参考本机 dsh-context 插件的
 * Fleet（编队）tab 线框图 —— 分组行 / 卡片 / 贝塞尔连线 / 状态点 / chip。
 * 这里只借形态，不 import 它的任何模块、不读它的运行时数据、不用它的令牌。
 */

const TRANSPORT_LABEL: Record<string, string> = {
  static: "C 静态登记",
  heartbeat: "B 心跳",
  mcp: "A 端点",
};

const STATE_COLOR: Record<string, string> = {
  online: "#43e0a6",
  stale: "#ffc94d",
  offline: "#ff8080",
  unknown: "#6b7f9c",
};

const STATE_LABEL: Record<string, string> = {
  online: "在线",
  stale: "心跳过期",
  offline: "离线",
  unknown: "无心跳通道",
};

type ViewMode = "graph" | "list";

export interface FleetPanelProps {
  /** 故障计数与维修站同源；这里只做紧凑展示 */
  faultCounts: { critical: number; error: number; warning: number } | null;
  onGoRepair: () => void;
}

export function FleetPanel({ faultCounts, onGoRepair }: FleetPanelProps) {
  const [agents, setAgents] = useState<AgentRecord[]>([]);
  const [usage, setUsage] = useState<UsageRow[]>([]);
  const [mode, setMode] = useState<ViewMode>("graph");
  const [staticRep, setStaticRep] = useState<StaticRegistrationReport | null>(null);
  const [hbRep, setHbRep] = useState<HeartbeatScanReport | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const reload = useCallback(async () => {
    try {
      const [a, u] = await Promise.all([listAgents(), capabilityUsage(400)]);
      setAgents(a);
      setUsage(u);
      setErr(null);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 后台自刷新：中枢有一条 30s 一轮的心跳扫描线程（见 src-tauri/src/lib.rs
  // 的 assets-heartbeat），它落库后**不会**推事件给前端。
  //
  // 早先只有挂载时 reload 一次，于是「插件明明写心跳、库里 observed_state
  // 已经是 online、界面却一直显示在线 0」——用户只能手动点按钮才发现。
  // 这里加 20s 轮询（比扫描周期略短，最多落后一轮），让在线数自己追上来。
  //
  // 轮询失败不打扰用户：只在首次加载失败时显示错误，后续静默重试。
  useEffect(() => {
    const t = setInterval(() => {
      void reload();
    }, 20_000);
    return () => clearInterval(t);
  }, [reload]);

  /** C 通道：跑一次静态登记。幂等，重复点不会产生重复行。 */
  const runStatic = useCallback(async () => {
    setBusy("static");
    try {
      setStaticRep(await registerStaticAgents());
      await reload();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  }, [reload]);

  /** B 通道：扫一次心跳目录。 */
  const runHeartbeat = useCallback(async () => {
    setBusy("heartbeat");
    try {
      setHbRep(await scanHeartbeats());
      await reload();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  }, [reload]);

  const graph: FleetGraph = useMemo(
    () => buildFleetGraph(agents, usage),
    [agents, usage],
  );
  const layout = useMemo(() => layoutFleet(graph.groups), [graph]);
  const liveCount = useMemo(
    () => agents.filter((a) => a.heartbeat && a.heartbeat.state === "online").length,
    [agents],
  );
  const preview = runtime === "browser-preview";

  return (
    <div className="panel-scroll">
      <PresenceCard />
      <div className="card-head sticky-head">
        <h3>在线智能体</h3>
        <div className="view-toggle" role="group" aria-label="视图切换">
          <button
            type="button"
            className={mode === "graph" ? "active" : ""}
            aria-pressed={mode === "graph"}
            onClick={() => setMode("graph")}
            title="图形化：智能体 → 能力的分派关系"
          >
            图形化
          </button>
          <button
            type="button"
            className={mode === "list" ? "active" : ""}
            aria-pressed={mode === "list"}
            onClick={() => setMode("list")}
            title="列表：同一批数据的表格形态"
          >
            列表
          </button>
        </div>
      </div>

      {err !== null && <div className="alert error">{err}</div>}

      {/* 概要条：全部来自 list_agents / capability_usage */}
      <div className="fleet-summary">
        <span className="fleet-chip-stat">
          智能体 {graph.groups.reduce((n, g) => n + g.agents.length, 0)}
        </span>
        <span className={"fleet-chip-stat" + (liveCount > 0 ? " on" : "")}>
          在线 {liveCount}
        </span>
        <span className="fleet-chip-stat">分组 {graph.groups.length}</span>
        <span className="fleet-chip-stat">能力 {graph.capabilityTotal}</span>
        <span className="fleet-chip-stat">实报 {graph.reportedTotal}</span>
        <span
          className={
            "fleet-chip-stat" + (graph.errorTotal > 0 ? " bad" : "")
          }
        >
          错误 {graph.errorTotal}
        </span>
      </div>

      {preview && (
        <div className="empty">
          预览模式：没有内核，无法查询智能体。请在 Tauri 宿主里打开本程序。
        </div>
      )}

      {!preview && graph.groups.length === 0 && (
        <div className="empty">
          暂无数据：库里还没有任何智能体登记，因此不画任何节点或连线。
          <br />
          原因：静态归属是从磁盘路径推断出来的，需要先扫描过 vendor 目录；
          <br />
          心跳通道要宿主往共享目录写 .json；端点通道要宿主调用 register_agent。
          <br />
          点下面的「跑一次静态登记」即可按路径上的宿主标记推断归属。
        </div>
      )}

      {!preview && graph.groups.length > 0 && mode === "graph" && (
        <div className="fleet-canvas" aria-label="智能体能力分派图">
          {layout.groups.map((g) => (
            <section className="fleet-group" key={g.ecosystem}>
              <div className="fleet-group-head">
                <span className="fleet-group-dot" style={{ background: g.color }} />
                <b>{g.ecosystem === "unassigned" ? "未归属生态" : g.ecosystem}</b>
                <span className="muted small">
                  {g.blocks.length} 个智能体 ·{" "}
                  {g.blocks.reduce((n, b) => n + b.chips.length, 0)} 项能力
                </span>
              </div>
              <div className="fleet-stage" style={{ height: g.height || 1 }}>
                <svg
                  className="fleet-links"
                  width={layout.chipLeft}
                  height={g.height || 1}
                  viewBox={`0 0 ${layout.chipLeft} ${g.height || 1}`}
                >
                  {g.links.map((l, i) => (
                    <path
                      key={i}
                      d={l.d}
                      stroke={l.color}
                      fill="none"
                      strokeWidth={l.calls > 0 ? 1.6 : 1}
                      strokeDasharray={l.calls > 0 ? undefined : "3 4"}
                      opacity={l.calls > 0 ? 0.85 : 0.45}
                    />
                  ))}
                </svg>
                {g.blocks.map((b) => (
                  <div key={b.agent.agentId}>
                    <AgentCard node={b.agent} top={b.top} />
                    {b.chips.map((c) => (
                      <span
                        key={c.label}
                        className={
                          "fleet-cap" +
                          (c.hasError ? " error" : "") +
                          (c.source === "mcp" ? " reported" : "")
                        }
                        style={{
                          left: layout.chipLeft,
                          top: c.top,
                          height: layout.chipHeight,
                          maxWidth: `calc(100% - ${layout.chipLeft + 8}px)`,
                        }}
                        title={
                          (c.source === "mcp" ? "A 通道实报" : "C 通道推断（调用恒 0）") +
                          " · 调用 " +
                          c.calls +
                          " 次" +
                          (c.assetName ? " · 资产 " + c.assetName : "") +
                          (c.hasError ? " · 最近一次报错" : "")
                        }
                      >
                        <span className="fleet-cap-label">{c.label}</span>
                        <span className="fleet-cap-n">{c.calls}</span>
                      </span>
                    ))}
                  </div>
                ))}
              </div>
            </section>
          ))}
          <div className="legend">
            连线 = 一条真实的 capability_usage 记录；虚线表示 C 通道推断归属
            （没有实报，调用次数恒为 0，**不是**实测）。
            <br />
            没有数据的智能体不画节点，没有能力的不画连线 —— 这张图不补任何假点。
          </div>
        </div>
      )}

      {!preview && graph.groups.length > 0 && mode === "list" && (
        <FleetList graph={graph} />
      )}

      {/* ④ 故障能力报错（紧凑版，与维修站同源） */}
      <section className="card">
        <div className="card-head">
          <h3>④ 故障能力报错</h3>
          <button className="link" onClick={onGoRepair}>
            在维修站查看详情 →
          </button>
        </div>
        {faultCounts === null ? (
          <div className="empty">尚未运行诊断。</div>
        ) : (
          <div className="kv-grid three">
            <div className="kv-cell sev-critical">
              <span>严重</span>
              <b>{faultCounts.critical}</b>
            </div>
            <div className="kv-cell sev-error">
              <span>错误</span>
              <b>{faultCounts.error}</b>
            </div>
            <div className="kv-cell sev-warning">
              <span>警告</span>
              <b>{faultCounts.warning}</b>
            </div>
          </div>
        )}
      </section>

      <div className="actions">
        <button onClick={runStatic} disabled={busy !== null || preview}>
          {busy === "static" ? "登记中…" : "跑一次静态登记（C）"}
        </button>
        <button onClick={runHeartbeat} disabled={busy !== null || preview}>
          {busy === "heartbeat" ? "扫描中…" : "扫描心跳目录（B）"}
        </button>
        <button className="ghost" onClick={() => void reload()} disabled={preview}>
          刷新
        </button>
      </div>

      {staticRep !== null && (
        <div className="note ok">
          静态登记完成：{staticRep.ecosystems} 个生态 · {staticRep.assets} 项资产 ·{" "}
          {staticRep.capability_rows} 条能力归属 · {staticRep.unassigned_assets}{" "}
          项资产无宿主标记（未归属，不计入分布图）
        </div>
      )}
      {hbRep !== null && (
        <div className={hbRep.hosts_registered > 0 ? "note ok" : "note"}>
          心跳目录 {hbRep.dir}：
          {hbRep.dir_exists
            ? "读到 " + hbRep.files_seen + " 个 .json，登记 " +
              hbRep.hosts_registered + " 个宿主"
            : "暂无数据 —— 目录尚不存在。还没有任何宿主接入 B 通道。"}
          {hbRep.rejected.length > 0 && (
            <ul className="reject-list">
              {hbRep.rejected.map((r) => (
                <li key={r.file}>
                  {r.file}：{r.reason}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {!loaded && !preview && <div className="muted small">加载中…</div>}
    </div>
  );
}

/** 图上的智能体卡片。 */
function AgentCard({ node, top }: { node: FleetAgentNode; top: number }) {
  return (
    <div
      className="fleet-node"
      style={{ top, width: FLEET_GEOMETRY.cardWidth }}
      title={
        TRANSPORT_LABEL[node.transport] +
        (node.calls > 0 ? " · 实报调用 " + node.calls + " 次" : " · 无实报调用")
      }
    >
      <div className="fleet-node-head">
        <span
          className="state-dot"
          style={{ background: STATE_COLOR[node.state] ?? STATE_COLOR.unknown }}
        />
        <b className="fleet-node-name">{node.label}</b>
      </div>
      <div className="fleet-node-meta">
        <span>{TRANSPORT_LABEL[node.transport] || node.transport}</span>
        <span>·</span>
        <span>{STATE_LABEL[node.state]}</span>
      </div>
      <div className="fleet-node-meta">
        <span>能力 {node.capabilities.length}</span>
        {node.capabilities.length < node.declaredCapabilities && (
          <span> / 声明 {node.declaredCapabilities}</span>
        )}
        <span> · 调用 {node.calls}</span>
      </div>
    </div>
  );
}

/** 列表形态：同一批数据的表格。 */
function FleetList({ graph }: { graph: FleetGraph }) {
  const rows: FleetAgentNode[] = graph.groups.flatMap((g) => g.agents);
  return (
    <table className="matrix fleet-table">
      <thead>
        <tr>
          <th>智能体</th>
          <th>生态</th>
          <th>通道</th>
          <th>状态</th>
          <th>能力</th>
          <th className="num">实报调用</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((n) => (
          <tr key={n.agentId}>
            <td>{n.label}</td>
            <td>{n.ecosystem === "unassigned" ? "未归属" : n.ecosystem}</td>
            <td>{TRANSPORT_LABEL[n.transport] || n.transport}</td>
            <td>
              <span
                className="state-dot inline"
                style={{ background: STATE_COLOR[n.state] ?? STATE_COLOR.unknown }}
              />
              {STATE_LABEL[n.state]}
            </td>
            <td className="fleet-cell-caps">
              {n.capabilities.length === 0 ? (
                <span className="muted small">未拉取</span>
              ) : (
                n.capabilities.map((c) => (
                  <span
                    key={c.label}
                    className={"chip" + (c.lastError ? " chip-err" : "")}
                    title={
                      (c.source === "mcp" ? "A 通道实报" : "C 通道推断") +
                      " · 调用 " +
                      c.calls +
                      " 次"
                    }
                  >
                    {c.label}
                  </span>
                ))
              )}
            </td>
            <td className="num">{n.calls}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
