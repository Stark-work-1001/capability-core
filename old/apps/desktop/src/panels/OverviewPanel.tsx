/**
 * @deprecated M3.5 起界面不再挂载本组件，功能拆到 FleetPanel（在线智能体，图形化 / 列表）+ PullPanel（能力拉取）。
 *
 * 文件保留只有一个原因：panels.test.tsx 里针对它的断言是这批 UI 的回归网，
 * 本轮不删断言（删断言凑绿是被明确禁止的）。新代码请用上面那几个面板。
 */

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
  EcosystemCount,
  HeartbeatScanReport,
  StaticRegistrationReport,
  UsageRow,
} from "../panel-types";
import type { IngestSummary, RegistryStats } from "../types";

/**
 * 总览侧栏：用户要看的三样 ——
 *
 * 1. 哪些智能体在线
 * 2. 哪些智能体拉了什么能力
 * 3. 故障能力报错（与维修站共享数据源，这里展示得更紧凑）
 *
 * 数据全部来自内核查询。**查不到就写「暂无数据 + 原因」，不填占位数字。**
 */

export interface OverviewProps {
  stats: RegistryStats | null;
  summary: IngestSummary | null;
  /** 故障计数，与维修站同一个诊断命令 */
  faultCounts: { critical: number; error: number; warning: number } | null;
  onGoRepair: () => void;
}

const TRANSPORT_LABEL: Record<string, string> = {
  static: "C 静态登记",
  heartbeat: "B 心跳",
  mcp: "A 端点",
};

const STATE_COLOR: Record<string, string> = {
  online: "#34d399",
  stale: "#fbbf24",
  offline: "#f87171",
};

export function OverviewPanel({
  stats,
  summary,
  faultCounts,
  onGoRepair,
}: OverviewProps) {
  const [agents, setAgents] = useState<AgentRecord[]>([]);
  const [usage, setUsage] = useState<UsageRow[]>([]);
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

  // ── 派生量 ──
  const byTransport = useMemo(() => {
    const m: Record<string, AgentRecord[]> = {};
    for (const a of agents) {
      (m[a.transport] ??= []).push(a);
    }
    return m;
  }, [agents]);

  const live = useMemo(
    () => agents.filter((a) => a.heartbeat && a.heartbeat.state === "online"),
    [agents],
  );
  const notLive = useMemo(
    () => agents.filter((a) => !a.heartbeat || a.heartbeat.state !== "online"),
    [agents],
  );

  const dist = useMemo<EcosystemCount[]>(() => {
    const m = new Map<string, EcosystemCount>();
    for (const u of usage) {
      const e =
        m.get(u.ecosystem) ??
        ({
          ecosystem: u.ecosystem,
          agents: 0,
          assets: 0,
          capabilities: 0,
          reported: 0,
        });
      e.capabilities += 1;
      if (u.asset_id) e.assets += 1;
      if (u.source === "mcp") e.reported += 1;
      m.set(u.ecosystem, e);
    }
    const arr = [...m.values()];
    for (const e of arr) {
      e.agents = byTransport[e.ecosystem] ? 1 : 0;
    }
    return arr.sort((a, b) => b.capabilities - a.capabilities);
  }, [usage, byTransport]);

  const maxCap = Math.max(1, ...dist.map((d) => d.capabilities));
  const reportedTotal = usage.filter((u) => u.source === "mcp").length;
  const preview = runtime === "browser-preview";

  return (
    <div className="panel-scroll">
      {err !== null && <div className="alert error">{err}</div>}
      {/* ① 哪些智能体在线 */}
      <section className="card">
        <div className="card-head">
          <h3>① 哪些智能体在线</h3>
          <span className="card-num">
            在线 {live.length} / 登记 {agents.length}
          </span>
        </div>
        {preview && (
          <div className="empty">
            预览模式：没有内核，无法查询智能体。请在 Tauri 宿主里打开本程序。
          </div>
        )}
        {!preview && loaded && agents.length === 0 && (
          <div className="empty">
            暂无数据：库里还没有任何智能体登记。
            <br />
            原因：静态归属是从磁盘路径推断出来的，需要先扫描过 vendor 目录。
            <br />
            点下面的「跑一次静态登记」即可按路径上的宿主标记推断归属。
          </div>
        )}
        {live.length > 0 && (
          <ul className="agent-live">
            {live.map((a) => (
              <li key={a.agent_id}>
                <span
                  className="state-dot"
                  style={{ background: STATE_COLOR.online }}
                />
                <b>{a.display_name || a.agent_id}</b>
                <span className="muted small">
                  {TRANSPORT_LABEL[a.transport] || a.transport} · 心跳{" "}
                  {a.heartbeat ? formatAge(a.heartbeat.age_ms) : "-"}前
                </span>
              </li>
            ))}
          </ul>
        )}
        {agents.length > 0 && (
          <details className="fold">
            <summary>
              非在线 / 无心跳 {notLive.length} 个
              （静态登记与端点注册本来就没有心跳，这是事实不是故障）
            </summary>
            <ul className="agent-list">
              {notLive.map((a) => (
                <li key={a.agent_id}>
                  <span
                    className="state-dot"
                    style={{
                      background:
                        STATE_COLOR[a.heartbeat?.state || ""] || "#64748b",
                    }}
                  />
                  <b>{a.display_name || a.agent_id}</b>
                  <span className="muted small">
                    {TRANSPORT_LABEL[a.transport] || a.transport}
                    {a.heartbeat
                      ? " · " + a.heartbeat.state + " · " +
                        formatAge(a.heartbeat.age_ms) + "前"
                      : ""}
                  </span>
                </li>
              ))}
            </ul>
          </details>
        )}
        <div className="actions">
          <button onClick={runStatic} disabled={busy !== null || preview}>
            {busy === "static" ? "登记中…" : "跑一次静态登记（C）"}
          </button>
          <button
            onClick={runHeartbeat}
            disabled={busy !== null || preview}
          >
            {busy === "heartbeat" ? "扫描中…" : "扫描心跳目录（B）"}
          </button>
          <button className="ghost" onClick={() => void reload()} disabled={preview}>
            刷新
          </button>
        </div>
        {staticRep !== null && (
          <div className="note ok">
            静态登记完成：{staticRep.ecosystems} 个生态 · {staticRep.assets} 项资产
            · {staticRep.capability_rows} 条能力归属 · {staticRep.unassigned_assets}{" "}
            项资产无宿主标记（未归属，不计入分布图）
          </div>
        )}
        {hbRep !== null && (
          <div className={hbRep.hosts_registered > 0 ? "note ok" : "note"}>
            心跳目录 {hbRep.dir}：
            {hbRep.dir_exists
              ? "读到 " +
                hbRep.files_seen +
                " 个 .json，登记 " +
                hbRep.hosts_registered +
                " 个宿主"
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
      </section>

      {/* ② 哪些智能体拉了什么能力 */}
      <section className="card">
        <div className="card-head">
          <h3>② 哪些智能体拉了什么能力</h3>
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
        {usage.length > 0 && (
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
                  </span>
                </span>
              </li>
            ))}
          </ul>
        )}
        {usage.length > 120 && (
          <div className="muted small">
            仅显示前 120 条，共 {usage.length} 条
          </div>
        )}
      </section>

      {/* ③ 跨宿主来源分布图 */}
      <section className="card">
        <div className="card-head">
          <h3>③ 跨宿主来源分布</h3>
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
                <span className="bar-label">{d.ecosystem}</span>
                <span className="bar-track">
                  <span
                    className="bar-fill"
                    style={{ width: (d.capabilities / maxCap) * 100 + "%" }}
                  />
                </span>
                <span className="bar-n">
                  {d.capabilities} 能力 / {d.assets} 资产
                  {d.reported > 0 && <em> · 实报 {d.reported}</em>}
                </span>
              </li>))}
          </ul>
        )}
      </section>

      {/* 库内现状（复用已有统计） */}
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
            </div>))}
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

      {/* ④ 故障能力报错（紧凑版） */}
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

      <div className="muted small foot-note">
        A / B / C 三个通道的实报调用合计 {reportedTotal} 次。
        <br />
        C 通道的归属来自磁盘路径推断，因此调用次数恒为 0 ——
        <br />
        面板把它单独标出来，就是为了让「推断」和「实测」永远不会被混为一谈。
      </div>
    </div>
  );
}

function formatAge(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return s + " 秒";
  const m = Math.round(s / 60);
  if (m < 60) return m + " 分钟";
  const h = Math.round(m / 60);
  return h + " 小时";
}