import type { AgentHost, HostProtocol } from "../panel-types";

/**
 * 右侧属性栏的「宿主详情」形态（总览分区 · 协议支持矩阵）。
 *
 * 用户原话：「协议支持矩阵……最后那个证据中显示的内容过多，导致了多行。
 * 所以需要把证据这一列中各个智能体对应的内容，显示到右侧的属性面板中，
 * 点哪一个智能体行就能看到右侧就能看到相应的证据显示」。
 *
 * 所以表格里的「证据」列缩成一句极短摘要（「3 条证据」），
 * **完整证据逐条搬到这里**：一条一行，原文照录，不截断、不合并、不概括。
 * 证据是这个项目里最重要的东西 —— 它回答「凭什么说检测到了」，
 * 在表格里被挤成一团正是这轮要修的问题。
 *
 * 与 {@link ../components/DetailPane} 的关系：复用同一套 .detail-body /
 * .kv / .chips / chip 样式，但**不塞进 DetailPane** —— 资产属性与宿主详情
 * 是两种完全不同的数据形状（manifest vs AgentHost），揉进一个组件会让
 * 两边都变成条件分支的垃圾场。App 按「当前选中了什么」分派。
 */

/** 支持状态的三档文案，与表格里的 ProtocolCell 共用同一套语义。 */
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

const KIND_LABEL: Record<string, string> = {
  plugin: "插件",
  cli: "命令行",
  mcp: "MCP",
  harness: "宿主",
};

export function HostDetail({ host }: { host: AgentHost }) {
  const evidence = (host.evidence ?? []).filter((e) => typeof e === "string");
  const protocols = (host.protocols ?? []).filter(
    (p): p is HostProtocol => p !== null && typeof p === "object",
  );

  return (
    <div className="detail-body host-detail" data-host-id={host.id}>
      <div className="detail-title">
        <span
          className={"tag host-kind " + (host.detected ? "detected" : "undetected")}
          title={
            host.detected
              ? "本机找到了该智能体的存在证据"
              : "本机没有找到该智能体的证据（不是错误，只是没装）"
          }
        >
          {KIND_LABEL[host.kind] ?? host.kind ?? "宿主"}
        </span>
        <h3>{host.display_name || host.id}</h3>
        <code className="detail-id">{host.id}</code>
      </div>

      <h4>状态</h4>
      <dl className="kv">
        <dt>厂商</dt>
        <dd>{host.vendor || "—（后端未提供）"}</dd>
        <dt>类型</dt>
        <dd>
          {KIND_LABEL[host.kind] ?? host.kind ?? "—"}
          {host.kind ? <span className="muted small"> · {host.kind}</span> : null}
        </dd>
        <dt>检测到</dt>
        <dd>
          <span className={host.detected ? "prov ready" : "muted"}>
            {host.detected ? "是（本机有证据）" : "否（本机未找到）"}
          </span>
        </dd>
        <dt>已连接</dt>
        <dd>
          <span className={host.connected ? "prov ready" : "muted"}>
            {host.connected ? "是（已接入中枢）" : "否（未接入中枢）"}
          </span>
        </dd>
        <dt>最后出现</dt>
        <dd>{formatUnix(host.last_seen)}</dd>
      </dl>

      <p className="muted small host-detail-note">
        「检测到」与「已连接」是两件事：前者只说明本机有这个智能体的痕迹，
        后者要求它真的把心跳或端点注册报给了中枢。判不出来一律「否」，
        原因写在下面的证据里。
      </p>

      {/* ── 证据：表格里那一列的全部内容都在这里，逐条原文 ── */}
      <h4>全部证据（{evidence.length} 条）</h4>
      {evidence.length === 0 ? (
        <p className="muted small">
          后端没有给出任何判定证据。这只说明注册表本轮没写 evidence 字段，
          不代表检测结果不可信 —— 但也无法在这里解释它。
        </p>
      ) : (
        <ol className="evidence-list">
          {evidence.map((e, i) => (
            <li key={`ev-${i}`}>{e}</li>
          ))}
        </ol>
      )}

      {/* ── 协议清单：每个协议的支持档位 + 传输方式 + 说明 ── */}
      <h4>协议清单（{protocols.length} 个）</h4>
      {protocols.length === 0 ? (
        <p className="muted small">
          该宿主没有声明任何协议（不是「不支持」，是注册表里没有这一项）。
        </p>
      ) : (
        <ul className="protocol-list">
          {protocols.map((p, i) => {
            const cls = SUPPORT_CLASS[p.support] ?? "sup-planned";
            return (
              <li key={`${p.name}-${i}`}>
                <div className="protocol-head">
                  <b>{p.name}</b>
                  <span className={"sup-cell " + cls}>
                    {SUPPORT_LABEL[p.support] ?? p.support}
                  </span>
                  {p.transport && <code className="protocol-transport">{p.transport}</code>}
                </div>
                {p.note ? (
                  <div className="protocol-note">{p.note}</div>
                ) : (
                  <div className="protocol-note muted">（该协议没有附加说明）</div>
                )}
              </li>
            );
          })}
        </ul>
      )}

      <p className="muted small host-detail-note">
        完整协议矩阵（智能体 × 协议）在左侧表格里；点任意一行切换到这里。
      </p>
    </div>
  );
}

/** unix 秒 → 本地时间。0 / 缺失 = 从未见过（不写成 1970 年）。 */
function formatUnix(sec: number | null | undefined): string {
  if (typeof sec !== "number" || !Number.isFinite(sec) || sec <= 0) {
    return "—（从未见过）";
  }
  try {
    return new Date(sec * 1000).toLocaleString();
  } catch {
    return String(sec);
  }
}
