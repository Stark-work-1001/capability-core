import { describe, expect, it } from "vitest";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { normalizeManifest } from "../types";

/**
 * 第 1 层 · 一次性数据体检（替代穷举点击 506 条）
 *
 * 对库中全部 manifest 跑一遍归一化函数，逐条断言：
 *   ① 不抛异常
 *   ② provides / relations / description / identity.dimensions 全部非 undefined
 *
 * 覆盖面 **大于** 点击 506 条：点击只能验证被点到的那几条，
 * 这里验证的是**所有数据形态**，而且是毫秒级完成。
 *
 * 数据源由 tools 导出：见 artifacts/reports/ui-normalize-sweep.json 的 source 字段。
 */

const here = dirname(fileURLToPath(import.meta.url));
// src/__tests__ → src → desktop → apps → capability-core，共 4 级上溯
const ROOT = resolve(here, "../../../../");
const SWEEP_IN = resolve(ROOT, "artifacts/sweep-20261017-122117/manifests.json");
const REPORT = resolve(ROOT, "artifacts/reports/ui-normalize-sweep.json");

interface Exported {
  id: string;
  kind: string;
  name: string;
  parse_error: string | null;
  manifest: unknown;
}

const rows = JSON.parse(readFileSync(SWEEP_IN, "utf8")) as Exported[];

describe("第 1 层 · 全库归一化体检", () => {
  it("数据源非空且是全量（506 条）", () => {
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBe(506);
  });

  it("库内 manifest 全部是合法 JSON（解析层就无破损）", () => {
    const bad = rows.filter((r) => r.parse_error !== null);
    expect(bad).toEqual([]);
  });

  it("506 条逐条归一化：无异常、关键字段全部非 undefined", () => {
    const failures: {
      id: string;
      kind: string;
      stage: string;
      error: string;
    }[] = [];
    let providesAbsent = 0;
    let relationsPresent = 0;
    let emptyDescription = 0;

    for (const row of rows) {
      try {
        const n = normalizeManifest(row.manifest as never);
        if (n.provides === undefined)
          failures.push({ id: row.id, kind: row.kind, stage: "provides", error: "undefined" });
        if (n.relations === undefined)
          failures.push({ id: row.id, kind: row.kind, stage: "relations", error: "undefined" });
        if (n.description === undefined)
          failures.push({ id: row.id, kind: row.kind, stage: "description", error: "undefined" });
        if (n.identity?.dimensions === undefined)
          failures.push({ id: row.id, kind: row.kind, stage: "identity.dimensions", error: "undefined" });
        if (!Array.isArray(n.provides))
          failures.push({ id: row.id, kind: row.kind, stage: "provides", error: "not array" });
        if (!Array.isArray(n.relations))
          failures.push({ id: row.id, kind: row.kind, stage: "relations", error: "not array" });

        if (n.provides.length === 0) providesAbsent++;
        if (n.relations.length > 0) relationsPresent++;
        if (n.description === "") emptyDescription++;
      } catch (e) {
        failures.push({
          id: row.id,
          kind: row.kind,
          stage: "throw",
          error: String(e instanceof Error ? e.message : e),
        });
      }
    }

    // 汇总报告落盘，即使断言失败也留下证据
    mkdirSync(dirname(REPORT), { recursive: true });
    writeFileSync(
      REPORT,
      JSON.stringify(
        {
          generated_at: "2026-10-17",
          method: "第 1 层 · 数据层穷举归一化（不点击 UI）",
          source: SWEEP_IN,
          total: rows.length,
          ok: rows.length - new Set(failures.map((f) => f.id)).size,
          failed: new Set(failures.map((f) => f.id)).size,
          failures,
          shape_stats: {
            zero_capability_assets: providesAbsent,
            assets_with_relations: relationsPresent,
            empty_description: emptyDescription,
          },
          asserts: [
            "无一条抛异常",
            "provides 非 undefined 且为数组",
            "relations 非 undefined 且为数组",
            "description 非 undefined",
            "identity.dimensions 非 undefined",
          ],
        },
        null,
        2,
      ),
      "utf8",
    );

    expect(failures).toEqual([]);
  });
});
