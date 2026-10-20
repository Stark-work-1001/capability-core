import { describe, expect, it } from "vitest";
import { normalizeManifest } from "../types";
import type { CapabilityManifest } from "../types";

/**
 * normalizeManifest 参数化单测。
 *
 * 这是上一轮 506 条点击验证的**长期替代品**：点击只覆盖被点到的那几条，
 * 这里把每一种畸形形态都钉成一个独立断言，改前端后秒级回归。
 */

const good: CapabilityManifest = {
  schema_version: "1.0",
  id: "skill::demo",
  kind: "skill",
  name: "示例技能",
  version: "0.1.0",
  description: "一个完整形态的示例",
  identity: {
    role: "writer",
    authority: "write",
    side_effect: "local",
    tool_posture: "pure",
    idempotent: true,
    open_world: false,
    dimensions: { 层: "L3", 域: "ops" },
  },
  provides: [
    { capability_id: "demo.cap", name: "示例能力", summary: "摘要", tags: ["Read"] },
  ],
  relations: [{ src_id: "skill::demo", kind: "related", target_id: "plugin::other" }],
  provenance: {
    source_path: "vendor/demo/SKILL.md",
    imported_at: "2026-10-17",
    content_hash: "abc",
    source_format: "skill_md",
  },
};

describe("normalizeManifest · 完整输入", () => {
  const n = normalizeManifest(good);
  it("保留全部已填字段", () => {
    expect(n.id).toBe("skill::demo");
    expect(n.kind).toBe("skill");
    expect(n.name).toBe("示例技能");
    expect(n.description).toBe("一个完整形态的示例");
    expect(n.identity.authority).toBe("write");
    expect(n.identity.dimensions).toEqual({ 层: "L3", 域: "ops" });
    expect(n.provides).toHaveLength(1);
    expect(n.relations).toHaveLength(1);
    expect(n.provenance.source_path).toBe("vendor/demo/SKILL.md");
  });
  it("provides/relations 归一化后非 undefined", () => {
    expect(n.provides).not.toBeUndefined();
    expect(n.relations).not.toBeUndefined();
    expect(n.identity.dimensions).not.toBeUndefined();
    expect(n.description).not.toBeUndefined();
  });
});

/** 每种畸形输入都必须：① 不抛 ② 关键字段非 undefined */
const malformed: [string, unknown][] = [
  ["null", null],
  ["undefined", undefined],
  ["完全空对象", {}],
  ["provides 键缺失（本轮 506 条里的 7 条零能力资产）", {
    schema_version: "1.0", id: "x::y", kind: "skill", name: "零能力",
    description: "没有 provides 键", identity: {},
  }],
  ["provides 为空数组", { id: "x::y", kind: "skill", provides: [] }],
  ["provides 为 null", { id: "x::y", kind: "skill", provides: null }],
  ["provides 塞非对象（字符串/数字/null 混杂）", {
    id: "x::y", kind: "skill",
    provides: ["不是对象", 42, null, undefined, { capability_id: "ok.cap", name: "正常能力" }],
  }],
  ["relations 键缺失", { id: "x::y", kind: "skill" }],
  ["relations 为 null", { id: "x::y", kind: "skill", relations: null }],
  ["relations 塞非对象", { id: "x::y", kind: "skill", relations: [null, 7, "bad"] }],
  ["relations 缺字段", { id: "x::y", kind: "skill", relations: [{}] }],
  ["identity 为 null", { id: "x::y", kind: "skill", identity: null }],
  ["identity 为字符串", { id: "x::y", kind: "skill", identity: "我是字符串" }],
  ["identity 字段类型不符", {
    id: "x::y", kind: "skill",
    identity: { role: 123, authority: [], idempotent: "yes", open_world: 1, dimensions: "不是对象" },
  }],
  ["dimensions 值非字符串", {
    id: "x::y", kind: "skill", identity: { dimensions: { a: 1, b: true, c: null, d: "ok" } },
  }],
  ["顶层是数组", [1, 2, 3]],
  ["顶层是字符串", "我是字符串"],
  ["顶层是数字", 12345],
];

describe("normalizeManifest · 畸形输入（每种一条独立断言）", () => {
  it.each(malformed)("%s 不抛异常且补齐关键字段", (_label, input) => {
    let n!: ReturnType<typeof normalizeManifest>;
    expect(() => { n = normalizeManifest(input as CapabilityManifest); }).not.toThrow();
    expect(n.provides).toBeDefined();
    expect(n.relations).toBeDefined();
    expect(n.identity.dimensions).toBeDefined();
    expect(n.provenance).toBeDefined();
    expect(n.description).toBeDefined();
    expect(typeof n.description).toBe("string");
    expect(Array.isArray(n.provides)).toBe(true);
    expect(Array.isArray(n.relations)).toBe(true);
  });
});

describe("normalizeManifest · 具体降级取值", () => {
  it("null → 全部字段仍存在且带可读默认值", () => {
    const n = normalizeManifest(null);
    expect(n.id).toBe("（未知 ID）");
    expect(n.kind).toBe("unknown");
    expect(n.name).toBe("（未命名资产）");
    expect(n.provides).toEqual([]);
    expect(n.relations).toEqual([]);
    expect(n.identity.authority).toBe("unknown");
    expect(n.identity.role).toBeNull();
    expect(n.provenance.source_path).toBeNull();
  });

  it("完全空对象 → 同上，不抛", () => {
    const n = normalizeManifest({});
    expect(n.provides).toEqual([]);
    expect(n.relations).toEqual([]);
    expect(n.identity.dimensions).toEqual({});
  });

  it("缺 provides 的零能力资产 → provides 为空数组而非 undefined", () => {
    const n = normalizeManifest({ id: "z::a", kind: "skill", name: "零能力" } as never);
    expect(n.provides).toEqual([]);
    expect(Array.isArray(n.provides)).toBe(true);
  });

  it("数组里塞非对象 → 非对象条目被丢弃，只留合法能力项（不产生幽灵能力）", () => {
    const n = normalizeManifest({
      provides: ["str", 42, null, [1, 2], { capability_id: "k.cap", name: "保留", tags: ["Read"] }],
    } as never);
    expect(n.provides).toHaveLength(1);
    expect(n.provides[0].capability_id).toBe("k.cap");
    expect(n.provides[0].tags).toEqual(["Read"]);
  });

  it("relations 塞非对象 → 同样被丢弃，不产生空 src_id 的幽灵关系", () => {
    const n = normalizeManifest({
      relations: [null, "bad", 7, [1], { src_id: "a", kind: "rel", target_id: "b" }],
    } as never);
    expect(n.relations).toHaveLength(1);
    expect(n.relations[0].src_id).toBe("a");
  });

  it("tags 里混非字符串 → 被过滤掉", () => {
    const n = normalizeManifest({
      provides: [{ capability_id: "c", tags: ["Read", 1, null, "Write"] }],
    } as never);
    expect(n.provides[0].tags).toEqual(["Read", "Write"]);
  });

  it("relations 缺 kind → 补成 related，不留 undefined", () => {
    const n = normalizeManifest({ relations: [{ src_id: "a", target_id: "b" }] } as never);
    expect(n.relations).toHaveLength(1);
    expect(n.relations[0].kind).toBe("related");
    expect(n.relations[0].src_id).toBe("a");
  });

  it("booleans 只认 true，字符串 'true' 不算", () => {
    const n = normalizeManifest({
      identity: { idempotent: "true", open_world: true },
    } as never);
    expect(n.identity.idempotent).toBe(false);
    expect(n.identity.open_world).toBe(true);
  });

  it("dimensions 非字符串值被转成字符串，不丢键", () => {
    const n = normalizeManifest({
      identity: { dimensions: { n: 1, b: true, s: "x" } },
    } as never);
    expect(n.identity.dimensions.n).toBe("1");
    expect(n.identity.dimensions.b).toBe("true");
    expect(n.identity.dimensions.s).toBe("x");
  });

  it("幂等：归一化结果再归一化，结果不变", () => {
    const once = normalizeManifest(good);
    const twice = normalizeManifest(once as never);
    expect(twice).toEqual(once);
  });
});
