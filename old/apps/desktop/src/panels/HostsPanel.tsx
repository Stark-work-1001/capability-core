import { ProtocolCatalog } from "../components/ProtocolCatalog";
import { useCallback, useEffect, useState } from "react";
import {
  isMissingCommandError,
  listAgentHosts,
  protocolMatrix,
  runtime,
} from "../bridge";
import type { AgentHost, HostProtocol } from "../panel-types";
import type { ProtocolMatrix } from "../panel-types";

/**
 * 行选中回调：把整条 {@link AgentHost} 交给上层（App），
 * 由 App 决定塞进右侧属性栏 —— 列表只负责「选中了谁」。
 */
export type HostSelectHandler = (host: AgentHost) => void;

/**
 * 总览分区 ·「协议支持矩阵」侧栏。
 *
 * 用户原话：「模型配置分区的模型配置组别，界面显示的『协议支持矩阵』卡片
 * 独立为组别，转移到总览分区，用于将智能体协议清单罗列所支持的智能体，
 * 包括但不限于 claude-code、codex、qoder、minimax-code、workbuddy、
 * kimi-code、deepseek-harness 等智能体、协议及支持状态」。
 *
 * ## 两张表，两个数据源，都不许造假
 *
 * 1. **智能体 × 协议矩阵** —— 来自新命令 `list_agent_hosts`
 *    （kernel `hosts.rs` + `agent_hosts` 表，schema v8）。
 *    一个智能体一行，每个协议一格，标 supported / partial / planned。
 * 2. **协议兼容性检查** —— 从 ModelsPanel 搬过来的那张卡片，
 *    数据仍是 `protocol_matrix`（真的能用）。
 *
 * ## 后端没就绪时的样子（重要）
 *
 * `list_agent_hosts` 本轮才接入。命令还没注册时 {@link listAgentHosts} 会抛
 * 「Command ... not found」。这时界面显示
 * 「后端尚未提供 list_agent_hosts（本轮正在接入）」**加一句说明**，
 * 表格一行都不画 —— 绝不拿内置清单伪造一张看起来已经工作的表。
 */

const SUPPORT_LABEL: Record<string, string> = {
  supported: "支持",
  partial: "部分",
  planned: "计划",
};

const SUPPORT_CLASS: Record<string, string> = {
  supported: "sup-ok",
  partial: "sup-partial",
  planned: "sup-planned",
};

const PROTOCOL_LABEL: Record<string, string> = {
  skill_md: "SKILL.md（frontmatter）",
  plugin_json: "plugin.json",
  mcp_json: ".mcp.json",
};

const KIND_LABEL: Record<string, string> = {
  plugin: "插件",
  cli: "命令行",
  mcp: "MCP",
  harness: "宿主",
};

export interface HostsPanelProps {
  /**
   * 点某一行 → 选中该智能体，右侧属性栏显示它的完整证据。
   *
   * 证据列在表格里已经缩成「N 条」摘要：完整内容进了属性栏，
   * 表格不再被一条条长证据撑成多行。
   */
  onSelect?: HostSelectHandler;
  /** 当前选中行的 id：该行高亮（与资产列表的 .active 同一套视觉语言）。 */
  selectedId?: string | null;
}

export function HostsPanel({ onSelect, selectedId = null }: HostsPanelProps = {}) {
  const [hosts, setHosts] = useState<AgentHost[]>([]);
  const [hostsErr, setHostsErr] = useState<string | null>(null);
  const [hostsMissing, setHostsMissing] = useState(false);
  const [hostsLoaded, setHostsLoaded] = useState(false);
  const [matrix, setMatrix] = useState<ProtocolMatrix | null>(null);
  const [loading, setLoading] = useState(false);

  const loadHosts = useCallback(async () => {
    setLoading(true);
    try {
      const list = await listAgentHosts();
      setHosts(list);
      setHostsErr(null);
      setHostsMissing(false);
    } catch (e) {
      setHosts([]);
      setHostsErr(String(e));
      setHostsMissing(isMissingCommandError(e));
    } finally {
      setHostsLoaded(true);
      setLoading(false);
    }
  }, []);

  const loadMatrix = useCallback(async () => {
    try {
      setMatrix(await protocolMatrix());
    } catch {
      // 矩阵拿不到就留 null：界面上不显示这张表，而不是显示 0
      setMatrix(null);
    }
  }, []);

  useEffect(() => {
    void loadHosts();
    void loadMatrix();
  }, [loadHosts, loadMatrix]);

  const preview = runtime === "browser-preview";

  /**
   * 协议列：所有宿主声明过的协议名的并集（保序）。
   * 这样不同宿主支持的协议集合不一样时，表格仍然是完整的笛卡尔视图。
   */
  const protocolNames: string[] = (() => {
    const out: string[] = [];
    for (const h of hosts) {
      for (const p of h.protocols ?? []) {
        if (p && p.name && !out.includes(p.name)) out.push(p.name);
      }
    }
    return out;
  })();

  const detectedCount = hosts.filter((h) => h.detected).length;
  const connectedCount = hosts.filter((h) => h.connected).length;

  return (
    <div className="panel-scroll hosts-panel">
      <ProtocolCatalog />
      <div className="card-head sticky-head">
        <h3>协议支持矩阵</h3>
        <div className="actions inline">
          <span className="card-num">
            {hostsLoaded ? hosts.length + " 个智能体" : "读取中…"}
          </span>
          <button className="link" onClick={() => void loadHosts()} disabled={loading}>
            {loading ? "读取中…" : "刷新"}
          </button>
        </div>
      </div>

      {preview && (
        <div className="empty">
          预览模式：没有 Tauri 宿主，读不到智能体协议注册表。
          <br />
          这里不填占位行 —— 一张假的「全部支持」表比空着更糟。
        </div>
      )}

      {/* ── 后端未就绪：明说缺什么，不伪造表格 ── */}
      {!preview && hostsMissing && (
        <div className="empty warn-empty">
          <b>后端尚未提供 list_agent_hosts（本轮正在接入）</b>
          <br />
          这个侧栏要罗列的 claude-code / codex / qoder / minimax-code /
          workbuddy / kimi-code / deepseek-harness 等智能体及其协议状态，
          <br />
          全部来自内核的智能体协议注册表（kernel `hosts.rs` + `agent_hosts` 表，
          schema v8），本轮正在由后端一并接入。
          <br />
          <br />
          界面在这里**保持空格**：不拿内置清单在前端渲染成一张「已经检测过」的表，
          <br />
          因为「检测到 / 已连接」这两列的真假必须来自后端实测，不能由界面臆断。
          <br />
          出错原文：<code>{hostsErr}</code>
        </div>
      )}

      {/* ── 其他错误（命令在，但调用失败）── */}
      {!preview && !hostsMissing && hostsErr !== null && (
        <div className="alert error">
          读取 list_agent_hosts 失败：{hostsErr}
        </div>
      )}

      {/* ── 有数据：才是真正的矩阵 ── */}
      {!preview && hosts.length > 0 && (
        <>
          <div className="kv-grid three">
            <div className="kv-cell">
              <span>登记智能体</span>
              <b>{hosts.length}</b>
            </div>
            <div className="kv-cell fix-auto">
              <span>本机检测到</span>
              <b>{detectedCount}</b>
            </div>
            <div className="kv-cell">
              <span>已连接中枢</span>
              <b>{connectedCount}</b>
            </div>
          </div>

          <table className="matrix hosts-matrix">
            <thead>
              <tr>
                <th>智能体</th>
                <th>厂商</th>
                <th>类型</th>
                <th className="num">检测到</th>
                <th className="num">已连接</th>
                {protocolNames.map((p) => (
                  <th key={p} className="proto-col">
                    {p}
                  </th>
                ))}
                {/* 证据列**只放摘要**：「3 条证据」。
                    完整证据（逐条原文）在右侧属性栏里，点这一行就能看到 ——
                    原来把 evidence 全量 join("；") 塞进单元格，长证据会把
                    整行撑成很多行，表格根本读不动。 */}
                <th>证据</th>
              </tr>
            </thead>
            <tbody>
              {hosts.map((h) => {
                const evCount = (h.evidence ?? []).length;
                const active = selectedId === h.id;
                return (
                <tr
                  key={h.id}
                  className={
                    (h.detected ? "" : "unsupported ") +
                    "host-row" +
                    (active ? " active" : "") +
                    (onSelect ? " is-clickable" : "")
                  }
                  onClick={onSelect ? () => onSelect(h) : undefined}
                  aria-selected={onSelect ? active : undefined}
                  tabIndex={onSelect ? 0 : undefined}
                  onKeyDown={
                    onSelect
                      ? (e) => {
                          if (e.key !== "Enter" && e.key !== " ") return;
                          e.preventDefault();
                          onSelect(h);
                        }
                      : undefined
                  }
                  title={onSelect ? "点这一行 → 右侧显示该智能体的全部证据" : undefined}
                >
                  <td>
                    <b>{h.display_name || h.id}</b>
                    <br />
                    <code className="host-id">{h.id}</code>
                  </td>
                  <td>{h.vendor || "—"}</td>
                  <td>{KIND_LABEL[h.kind] || h.kind || "—"}</td>
                  <td className="num">{h.detected ? "是" : "否"}</td>
                  <td className="num">
                    <span className={h.connected ? "prov ready" : "muted small"}>
                      {h.connected ? "是" : "否"}
                    </span>
                  </td>
                  {protocolNames.map((name) => {
                    const p = (h.protocols ?? []).find((x) => x.name === name);
                    return (
                      <td key={name} className="proto-cell">
                        <ProtocolCell protocol={p} />
                      </td>
                    );
                  })}
                  {/* 摘要而不是全量：完整内容在右侧属性栏。
                      没有证据 ≠ 没有检测结果，所以写「0 条」而不是一个破折号。 */}
                  <td className="reason evidence-cell">
                    {evCount === 0 ? (
                      <span className="muted small">0 条</span>
                    ) : (
                      <span
                        className="evidence-count"
                        title="完整证据已移到右侧属性栏：点这一行查看"
                      >
                        {evCount} 条
                      </span>
                    )}
                  </td>
                </tr>
                );
              })}
            </tbody>
          </table>

          <div className="legend">
            点任意一行 → 右侧属性栏显示该智能体的完整信息与逐条证据。
            <br />
            <b>支持</b> = 已验证可用；<b>部分</b> = 部分可用 / 需额外配置；
            <b>计划</b> = 计划支持但本轮没有实现。
            <br />
            「不支持」与「还没做」含义完全不同，所以用三档而不是一个是非勾。
            <br />
            「检测到」= 本机找到证据（插件目录 / CLI / 已知路径）；
            「已连接」= 已接入中枢（心跳或端点注册）。判不出来一律「否」，
            判定原因（证据）在右侧属性栏里逐条列出：点任意一行即可查看。
          </div>
        </>
      )}

      {!preview && hostsLoaded && hosts.length === 0 && !hostsMissing && hostsErr === null && (
        <div className="empty">
          暂无数据：内核返回了 0 个智能体宿主。
          <br />
          原因：注册表的首次启动 seed 尚未执行，或 hosts 目录还没有任何清单。
        </div>
      )}

      {/* ── 从「模型配置」搬来的协议兼容性矩阵（原有数据源，仍然真实可用）── */}
      <section className="card" id="protocol-compat">
        <div className="card-head">
          <h3>协议兼容性检查</h3>
          <span className="card-num">protocol_matrix</span>
        </div>
        {matrix === null ? (
          <div className="empty">
            暂无数据：读不到 protocol_matrix 的结果。
          </div>
        ) : (
          <>
            <div className="kv-grid four">
              <div className="kv-cell">
                <span>受检资产</span>
                <b>{matrix.total_assets}</b>
              </div>
              <div className="kv-cell">
                <span>受检能力</span>
                <b>{matrix.total_capabilities}</b>
              </div>
              <div className="kv-cell fix-auto">
                <span>通过</span>
                <b>{matrix.total_passed}</b>
              </div>
              <div className="kv-cell sev-error">
                <span>不通过</span>
                <b>{matrix.total_failed}</b>
              </div>
            </div>
            <table className="matrix">
              <thead>
                <tr>
                  <th>协议</th>
                  <th className="num">支持</th>
                  <th className="num">资产</th>
                  <th className="num">能力</th>
                  <th className="num">解析成功率</th>
                  <th>主要原因</th>
                </tr>
              </thead>
              <tbody>
                {matrix.rows.map((r) => (
                  <tr key={r.protocol} className={r.supported ? "" : "unsupported"}>
                    <td>{PROTOCOL_LABEL[r.protocol] || r.protocol}</td>
                    <td className="num">{r.supported ? "是" : "否"}</td>
                    <td className="num">{r.assets}</td>
                    <td className="num">{r.capabilities}</td>
                    <td className="num">
                      <span
                        className={
                          "rate " +
                          (r.capabilities === 0
                            ? "rate-none"
                            : r.success_rate >= 1
                              ? "rate-ok"
                              : "rate-bad")
                        }
                      >
                        {r.capabilities === 0
                          ? "无样本"
                          : Math.round(r.success_rate * 100) + "%"}
                      </span>
                    </td>
                    <td className="reason">
                      {r.top_reasons.length === 0 ? "—" : r.top_reasons.join("；")}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            <div className="legend">
              「解析成功率」的分母是<b>能力条目数</b>，不是资产数；
              一个协议没有能力样本时显示「无样本」而不是 100% —— 没有样本不等于全部通过。
            </div>
          </>
        )}
      </section>
    </div>
  );
}

/** 一个协议格。没有该协议 → 明确写「未声明」，不画一个空格让人误读成支持。 */
function ProtocolCell({ protocol }: { protocol: HostProtocol | undefined }) {
  if (!protocol) {
    return <span className="muted small sup-none">未声明</span>;
  }
  const cls = SUPPORT_CLASS[protocol.support] ?? "sup-planned";
  return (
    <span
      className={"sup-cell " + cls}
      title={
        (protocol.transport ? "传输：" + protocol.transport : "") +
        (protocol.note ? (protocol.transport ? " · " : "") + protocol.note : "")
      }
    >
      {SUPPORT_LABEL[protocol.support] ?? protocol.support}
    </span>
  );
}
