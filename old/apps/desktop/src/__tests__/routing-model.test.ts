import { describe, expect, it } from "vitest";
import {
  EXECUTOR_ORDER,
  crossingCount,
  crossingGeometry,
  crossingsOf,
  groupByExecutor,
  implementationBrief,
} from "../routing-model";
import type { CapabilityTask } from "../panel-types";

/**
 * 模型分工的纯派生逻辑。
 *
 * 抽出来单独测的理由与 fleet-model 一致：**图必须真的从数据来**。
 * 布局与去重一旦混进 React 组件，就没法用最小数据钉住
 * 「没有数据就一根线都不画」这条纪律。
 */

function task(id: string, over: Partial<CapabilityTask> = {}): CapabilityTask {
  return {
    id,
    display_name: id,
    description: "",
    executor: "script",
    implementation: "",
    status: "implemented",
    depends_on: [],
    crosses_with: [],
    inputs: [],
    outputs: [],
    evidence: [],
    ...over,
  };
}

describe("groupByExecutor", () => {
  it("固定三组，顺序是脚本 → 混合 → 模型，空组也保留", () => {
    const groups = groupByExecutor([task("a")]);
    expect(groups.map((g) => g.executor)).toEqual(EXECUTOR_ORDER);
    expect(groups[0].tasks).toHaveLength(1);
    // 空组保留：界面上「模型 0 个」是有信息量的一句话，少一组会被读成漏渲染
    expect(groups[1].tasks).toHaveLength(0);
    expect(groups[2].tasks).toHaveLength(0);
  });

  it("脚本优先：三组配色可区分", () => {
    const [script, hybrid, model] = groupByExecutor([]).map((g) => g.color);
    expect(new Set([script, hybrid, model]).size).toBe(3);
  });
});

describe("crossingsOf", () => {
  it("对称声明去重成一条无向交叉", () => {
    const tasks = [
      task("a", { crosses_with: ["b"] }),
      task("b", { crosses_with: ["a"] }),
    ];
    expect(crossingsOf(tasks)).toEqual([{ a: "a", b: "b" }]);
  });

  it("不替单边交叉补另一半（那会掩盖内核的对称性缺陷）", () => {
    // 内核有 crossings_are_symmetric 测试保证对称；如果数据坏了，
    // 这里应当如实只报一条，而不是看起来「正常」地补成两条。
    const tasks = [task("a", { crosses_with: ["b"] }), task("b")];
    const got = crossingsOf(tasks);
    expect(got).toHaveLength(1);
    expect(got[0]).toEqual({ a: "a", b: "b" });
  });

  it("结果有序且稳定（截图可复现）", () => {
    const tasks = [
      task("z", { crosses_with: ["a"] }),
      task("a", { crosses_with: ["z", "m"] }),
      task("m", { crosses_with: ["a"] }),
    ];
    const got = crossingsOf(tasks).map((c) => c.a + ">" + c.b);
    expect(got).toEqual([...got].sort());
  });

  it("交叉计数两边各加一", () => {
    const tasks = [
      task("a", { crosses_with: ["b"] }),
      task("b", { crosses_with: ["a"] }),
    ];
    const n = crossingCount(tasks);
    expect(n.get("a")).toBe(1);
    expect(n.get("b")).toBe(1);
  });
});

describe("crossingGeometry", () => {
  it("没有交叉就没有曲线", () => {
    const g = crossingGeometry([task("a"), task("b")]);
    expect(g.links).toHaveLength(0);
    expect(g.boxes).toHaveLength(2);
  });

  it("交叉指向不在这份数据里的节点时，不画半截线", () => {
    const g = crossingGeometry([task("a", { crosses_with: ["ghost"] })]);
    expect(g.links).toHaveLength(0);
    expect(g.boxes).toHaveLength(1);
  });

  it("盒子按列排布：脚本列在最左，模型列在最右", () => {
    const g = crossingGeometry([
      task("s", { executor: "script" }),
      task("h", { executor: "hybrid" }),
      task("m", { executor: "model" }),
    ]);
    const byId = new Map(g.boxes.map((b) => [b.id, b]));
    expect(byId.get("s")!.x).toBeLessThan(byId.get("h")!.x);
    expect(byId.get("h")!.x).toBeLessThan(byId.get("m")!.x);
  });

  it("空数据时宽高为 0（不许凭空撑出一张画布）", () => {
    const g = crossingGeometry([]);
    expect(g.boxes).toHaveLength(0);
    expect(g.links).toHaveLength(0);
    expect(g.width).toBe(0);
    expect(g.height).toBe(0);
  });
});

describe("implementationBrief", () => {
  it("压成一行并截断，超长加省略号", () => {
    expect(implementationBrief("a\n b   c")).toBe("a b c");
    const long = "x".repeat(200);
    const brief = implementationBrief(long, 20);
    expect(brief).toHaveLength(21);
    expect(brief.endsWith("…")).toBe(true);
  });
});
