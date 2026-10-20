import { describe, expect, it } from "vitest";
import {
  COMBINED_PLUGIN_SUITE_ID,
  SECTIONS,
  assetKindOf,
  assetKindsOf,
  defaultSidebarOf,
  isAssetSidebar,
  sectionById,
  sectionOfSidebar,
  sidebarDef,
} from "../nav";
import { ASSET_KINDS } from "../types";

/**
 * 两级导航（分区 → 侧栏）的结构测试。
 *
 * 用户点名了每个分区下必须有哪些侧栏，这里逐条钉死：
 * 改结构时若漏掉某条侧栏，测试立刻红，而不是等人打开界面才发现。
 *
 * ## 本轮（第二轮）改了什么
 *
 * 1. 分区顺序：总览 → **资产** → 模型配置 → **新增能力** → **维修站**。
 *    用户要求「维修站分区及其侧栏分组移到新增能力分区下面」。
 * 2. 维修站多一条「去重」。
 * 3. 资产分区把 plugin + suite 合并成一条侧栏。
 * 4. 总览多一条「协议支持矩阵」。
 *
 * 原有用例的**断言意图一条没删**：分区数、每类资产必须有 list_assets、
 * live 必须有命令、id 全局唯一、分区↔侧栏可反查、assetKindOf 语义 —— 全部保留，
 * 只是数字与名称按用户新要求更新。
 */

describe("nav · 分区与侧栏结构", () => {
  it("五个分区齐全，且顺序为 总览 → 资产 → 模型配置 → 新增能力 → 维修站", () => {
    expect(SECTIONS.map((s) => s.id)).toEqual([
      "overview",
      "assets",
      "models",
      "create",
      "repair",
    ]);
    expect(SECTIONS.map((s) => s.label)).toEqual([
      "总览",
      "资产",
      "模型配置",
      "新增能力",
      "维修站",
    ]);
  });

  it("维修站分区排在新增能力分区**下面**（用户原话：整体移到下面）", () => {
    const ids = SECTIONS.map((s) => s.id);
    expect(ids.indexOf("repair")).toBeGreaterThan(ids.indexOf("create"));
    // 且维修站是最后一个分区，视觉上就是「新增能力」的下一组
    expect(ids[ids.length - 1]).toBe("repair");
  });

  it("总览分区：在线智能体 / 能力拉取 / 能力检索 / 协议支持矩阵", () => {
    const s = sectionById("overview");
    expect(s.sidebars.map((x) => x.label)).toEqual([
      "在线智能体",
      "能力拉取",
      "能力检索",
      "协议支持矩阵",
    ]);
    // 能力检索本轮只接了关键词，其余四种语义检索未接 —— 必须标 stub
    expect(sidebarDef("search").state).toBe("live");
    expect(sidebarDef("fleet").state).toBe("live");
    // 协议支持矩阵接的是新命令 list_agent_hosts（后端本轮接入）
    expect(sidebarDef("hosts").commands).toContain("list_agent_hosts");
  });

  it("维修站分区：故障清单 / 故障解决方案 / 故障修复 / 去重", () => {
    const s = sectionById("repair");
    expect(s.sidebars.map((x) => x.label)).toEqual([
      "故障清单",
      "故障解决方案",
      "故障修复",
      "去重",
    ]);
    expect(sidebarDef("fixes").state).toBe("live");
    // 去重侧栏接的是真实三命令
    expect(sidebarDef("dedup").state).toBe("live");
    expect(sidebarDef("dedup").commands).toEqual([
      "list_dedup_candidates",
      "decide_dedup",
      "dedup_history",
    ]);
  });

  it("资产分区：plugin 与 suite 合并为一条侧栏，其余 kind 各一条", () => {
    const s = sectionById("assets");
    // 合并前是 6 条（六类各一条），合并后 5 条
    expect(s.sidebars).toHaveLength(ASSET_KINDS.length - 1);
    expect(s.sidebars.map((x) => x.label)).toEqual([
      "Skill",
      "MCP",
      "Plugin / Suite",
      "Ghost",
      "Org",
    ]);
    // 合并栏确实同时覆盖两个 kind
    expect(assetKindsOf(COMBINED_PLUGIN_SUITE_ID)).toEqual(["plugin", "suite"]);
    // 每个侧栏仍然是 live 且必须带 list_assets —— 合并栏也不例外
    for (const sb of s.sidebars) {
      expect(isAssetSidebar(sb.id)).toBe(true);
      expect(sb.state).toBe("live");
      expect(sb.commands).toContain("list_assets");
    }
    // 每个 kind（含被合并的两个）都恰好被某一条侧栏覆盖一次
    const covered = s.sidebars.flatMap((sb) => assetKindsOf(sb.id));
    expect(covered.sort()).toEqual(ASSET_KINDS.map((k) => k.key).sort());
  });

  it("模型配置分区：模型分工 / 模型配置", () => {
    const s = sectionById("models");
    expect(s.sidebars.map((x) => x.label)).toEqual(["模型分工", "模型配置"]);
    // 模型分工本轮接上了真实命令 list_capability_tasks（内核 ac_registry::tasks）。
    // 仍要保持诚实：「任务 → provider」的分派没接，dataPath 必须写明这一点。
    expect(sidebarDef("routing").state).toBe("live");
    expect(sidebarDef("routing").commands).toContain("list_capability_tasks");
    expect(sidebarDef("routing").dataPath).toContain("工作流");
    expect(sidebarDef("providers").state).toBe("live");
  });

  it("新增能力分区：预览与提交有真实命令", () => {
    const s = sectionById("create");
    expect(s.sidebars).toHaveLength(1);
    expect(sidebarDef("new-capability").state).toBe("live");
    expect(sidebarDef("new-capability").commands).toContain("commit_capability");
  });

  it("每个侧栏都写明数据通路，且 live 侧栏必定有命令", () => {
    for (const section of SECTIONS) {
      expect(section.sidebars.length).toBeGreaterThan(0);
      for (const sb of section.sidebars) {
        expect(sb.dataPath.length).toBeGreaterThan(0);
        expect(sb.hint.length).toBeGreaterThan(0);
        if (sb.state === "live") {
          expect(sb.commands.length).toBeGreaterThan(0);
        }
      }
    }
  });

  it("侧栏 id 全局唯一（避免切换时命中错的那条）", () => {
    const ids = SECTIONS.flatMap((s) => s.sidebars.map((x) => x.id));
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("分区 ↔ 侧栏互相可反查，默认侧栏落在第一条", () => {
    expect(sectionOfSidebar("fleet").id).toBe("overview");
    expect(sectionOfSidebar("asset:mcp").id).toBe("assets");
    expect(sectionOfSidebar(COMBINED_PLUGIN_SUITE_ID).id).toBe("assets");
    expect(sectionOfSidebar("dedup").id).toBe("repair");
    expect(sectionOfSidebar("hosts").id).toBe("overview");
    expect(sectionOfSidebar("providers").id).toBe("models");
    for (const s of SECTIONS) {
      expect(defaultSidebarOf(s.id)).toBe(s.sidebars[0].id);
      expect(sectionOfSidebar(s.sidebars[0].id).id).toBe(s.id);
    }
  });

  it("assetKindOf：非资产侧栏返回 null，合并栏也返回 null（多 kind 用 assetKindsOf）", () => {
    expect(assetKindOf("fleet")).toBeNull();
    expect(assetKindOf("dedup")).toBeNull();
    expect(assetKindOf("asset:skill")).toBe("skill");
    expect(assetKindOf("asset:organization")).toBe("organization");
    // 合并栏有两个 kind，单值 API 不撒谎地返回 null
    expect(assetKindOf(COMBINED_PLUGIN_SUITE_ID)).toBeNull();
    expect(assetKindsOf(COMBINED_PLUGIN_SUITE_ID)).toEqual(["plugin", "suite"]);
    expect(assetKindsOf("asset:skill")).toEqual(["skill"]);
    expect(assetKindsOf("fleet")).toEqual([]);
  });
});
