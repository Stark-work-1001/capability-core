import { WorkflowWorkbench } from "../components/WorkflowWorkbench";
import { useCallback, useEffect, useMemo, useState } from "react";
import { isMissingCommandError, listCapabilityTasks, runtime } from "../bridge";
import type { CapabilityTask, TaskCatalog, TaskExecutor } from "../panel-types";
import {
  EXECUTOR_COLOR,
  EXECUTOR_LABEL,
  STATUS_CLASS,
  STATUS_LABEL,
  crossingCount,
  crossingGeometry,
  groupByExecutor,
  implementationBrief,
} from "../routing-model";

/**
 * 模型配置分区 ·「模型分工」侧栏。
 *
 * ## 这一栏以前是空的，现在为什么不是
 *
 * 上一轮的注释写着「内核没有任务 → 模型的分派命令」。那句话在**分派**这一层
 * 至今仍然成立：\`ac-llm\` 只有 \`LlmProvider\` trait，没有任何厂商实现，
 * 所以「某个任务走某个 provider」确实无从谈起。
 *
 * 但用户要的第一件事不是分派，是**分工的判定**：
 * 「这些能力不调用模型的话，能不能用脚本实现；能实现就实现，不能实现才接模型」。
 * 这份判定现在由内核 \`ac_registry::tasks\` 给出，命令是 \`list_capability_tasks\`：
 * 25 个任务节点，每个都标了执行者、状态、依赖、交叉与判定证据。
 *
 * ## 三条不造假的纪律
 *
 * 1. **没数据就明说**：命令未注册 / 预览模式 / 读失败，各自显示不同的原因，
 *    一个节点都不画。绝不内置一份「示例路由表」——那是最容易被当成真的假数据。
 * 2. **状态三档照实显示**：已实现 / 部分 / 计划。模型型节点在当前内核里
 *    全部是「计划中」，界面上不许把它们渲染成已接线。
 * 3. **交叉线只画真实声明过的**：\`crosses_with\` 由内核保证双向对称，
 *    前端不补、不猜、不为了好看硬连两条无关的线。
 */

/** 某个节点被哪个需求认领（r1…r6 → 标题），列表与属性栏都用它。 */
function requirementIndex(catalog: TaskCatalog): Map<string, string> {
  const m = new Map<string, string>();
  for (const r of catalog.requirements ?? []) {
    for (const id of r.task_ids ?? []) m.set(id, r.id + " " + r.title);
  }
  return m;
}

export function RoutingPanel() {
  const [catalog, setCatalog] = useState<TaskCatalog | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [missing, setMissing] = useState(false);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const c = await listCapabilityTasks();
      setCatalog(c);
      setErr(null);
      setMissing(false);
    } catch (e) {
      setCatalog(null);
      setErr(String(e));
      setMissing(isMissingCommandError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const preview = runtime === "browser-preview";

  const tasks = catalog?.tasks ?? [];
  const groups = useMemo(() => groupByExecutor(tasks), [tasks]);
  const crossCounts = useMemo(() => crossingCount(tasks), [tasks]);
  const reqOf = useMemo(
    () => (catalog ? requirementIndex(catalog) : new Map<string, string>()),
    [catalog],
  );
  const selectedTask = useMemo(
    () => tasks.find((t) => t.id === selected) ?? null,
    [tasks, selected],
  );
  const byId = useMemo(() => new Map(tasks.map((t) => [t.id, t])), [tasks]);
  const graph = useMemo(() => crossingGeometry(tasks), [tasks]);

  const counts = catalog?.counts;

  return (
    <div className="panel-scroll routing-panel">
      <WorkflowWorkbench />
      <div className="card-head sticky-head">
        <h3>模型分工</h3>
        <div className="actions inline">
          <span className="card-num">
            {loading ? "读取中…" : counts ? counts.total + " 个任务节点" : "未读到数据"}
          </span>
          <button className="link" onClick={() => void load()} disabled={loading}>
            {loading ? "读取中…" : "刷新"}
          </button>
        </div>
      </div>

      {/* ── 预览模式：没有内核，也就没有这张表 ── */}
      {preview && (
        <div className="empty warn-empty">
          <b>浏览器预览模式：读不到任务路由表。</b>
          <br />
          这张表的每个节点、每条依赖、每处交叉都来自内核的静态目录
          （<code>ac_registry::tasks</code>）。前端不复制一份副本 ——
          <br />
          副本一旦与内核漂移，界面上看到的「分工」就不再是内核实际执行的那一份，
          而这种偏差没人看得出来。
        </div>
      )}

      {/* ── 命令未注册：明说缺什么 ── */}
      {!preview && missing && (
        <div className="empty warn-empty">
          <b>后端尚未提供 list_capability_tasks。</b>
          <br />
          内核模块 <code>ac_registry::tasks</code> 已经就绪（25 个节点、六项需求映射、
          <br />
          交叉对称性都由测试钉住），但对应的 Tauri 命令还没注册到这个版本里。
          <br />
          <br />
          界面在这里**保持空白**：不内置一份「示例分工表」冒充真实数据。
          <br />
          出错原文：<code>{err}</code>
        </div>
      )}

      {/* ── 其他失败（命令在，但调用失败）── */}
      {!preview && !missing && err !== null && (
        <div className="alert error">读取 list_capability_tasks 失败：{err}</div>
      )}

      {/* ── 有数据：概览 ── */}
      {catalog !== null && (
        <>
          <div className="kv-grid four">
            <div className="kv-cell">
              <span>任务节点</span>
              <b>{counts?.total ?? 0}</b>
            </div>
            <div className="kv-cell fix-auto">
              <span>脚本 / 算法</span>
              <b>{counts?.script ?? 0}</b>
            </div>
            <div className="kv-cell">
              <span>混合</span>
              <b>{counts?.hybrid ?? 0}</b>
            </div>
            <div className="kv-cell sev-warning">
              <span>模型</span>
              <b>{counts?.model ?? 0}</b>
            </div>
          </div>

          <div className="routing-note">
            {catalog.note}
            <br />
            已实现 <b>{counts?.implemented ?? 0}</b> · 部分实现{" "}
            <b>{counts?.partial ?? 0}</b> · 计划中 <b>{counts?.planned ?? 0}</b>
            （脚本型占节点数 {Math.round((catalog.script_share ?? 0) * 100)}%，
            这是**节点数**占比，不是工作量占比）。
          </div>

          {/* ── 六项需求 → 节点 ── */}
          <section className="card">
            <div className="card-head">
              <h3>六项需求 → 任务节点</h3>
              <span className="card-num">{catalog.requirements?.length ?? 0} 条</span>
            </div>
            <ul className="req-list">
              {(catalog.requirements ?? []).map((r) => (
                <li key={r.id} className="req-row">
                  <div className="req-head">
                    <span className="req-id">{r.id}</span>
                    <b>{r.title}</b>
                    <span className="req-n">{r.task_ids.length} 个节点</span>
                  </div>
                  <div className="req-quote">「{r.statement}」</div>
                  <div className="req-note">{r.note}</div>
                  <div className="chips">
                    {r.task_ids.map((id) => (
                      <button
                        key={id}
                        className="chip req-chip"
                        onClick={() => setSelected(id)}
                        title={byId.get(id)?.description ?? id}
                      >
                        {byId.get(id)?.display_name ?? id}
                      </button>
                    ))}
                  </div>
                </li>
              ))}
            </ul>
          </section>

          {/* ── 交叉关系图 ── */}
          <section className="card">
            <div className="card-head">
              <h3>任务交叉关系</h3>
              <span className="card-num">{graph.links.length} 条交叉</span>
            </div>
            {graph.boxes.length === 0 ? (
              <div className="empty">内核返回了 0 个任务节点，画不出交叉图。</div>
            ) : (
              <>
                <div className="routing-graph-wrap">
                  <svg
                    className="routing-graph"
                    width={graph.width}
                    height={graph.height}
                    role="img"
                    aria-label="任务交叉关系图"
                  >
                    {graph.links.map((l) => (
                      <path
                        key={l.a + "->" + l.b}
                        className={
                          "routing-link" +
                          (selected === l.a || selected === l.b ? " is-hot" : "")
                        }
                        d={l.d}
                      />
                    ))}
                    {graph.boxes.map((b) => {
                      const t = byId.get(b.id);
                      const hot = selected === b.id;
                      return (
                        <g
                          key={b.id}
                          className={"routing-box" + (hot ? " is-hot" : "")}
                          transform={"translate(" + b.x + "," + b.y + ")"}
                          onClick={() => setSelected(b.id)}
                          role="button"
                          tabIndex={0}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ") setSelected(b.id);
                          }}
                        >
                          <rect
                            width={b.w}
                            height={b.h}
                            rx={6}
                            fill={EXECUTOR_COLOR[b.executor] + "1f"}
                            stroke={EXECUTOR_COLOR[b.executor]}
                            strokeWidth={hot ? 2 : 1}
                          />
                          <text x={8} y={b.h / 2 + 4} className="routing-box-label">
                            {b.label}
                          </text>
                          <title>
                            {(t?.display_name ?? b.id) +
                              "（" +
                              EXECUTOR_LABEL[b.executor] +
                              " · " +
                              (STATUS_LABEL[t?.status ?? "planned"] ?? "") +
                              "）"}
                          </title>
                        </g>
                      );
                    })}
                  </svg>
                </div>
                <div className="legend">
                  三列 = 脚本 / 混合 / 模型；每条曲线对应一对真实声明过的
                  <code>crosses_with</code>。交叉是**双向**的（内核有测试钉住对称性），
                  所以图里不画箭头。点盒子可看详情。
                </div>
              </>
            )}
          </section>

          {/* ── 按执行者分组的三张清单 ── */}
          {groups.map((g) => (
            <section className="card" key={g.executor}>
              <div className="card-head">
                <h3>
                  <span className="exec-dot" style={{ background: g.color }} />
                  {g.label}
                </h3>
                <span className="card-num">{g.tasks.length} 个</span>
              </div>
              {g.tasks.length === 0 ? (
                <div className="empty">
                  {g.executor === "model"
                    ? "本轮没有纯模型节点 —— 三个需要模型的地方都是「脚本做不了的那一段」，所以归类在混合里。"
                    : "这一组为空。"}
                </div>
              ) : (
                <ul className="task-list">
                  {g.tasks.map((t) => (
                    <li
                      key={t.id}
                      className={
                        "task-row" + (selected === t.id ? " is-selected" : "")
                      }
                      onClick={() => setSelected(t.id)}
                    >
                      <div className="task-main">
                        <span
                          className={"exec-badge exec-" + t.executor}
                          title={EXECUTOR_LABEL[t.executor]}
                        >
                          {t.executor === "script"
                            ? "脚本"
                            : t.executor === "model"
                              ? "模型"
                              : "混合"}
                        </span>
                        <b className="task-name">{t.display_name}</b>
                        <span className={"task-status " + (STATUS_CLASS[t.status] ?? "")}>
                          {STATUS_LABEL[t.status] ?? t.status}
                        </span>
                        <code className="task-id">{t.id}</code>
                      </div>
                      <div className="task-impl">{implementationBrief(t.implementation)}</div>
                      <div className="task-meta">
                        {reqOf.get(t.id) && <span className="task-req">{reqOf.get(t.id)}</span>}
                        <span>
                          依赖 {t.depends_on.length} · 交叉{" "}
                          {crossCounts.get(t.id) ?? 0}
                        </span>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          ))}

          {/* ── 选中节点的完整属性（列表下方，不依赖右侧属性栏） ── */}
          {selectedTask !== null && (
            <TaskDetail
              task={selectedTask}
              requirement={reqOf.get(selectedTask.id) ?? ""}
              crossings={crossingCount(tasks).get(selectedTask.id) ?? 0}
              byId={byId}
              onClose={() => setSelected(null)}
            />
          )}
        </>
      )}
    </div>
  );
}

/** 一个任务节点的完整信息。字段全部原样来自内核，不做二次解释。 */
function TaskDetail({
  task,
  requirement,
  crossings,
  byId,
  onClose,
}: {
  task: CapabilityTask;
  requirement: string;
  crossings: number;
  byId: Map<string, CapabilityTask>;
  onClose: () => void;
}) {
  const nameOf = (id: string) => byId.get(id)?.display_name ?? id;
  const executor: TaskExecutor = task.executor;
  return (
    <section className="card task-detail">
      <div className="card-head">
        <h3>
          <span className="exec-dot" style={{ background: EXECUTOR_COLOR[executor] }} />
          {task.display_name}
        </h3>
        <div className="actions inline">
          <span className="card-num">{task.id}</span>
          <button className="link" onClick={onClose}>
            收起
          </button>
        </div>
      </div>

      <div className="chips">
        <span className={"exec-badge exec-" + executor}>{EXECUTOR_LABEL[executor]}</span>
        <span className={"task-status " + (STATUS_CLASS[task.status] ?? "")}>
          {STATUS_LABEL[task.status] ?? task.status}
        </span>
        {requirement && <span className="chip">{requirement}</span>}
        <span className="chip">交叉 {crossings}</span>
      </div>

      <p className="detail-desc">{task.description}</p>

      <h4>谁执行 / 怎么做</h4>
      <p className="detail-desc">{task.implementation}</p>

      {task.script_entry && task.script_entry.length > 0 && (
        <>
          <h4>可直接调用的内核函数</h4>
          <div className="chips">
            {task.script_entry.map((f) => (
              <code className="chip chip-code" key={f}>
                {f}
              </code>
            ))}
          </div>
          <p className="muted small">
            这些函数在 kernel 源码里真实存在，由内核测试
            <code>every_script_entry_resolves_to_a_real_function</code> 逐个核对。
          </p>
        </>
      )}

      {task.model_contract && (
        <>
          <h4>模型契约（为什么脚本做不了）</h4>
          <p className="detail-desc">{task.model_contract.why_not_script}</p>
          <h4>需要的模型能力</h4>
          <div className="chips">
            {task.model_contract.capabilities.map((c) => (
              <span className="chip" key={c}>
                {c}
              </span>
            ))}
          </div>
          <h4>输入</h4>
          <pre className="contract">{task.model_contract.input_schema}</pre>
          <h4>输出</h4>
          <pre className="contract">{task.model_contract.output_schema}</pre>
        </>
      )}

      <h4>输入</h4>
      <ul className="plain-list">
        {task.inputs.map((x) => (
          <li key={x}>{x}</li>
        ))}
      </ul>

      <h4>输出</h4>
      <ul className="plain-list">
        {task.outputs.map((x) => (
          <li key={x}>{x}</li>
        ))}
      </ul>

      <h4>依赖</h4>
      {task.depends_on.length === 0 ? (
        <p className="muted small">无前置依赖：这是链路起点。</p>
      ) : (
        <div className="chips">
          {task.depends_on.map((id) => (
            <span className="chip" key={id} title={id}>
              {nameOf(id)}
            </span>
          ))}
        </div>
      )}

      <h4>交叉</h4>
      {task.crosses_with.length === 0 ? (
        <p className="muted small">与其它节点无交叉。</p>
      ) : (
        <div className="chips">
          {task.crosses_with.map((id) => (
            <span className="chip" key={id} title={id}>
              {nameOf(id)}
            </span>
          ))}
        </div>
      )}

      <h4>判定证据（为什么是这一档执行者）</h4>
      <ul className="plain-list">
        {task.evidence.map((x) => (
          <li key={x}>{x}</li>
        ))}
      </ul>
    </section>
  );
}
