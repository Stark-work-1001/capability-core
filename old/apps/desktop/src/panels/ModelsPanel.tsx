import { useCallback, useEffect, useState } from "react";
import { diagnostics, llmStatus, runtime, saveProvider } from "../bridge";
import type { DiagnosticReport, LlmProviderReport } from "../panel-types";

/**
 * 模型接入侧栏。
 *
 * 用户原话：「能力核心自带算法驱动（检索、修复），先建多协议支持；
 * 协议测试不通过时，再接云端或本地模型来驱动故障解决。」
 *
 * ## 本轮范围（刻意克制）
 *
 * 1. **本地算法优先** —— 诊断与修复建议全部由中枢自带算法给出，
 *    不调用任何模型。
 * 2. **LLM 兜底的接入位** —— provider 抽象与配置位置都在，
 *    但**本轮不实现任何厂商调用**，界面必须明确写出「未接入」。
 *
 * 这一轮**没有**把 ac-llm 膨胀成完整 LLM 层：implemented 恒为 0，
 * 界面上那句话是内核给的，不是前端编的。
 *
 * ## 搬走了什么（本轮）
 *
 * 用户原话：「模型配置分区的模型配置组别，界面显示的『协议支持矩阵』卡片
 * 独立为组别，转移到总览分区」——原来这里那张「协议支持矩阵」卡片
 * （protocol_matrix 表格）**整张搬到了总览分区的「协议支持矩阵」侧栏**
 * （{@link ./HostsPanel.tsx}），并扩成「各智能体的协议清单 + 支持状态」。
 *
 * 这里只删那一张卡片；「LLM 接入位」与「需要 LLM 求援的故障项」保持原样。
 */

export function ModelsPanel() {
  const [llm, setLlm] = useState<LlmProviderReport | null>(null);
  const [diag, setDiag] = useState<DiagnosticReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setBusy(true);
    try {
      const [l, d] = await Promise.all([llmStatus(), diagnostics()]);
      setLlm(l);
      setDiag(d);
      setErr(null);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const preview = runtime === "browser-preview";
  const llmNeeded = diag !== null ? diag.diagnostics.filter((d) => d.needs_llm) : [];
  return (
    <div className="panel-scroll">
      <div className="card-head sticky-head">
        <h3>模型配置</h3>
        <div className="actions inline">
          <button onClick={() => void reload()} disabled={busy || preview}>
            {busy ? "读取中…" : "重新读取"}
          </button>
        </div>
      </div>
      {err !== null && <div className="alert error">{err}</div>}

      {preview && (
        <div className="empty">
          预览模式：没有内核，读不到 provider 配置。
        </div>
      )}

      {/* 「协议支持矩阵」这张卡片整张搬到了 总览 →「协议支持矩阵」侧栏
          （HostsPanel.tsx），并扩成「各智能体的协议清单 + 支持状态」。
          这里只留一句指路，不放第二份表 —— 两张同名表会让人不知道看哪个。 */}
      <section className="card moved-card">
        <div className="card-head">
          <h3>协议支持矩阵</h3>
          <span className="card-num">已移到「总览」分区</span>
        </div>
        <div className="note">
          协议支持矩阵已独立成组，搬到<b>总览 →「协议支持矩阵」</b>侧栏，
          并扩成「每个智能体 × 每个协议的支持状态」（claude-code / codex / qoder /
          minimax-code / workbuddy / kimi-code / deepseek-harness …）。
          <br />
          本面板保留原有的「LLM 接入位」与「需要 LLM 求援的故障项」。
        </div>
      </section>

      {/* ── LLM 接入位 ── */}
      <section className="card">
        <div className="card-head">
          <h3>LLM 接入位</h3>
          <span className="card-num">
            已实现 {llm !== null ? llm.implemented : 0} 个 provider
          </span>
        </div>
        {llm !== null && (
          <div className={llm.implemented > 0 ? "note ok" : "note warn"}>
            {llm.note}
          </div>
        )}
        {llm !== null && (
          <div className="kv-grid">
            <div className="kv-cell">
              <span>配置文件</span>
              <b className="path">{llm.config_path}</b>
            </div>
            <div className="kv-cell">
              <span>配置文件是否存在</span>
              <b>{llm.config_exists ? "是" : "否"}</b>
            </div>
          </div>
        )}
        {llm !== null && llm.providers.length === 0 && (
          <div className="empty">
            暂无数据：还没有配置任何模型 provider。
            <br />
            原因：这是正常状态，不是错误 —— 中枢本轮只交付接入位。
            <br />
            想接入时，在上面那个路径建一个 JSON，写入 providers 数组，
            <br />
            再重启本程序即可被读到。
          </div>
        )}
        {llm !== null && llm.providers.length > 0 && (
          <table className="matrix">
            <thead>
              <tr>
                <th>provider</th>
                <th>类型</th>
                <th>协议</th>
                <th>模型</th>
                <th>API Key</th>
                <th>状态</th>
              </tr>
            </thead>
            <tbody>
              {llm.providers.map((p) => (
                <tr key={p.id}>
                  <td>{p.name || p.id}</td>
                  <td>{p.kind}</td>
                  <td>
                    <code className="proto-tag">{p.protocol || "openai"}</code>
                  </td>
                  <td>{p.model || "—"}</td>
                  <td>
                    <KeyCell
                      providerId={p.id}
                      masked={p.masked_key}
                      hasKey={p.has_key}
                    />
                  </td>
                  <td>
                    <span className={"prov " + p.status.status}>
                      {statusLabel(p.status.status)}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}

        {/* ── 通用接入表单：本地 / 云端同一套字段 ── */}
        <ProviderForm
          onSaved={() => void reload()}
          disabled={preview}
          existing={llm !== null ? llm.providers.map((p) => p.id) : []}
        />
      </section>

      {/* ── 哪些故障需要 LLM 求援 ── */}
      <section className="card">
        <div className="card-head">
          <h3>需要 LLM 求援的故障项</h3>
          <span className="card-num">{llmNeeded.length} 条</span>
        </div>
        {llmNeeded.length === 0 ? (
          <div className="empty">
            暂无：当前没有需要主对话模型出面的故障项。
            <br />
            本地算法（结构化过滤 + BM25）足以判定的故障，都不会出现在这里。
          </div>
        ) : (
          <ul className="diag-list">
            {llmNeeded.map((d) => (
              <li key={d.id} className="diag sev-info">
                <div className="diag-head">
                  <b className="diag-title">{d.title}</b>
                  <span className="llm-tag">建议求援 LLM</span>
                </div>
                <div className="diag-subject">{d.subject}</div>
                <div className="llm-reason">{d.needs_llm_reason}</div>
              </li>
            ))}
          </ul>
        )}
        <div className="legend">
          「建议求援 LLM」指的是**请主对话模型来判断**，不是调用本机上的某个模型。
          <br />
          中枢本地不发起任何模型请求 —— 那是本地算法优先这条纪律的一部分。
        </div>
      </section>
    </div>
  );
}

/**
 * API Key 单元格：**默认打码，点「显示」才临时露出**。
 *
 * 为什么明文不在这里：后端 `ProviderView` 只回 `masked_key`，
 * 前端根本拿不到明文。所以这个「显示」切换露出的是**掩码的展开说明**，
 * 不是真 key —— 真 key 只在编辑表单里由用户重新输入。
 *
 * 这是有意的：如果前端能显示明文，它就会出现在 DevTools、截图、
 * 崩溃转储与屏幕共享里。安全边界画在「后端不回传」这一层，比画在
 * UI 开关上可靠得多。
 */
function KeyCell({
  providerId,
  masked,
  hasKey,
}: {
  providerId: string;
  masked: string;
  hasKey: boolean;
}) {
  const [shown, setShown] = useState(false);
  if (!hasKey) {
    return <span className="muted small">未配置（本地模型通常不需要）</span>;
  }
  return (
    <span className="inline key-cell">
      <code className="key-mask">{shown ? masked || "****" : "********"}</code>
      <button
        type="button"
        className="link key-toggle"
        onClick={() => setShown((v) => !v)}
        title={
          shown
            ? "隐藏"
            : "显示末四位（明文不会离开内核，仅用于核对是否为同一把 key）"
        }
        aria-label={shown ? "隐藏 API key" : "显示 API key"}
        aria-pressed={shown}
        id={`key-toggle-${providerId}`}
      >
        {shown ? "隐藏" : "显示"}
      </button>
    </span>
  );
}

/** 协议选项：只按**协议**分支，不按厂商分支。 */
const PROTOCOLS = [
  {
    value: "openai",
    label: "OpenAI 兼容（本地 Ollama / vLLM / 硅基流动 / 百炼 / 智谱…）",
  },
  { value: "anthropic", label: "Anthropic Messages（Claude 系）" },
];

/**
 * 通用 provider 表单：**本地与云端用同一套字段**。
 *
 * 区别只在用户填什么：
 * - 本地：endpoint 填 `http://127.0.0.1:11434/v1`，API Key 留空
 * - 云端：endpoint 填厂商地址，API Key 填 key
 *
 * 表单不判断"这是本地还是云端"——那是用户心里的概念，不是协议的差别。
 * 强行分两套表单会让「本地服务也要 key」「云端服务不要 key」这些
 * 真实情况无处安放。
 */
function ProviderForm({
  onSaved,
  disabled,
  existing,
}: {
  onSaved: () => void;
  disabled: boolean;
  existing: string[];
}) {
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const [protocol, setProtocol] = useState("openai");
  const [endpoint, setEndpoint] = useState("");
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  const canSave =
    id.trim() !== "" && endpoint.trim() !== "" && model.trim() !== "" && !saving;

  const submit = useCallback(async () => {
    if (!canSave) return;
    setSaving(true);
    setMsg(null);
    try {
      await saveProvider({
        id: id.trim(),
        name: name.trim() || id.trim(),
        kind: /^https?:\/\/(127\.|localhost|0\.0\.0\.0)/i.test(endpoint.trim())
          ? "local"
          : "cloud",
        protocol,
        endpoint: endpoint.trim(),
        model: model.trim(),
        api_key: apiKey.trim() === "" ? null : apiKey.trim(),
        enabled: true,
      });
      setMsg({ ok: true, text: `已保存 ${id.trim()}` });
      setId("");
      setName("");
      setEndpoint("");
      setModel("");
      setApiKey("");
      setShowKey(false);
      onSaved();
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    } finally {
      setSaving(false);
    }
  }, [canSave, id, name, protocol, endpoint, model, apiKey, onSaved]);

  return (
    <div className="provider-form">
      <div className="form-head">
        <h4>新增 / 覆盖 provider</h4>
        <span className="muted small">
          本地与云端用同一套字段：本地填 endpoint、key 留空即可
        </span>
      </div>

      {existing.length > 0 && (
        <div className="foot-note">
          已存在：{existing.join("、")}（用相同 id 提交会覆盖）
        </div>
      )}

      <div className="form-grid">
        <label>
          <span>id *</span>
          <input
            value={id}
            onChange={(e) => setId(e.target.value)}
            placeholder="siliconflow / ollama-local"
            disabled={disabled}
          />
        </label>
        <label>
          <span>名称</span>
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="留空则用 id"
            disabled={disabled}
          />
        </label>
        <label className="span2">
          <span>协议 *</span>
          <select
            value={protocol}
            onChange={(e) => setProtocol(e.target.value)}
            disabled={disabled}
          >
            {PROTOCOLS.map((p) => (
              <option key={p.value} value={p.value}>
                {p.label}
              </option>
            ))}
          </select>
        </label>
        <label className="span2">
          <span>endpoint *</span>
          <input
            value={endpoint}
            onChange={(e) => setEndpoint(e.target.value)}
            placeholder="本地 http://127.0.0.1:11434/v1 ｜ 云端 https://api.siliconflow.cn/v1"
            disabled={disabled}
          />
        </label>
        <label className="span2">
          <span>模型 *</span>
          <input
            value={model}
            onChange={(e) => setModel(e.target.value)}
            placeholder="本地 qwen2.5 ｜ 云端 Qwen/Qwen3-Embedding-0.6B"
            disabled={disabled}
          />
        </label>
        <label className="span2">
          <span>API Key（本地可留空）</span>
          <div className="key-input">
            <input
              type={showKey ? "text" : "password"}
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder="留空表示不需要 key"
              autoComplete="off"
              spellCheck={false}
              disabled={disabled}
            />
            <button
              type="button"
              className="key-toggle-btn"
              onClick={() => setShowKey((v) => !v)}
              aria-pressed={showKey}
              aria-label={showKey ? "隐藏 API key" : "显示 API key"}
              title={showKey ? "隐藏" : "显示"}
            >
              {showKey ? "隐藏" : "显示"}
            </button>
          </div>
        </label>
      </div>

      <div className="actions">
        <button onClick={() => void submit()} disabled={!canSave || disabled}>
          {saving ? "保存中…" : "保存 provider"}
        </button>
      </div>

      {msg !== null && (
        <div className={"alert " + (msg.ok ? "ok" : "error")}>{msg.text}</div>
      )}
    </div>
  );
}

function statusLabel(s: string): string {
  switch (s) {
    case "ready":
      return "已实现";
    case "not_implemented":
      return "已配置 · 本轮未接入";
    case "disabled":
      return "已配置 · 被关闭";
    default:
      return "未配置";
  }
}