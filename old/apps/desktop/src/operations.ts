import { invoke } from "@tauri-apps/api/core";
import { isTauri } from "./bridge";
export async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri) throw new Error("浏览器预览：请在桌面程序中执行此操作");
  return invoke<T>(name, args);
}
export interface ImportRequest {
  id: string; kind: string; name: string; description: string; entry: string;
  files: Record<string, string>; author: string; repository: string; license: string;
}
export interface ImportPlan { operation_id: string; digest: string; asset_id: string; revision: string; files: string[]; bytes: number; warnings: string[]; state: string; }
export interface Workflow { id: string; title: string; requirement: string; steps: string[]; model_slot: string | null; implementation: string; input_hint: string; }
export interface WorkflowResult { id: string; workflow: string; status: string; result: Record<string, unknown>; }
export const workflowStatus: Record<string, string> = { completed: "已完成", needs_review: "待审核", needs_configuration: "未完成 · 模型未就绪", failed: "未完成 · 执行失败", blocked: "未完成 · 验收未通过" };
export const runWorkflow = (workflow: string, input: Record<string, unknown> = {}) => command<WorkflowResult>("run_workflow", { workflow, input });
export async function publishCapability(plan: ImportPlan): Promise<string> {
  const asset = await command<{id: string; kind: string}>("commit_capability", {operation: plan.operation_id, digest: plan.digest});
  if (asset.kind === "mcp") {
    try {
      const check = await runWorkflow("r3", {asset_id: asset.id});
      return check.status === "completed" ? "版本已发布并完成配置与依赖巡检；故障及维修记录可在维修站查看，实际运行仍需调用验收。" : `版本已发布；复检未完成：${JSON.stringify(check.result)}`;
    } catch (e) {return `版本已发布；复检未完成：${String(e)}`;}
  }
  return "版本已发布并注册；历史版本可回滚，实际运行仍需调用验收。";
}
