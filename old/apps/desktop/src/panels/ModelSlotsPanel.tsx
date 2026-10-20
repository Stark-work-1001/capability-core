import { useCallback, useEffect, useState } from "react";
import { modelSlots, runtime, saveModelSlot, testModelSlot } from "../bridge";
import {
  MODEL_SLOTS,
  SILICONFLOW_ENDPOINT,
  type ModelSlotConfig,
  type ModelSlotName,
  type ModelSlotProbeResult,
  type ModelSlotView,
} from "../panel-types";

/**
 * 模型配置分区 ·「模型配置」侧栏：三槽位接入。
 *
 * 用户原话：「云端模型使用 siliconflow / 模型API地址 ... / 填入 devel 版本
 * 进行相关配置和调用」，并给出三张参考图（memora 的模型配置界面）：
 * 一个向量模型、一个 reranker 模型、一个通用（判定）模型。
 *
 * ## 三条设计约束
 *
 * 1. **密钥永不出内核**。界面只拿 `masked_key`（`****mqunn`）与 `has_key`。
 *    输入框留空 = 不改动已保存的 key；输入内容 = 覆盖；点「清空」= 删除。
 *    如果把掩码当新值提交，用户的 key 会被一坨星号覆盖 —— 这是最坏的一种
 *    "看起来成功了"的失败。
 *
 * 2. **「测试连接」真的发请求**。它调 `test_model_slot`，内核那边真的
 *    打一次云端 API，返回延迟 + 摘要（维度/条数/回复）。只校验字段的
 *    假测试会让用户在 key 配错时以为配对了。
 *
 * 3. **预设只填地址与模型名，不填 key**。点「硅基流动 SiliconFlow」
 *    只改 endpoint 与 model，密钥必须用户自己填或走环境变量 ——
 *    预设里带 key 等于把凭据写进产品。
 */

interface SlotState {
  view: ModelSlotView;
  /** 用户正在编辑的字段（受控） */
  draft: ModelSlotConfig;
  /** 输入框里的新密钥。空串 = 不改动（与 null 语义区分见提交时处理） */
  keyInput: string;
  /** 是否点了「清空密钥」 */
  clearKey: boolean;
  /** 测试结果 */
  probe: ModelSlotProbeResult | null;
  /** 保存/测试中的忙碌标记 */
  busy: "save" | "test" | null;
  /** 本槽位的错误（保存失败/测试抛错） */
  err: string | null;
  /** 是否有未保存改动 */
  dirty: boolean;
}

function toDraft(v: ModelSlotView): ModelSlotConfig {
  return {
    slot: v.slot,
    enabled: v.enabled,
    mode: v.mode,
    endpoint: v.endpoint,
    model: v.model,
    api_key_env: v.api_key_env,
    api_key: null, // null = 不改动
    timeout_ms: v.timeout_ms,
    input_template: v.input_template,
    output_schema: v.output_schema,
  };
}

export function ModelSlotsPanel() {
  const [slots, setSlots] = useState<Record<string, SlotState>>({});
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const views = await modelSlots();
      const next: Record<string, SlotState> = {};
      for (const v of views) {
        next[v.slot] = {
          view: v,
          draft: toDraft(v),
          keyInput: "",
          clearKey: false,
          probe: null,
          busy: null,
          err: null,
          dirty: false,
        };
      }
      setSlots(next);
      setErr(null);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const patch = (slot: ModelSlotName, fn: (s: SlotState) => SlotState) => {
    setSlots((cur) => (cur[slot] ? { ...cur, [slot]: fn(cur[slot]) } : cur));
  };

  const edit = (slot: ModelSlotName, key: keyof ModelSlotConfig, value: unknown) => {
    patch(slot, (s) => ({
      ...s,
      draft: { ...s.draft, [key]: value },
      dirty: true,
      // 改了配置就清掉上一次的测试结果 —— 旧结论对新配置无效，
      // 留着会让用户以为新配置已经验证过了。
      probe: null,
    }));
  };

  const applyPreset = (slot: ModelSlotName) => {
    const meta = MODEL_SLOTS.find((m) => m.slot === slot)!;
    patch(slot, (s) => ({
      ...s,
      draft: {
        ...s.draft,
        mode: "cloud-openai",
        endpoint: SILICONFLOW_ENDPOINT,
        model: meta.presetModel,
        enabled: true,
        input_template: slot === "embedding" ? "qwen3-embedding" : s.draft.input_template ?? "",
        output_schema: slot === "judge" ? "json_schema" : s.draft.output_schema ?? "",
      },
      dirty: true,
      probe: null,
    }));
  };

  const doSave = async (slot: ModelSlotName) => {
    patch(slot, (s) => ({ ...s, busy: "save", err: null }));
    try {
      const s = slots[slot];
      // 三态密钥：
      // - clearKey → 提交空串（内核语义 = 清空）
      // - 输入了内容 → 提交它
      // - 都没动 → null（不改动）
      const apiKey = s.clearKey ? "" : s.keyInput.trim() !== "" ? s.keyInput.trim() : null;
      await saveModelSlot({ ...s.draft, api_key: apiKey });
      await load();
    } catch (e) {
      patch(slot, (st) => ({ ...st, err: String(e), busy: null }));
      return;
    }
    patch(slot, (st) => ({ ...st, busy: null }));
  };

  const doTest = async (slot: ModelSlotName) => {
    patch(slot, (s) => ({ ...s, busy: "test", err: null, probe: null }));
    try {
      const r = await testModelSlot(slot);
      patch(slot, (s) => ({ ...s, probe: r, busy: null }));
    } catch (e) {
      patch(slot, (s) => ({ ...s, err: String(e), busy: null }));
    }
  };

  const preview = runtime === "browser-preview";

  return (
    <div className="panel-scroll">
      <div className="card-head sticky-head">
        <h3>模型配置 · 十个专项槽位</h3>
        <div className="actions inline">
          <button onClick={() => void load()} disabled={loading || preview}>
            {loading ? "读取中…" : "重新读取"}
          </button>
        </div>
      </div>

      <div className="note">
        基础模型与专项模型分工：<b>Embedding</b> 负责向量化、<b>Judge</b> 负责判定分类、
        <b>Rerank</b> 负责重排。对接的是 <b>OpenAI 兼容协议</b>（硅基流动 / 百炼 / 智谱 /
        DeepSeek / vLLM / Ollama 的 <code>/v1</code> 端点都走同一套），
        新增诊断、修复、去重、生产、验收、备用和本地向量槽位。未指定的模型保持留空，专项任务可回退到通用槽位。
      </div>

      {err !== null && <div className="alert error">{err}</div>}
      {preview && (
        <div className="empty">预览模式：没有内核，读不到模型槽位配置。</div>
      )}

      {!preview &&
        MODEL_SLOTS.map((meta) => {
          const s = slots[meta.slot];
          if (!s) return null;
          const v = s.view;
          return (
            <section className="card slot-card" key={meta.slot}>
              <div className="card-head">
                <h3>{meta.title}</h3>
                <span className="card-num">
                  {v.has_key ? (
                    <span className="slot-key-ok">已配置 · {v.masked_key}</span>
                  ) : (
                    <span className="slot-key-none">未配置密钥</span>
                  )}
                </span>
              </div>
              <div className="slot-hint">{meta.hint}</div>

              <div className="slot-grid">
                <label>
                  <span>接入方式</span>
                  <select
                    value={s.draft.mode}
                    onChange={(e) => edit(meta.slot, "mode", e.target.value)}
                  >
                    <option value="cloud-openai">云端 OpenAI 兼容</option>
                    <option value="local-openai">本地 OpenAI 兼容</option>
                    <option value="disabled">停用</option>
                  </select>
                </label>

                <label>
                  <span>启用</span>
                  <select
                    value={s.draft.enabled ? "yes" : "no"}
                    onChange={(e) => edit(meta.slot, "enabled", e.target.value === "yes")}
                  >
                    <option value="yes">启用</option>
                    <option value="no">停用</option>
                  </select>
                </label>
              </div>

              {s.draft.mode === "cloud-openai" && (
                <div className="slot-warn">
                  云端：请求会离开本机，凭据由服务端保管且**永不回传前端**。
                </div>
              )}

              <label className="slot-wide">
                <span>API 地址（含 /v1）</span>
                <input
                  value={s.draft.endpoint}
                  onChange={(e) => edit(meta.slot, "endpoint", e.target.value)}
                  placeholder="https://api.siliconflow.cn/v1"
                  spellCheck={false}
                />
              </label>

              <label className="slot-wide">
                <span>模型名称</span>
                <input
                  value={s.draft.model}
                  onChange={(e) => edit(meta.slot, "model", e.target.value)}
                  placeholder={meta.presetModel}
                  spellCheck={false}
                />
              </label>

              <div className="slot-grid">
                <label>
                  <span>密钥环境变量名（可选）</span>
                  <input
                    value={s.draft.api_key_env}
                    onChange={(e) => edit(meta.slot, "api_key_env", e.target.value)}
                    placeholder="服务端调用时从这个环境变量读密钥"
                    spellCheck={false}
                  />
                </label>

                <label>
                  <span>超时（毫秒）</span>
                  <input
                    type="number"
                    value={s.draft.timeout_ms}
                    onChange={(e) => edit(meta.slot, "timeout_ms", Number(e.target.value) || 30000)}
                  />
                </label>
              </div>

              <label className="slot-wide">
                <span>字面密钥（服务端保存，永不回传）</span>
                <input
                  type="password"
                  value={s.keyInput}
                  onChange={(e) =>
                    patch(meta.slot, (st) => ({
                      ...st,
                      keyInput: e.target.value,
                      clearKey: false,
                      probe: null,
                    }))
                  }
                  placeholder={
                    v.has_key ? "留空表示不改动已保存的密钥" : "填入 API Key（仅保存到本机配置）"
                  }
                  spellCheck={false}
                />
              </label>
              {v.has_key && (
                <div className="slot-key-row">
                  <button
                    type="button"
                    className="link"
                    onClick={() =>
                      patch(meta.slot, (st) => ({ ...st, clearKey: true, keyInput: "" }))
                    }
                  >
                    清空已保存的密钥
                  </button>
                  {s.clearKey && (
                    <span className="slot-key-warn">
                      已标记清空：保存后该槽位将没有密钥（下次保存生效）
                    </span>
                  )}
                </div>
              )}

              <div className="slot-preset">
                <span>服务商预设（只填地址与模型名，不含密钥）：</span>
                <button type="button" onClick={() => applyPreset(meta.slot)}>
                  硅基流动 SiliconFlow
                </button>
                <div className="slot-preset-note">
                  硅基流动：{SILICONFLOW_ENDPOINT} · {meta.presetModel}
                </div>
              </div>

              {meta.slot === "embedding" && (
                <label className="slot-wide">
                  <span>输入模板</span>
                  <select
                    value={s.draft.input_template || "qwen3-embedding"}
                    onChange={(e) => edit(meta.slot, "input_template", e.target.value)}
                  >
                    <option value="qwen3-embedding">qwen3-embedding</option>
                    <option value="openai-generic">openai-generic</option>
                  </select>
                </label>
              )}

              {meta.slot === "judge" && (
                <label className="slot-wide">
                  <span>结构化输出</span>
                  <select
                    value={s.draft.output_schema || "json_schema"}
                    onChange={(e) => edit(meta.slot, "output_schema", e.target.value)}
                  >
                    <option value="json_schema">json_schema</option>
                    <option value="text">text</option>
                  </select>
                </label>
              )}

              <div className="actions">
                <button
                  onClick={() => void doSave(meta.slot)}
                  disabled={s.busy !== null || !s.dirty}
                >
                  {s.busy === "save" ? "保存中…" : s.dirty ? "保存" : "已保存"}
                </button>
                <button
                  onClick={() => void doTest(meta.slot)}
                  disabled={s.busy !== null || preview || !v.callable}
                  title={v.callable ? "真实调用一次该模型" : "先保存并启用该槽位"}
                >
                  {s.busy === "test" ? "测试中…" : "测试连接"}
                </button>
              </div>

              {s.err !== null && <div className="alert error">{s.err}</div>}

              {s.probe !== null && (
                <div className={s.probe.ok ? "note ok" : "alert error"}>
                  {s.probe.ok ? (
                    <>
                      连接成功 · {s.probe.elapsed_ms} ms · {s.probe.summary}
                    </>
                  ) : (
                    <>
                      连接失败（{s.probe.elapsed_ms} ms）：{s.probe.error}
                    </>
                  )}
                </div>
              )}
            </section>
          );
        })}

      {!preview && !loading && Object.keys(slots).length === 0 && (
        <div className="empty">
          暂无数据：内核没有返回任何槽位。
          <br />
          正常情况下应该固定返回三个（embedding / judge / rerank），即使是未配置态 ——
          一个都不返回说明命令或内核有问题，不是"没有模型"。
        </div>
      )}
    </div>
  );
}
