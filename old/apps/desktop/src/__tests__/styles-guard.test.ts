import { describe, expect, it } from "vitest";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

/**
 * 样式守门：每个用到的 class 必须在 styles.css 里有对应选择器。
 *
 * ## 为什么要有这条测试（真实事故）
 *
 * 2026-10-18 本轮开发中，给 RoutingPanel 追加样式时用 read 读了 styles.css
 * 再整文件写回。read 工具默认只返回前 2000 行，于是 56745 字节的文件被写回成
 * 45124 字节 —— 整个「M3.5 新外壳」CSS 段（fleet-* / hosts-matrix / sup-cell /
 * edition-badge / asset-filters 等）被静默删掉。
 *
 * 更糟的是：tsc / vitest / vite build 全部通过。因为「className 指向一个不存在的
 * CSS 选择器」在类型上完全合法，构建也不会报错 —— 界面只是安静地变成无样式。
 *
 * 这条测试补的就是这个洞：把「样式覆盖」从肉眼检查变成机械断言。
 *
 * ## 提取策略（只认 className 表达式内部，且只认会被输出的字面量）
 *
 *   className="a b c"                   -> a, b, c
 *   className={"a" + (x ? " b" : "")}   -> a, b（跳过比较运算里的 "x"）
 *   className={`a ${on ? "b" : ""}`}    -> a, b
 *
 * 两条刻意的排除：
 *   1. **比较运算的另一侧不是 class**。`mode === "graph" ? "active" : ""` 里
 *      "graph" 是状态值，"active" 才是 class。不排除的话，每个三元都会贡献一堆假阳性。
 *   2. **以 - 结尾的片段是动态前缀**（`"src src-" + u.source`）。它不是 class，
 *      但它暗示存在 `.src-mcp` 这样的具体变体 —— 所以单独收集前缀，
 *      要求 styles.css 里**至少有一个**以该前缀开头的选择器。这比直接跳过更严格：
 *      前缀一个变体都没有，说明那处拼接根本没有样式。
 */

const SRC = join(__dirname, "..");
const CSS_PATH = join(SRC, "styles.css");

/**
 * 故意没有样式规则的 class：**只准是语义挂钩**（测试 / 埋点用），
 * 不准用它来绕过「漏写样式」。每条都要写清理由。
 */
const KNOWN_UNSTYLED = new Map<string, string>([]);

function listSourceFiles(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    if (name === "__tests__" || name === "node_modules") continue;
    const p = join(dir, name);
    if (statSync(p).isDirectory()) listSourceFiles(p, out);
    else if (/\.(tsx|ts)$/.test(name)) out.push(p);
  }
  return out;
}

/** 从 openIdx 处的 { 找到配对的 }，返回其后一位（考虑字符串与模板）。 */
function matchBrace(src: string, openIdx: number): number {
  let depth = 0;
  let i = openIdx;
  const DQ = String.fromCharCode(34);
  const SQ = String.fromCharCode(39);
  const BT = String.fromCharCode(96);
  while (i < src.length) {
    const ch = src[i];
    if (ch === DQ || ch === SQ) {
      i++;
      while (i < src.length && src[i] !== ch) {
        if (src[i] === String.fromCharCode(92)) i++;
        i++;
      }
      i++;
      continue;
    }
    if (ch === BT) {
      i++;
      while (i < src.length && src[i] !== BT) {
        if (src[i] === String.fromCharCode(92)) i++;
        i++;
      }
      i++;
      continue;
    }
    if (ch === "{") depth++;
    else if (ch === "}") {
      depth--;
      if (depth === 0) return i + 1;
    }
    i++;
  }
  return src.length;
}

export interface Extraction {
  /** 静态 class 名 */
  classes: Set<string>;
  /** 以 - 结尾的动态前缀（如 src- ），要求至少有一个具体变体 */
  prefixes: Set<string>;
}

/** 去掉比较运算右侧的字符串字面量（那是状态值，不是 class）。 */
function stripComparisonLiterals(expr: string): string {
  return expr
    .replace(/[!=]==?\s*"[^"]*"/g, " CMP ")
    .replace(/[!=]==?\s*'[^']*'/g, " CMP ")
    .replace(/"([^"]*)"\s*[!=]==?/g, " CMP ")
    .replace(/'([^']*)'\s*[!=]==?/g, " CMP ");
}

function wordsFromExpression(rawExpr: string, classes: Set<string>, prefixes: Set<string>): void {
  const expr = stripComparisonLiterals(rawExpr);
  const push = (raw: string) => {
    for (const tok of raw.split(/\s+/)) {
      if (tok.endsWith("-") && /^[a-zA-Z][\w-]*-$/.test(tok)) {
        prefixes.add(tok);
      } else if (/^[a-zA-Z][\w-]*$/.test(tok)) {
        classes.add(tok);
      }
    }
  };
  for (const m of expr.matchAll(/"([^"]*)"|'([^']*)'/g)) push(m[1] ?? m[2] ?? "");
  for (const m of expr.matchAll(/`([^`]*)`/g)) {
    const body = m[1];
    push(body.replace(/\$\{[\s\S]*?\}/g, " "));
    for (const inner of body.matchAll(/\$\{([\s\S]*?)\}/g)) {
      const cleaned = stripComparisonLiterals(inner[1]);
      for (const lit of cleaned.matchAll(/"([^"]*)"|'([^']*)'/g)) push(lit[1] ?? lit[2] ?? "");
    }
  }
}

/** 提取一段源码里所有 className 表达式的静态 class 与动态前缀。 */
export function extractClassNames(source: string): Extraction {
  const classes = new Set<string>();
  const prefixes = new Set<string>();
  const re = /className\s*=\s*/g;
  const DQ = String.fromCharCode(34);
  const SQ = String.fromCharCode(39);
  let m: RegExpExecArray | null;
  while ((m = re.exec(source)) !== null) {
    const i = m.index + m[0].length;
    const ch = source[i];
    if (ch === DQ || ch === SQ) {
      const end = source.indexOf(ch, i + 1);
      if (end > i) {
        wordsFromExpression(source.slice(i, end + 1), classes, prefixes);
        re.lastIndex = end + 1;
      }
      continue;
    }
    if (ch === "{") {
      const end = matchBrace(source, i);
      wordsFromExpression(source.slice(i, end), classes, prefixes);
      re.lastIndex = end;
    }
  }
  return { classes, prefixes };
}

/** 从 styles.css 里提取所有已定义的选择器类名（注释不算定义）。 */
export function extractCssClasses(css: string): Set<string> {
  const out = new Set<string>();
  const body = css.replace(/\/\*[\s\S]*?\*\//g, " ");
  for (const m of body.matchAll(/\.([a-zA-Z][\w-]*)/g)) out.add(m[1]);
  return out;
}

function collectUsed(): { classes: Map<string, Set<string>>; prefixes: Map<string, Set<string>> } {
  const classes = new Map<string, Set<string>>();
  const prefixes = new Map<string, Set<string>>();
  const bump = (map: Map<string, Set<string>>, key: string, file: string) => {
    if (!map.has(key)) map.set(key, new Set());
    map.get(key)!.add(file);
  };
  for (const file of listSourceFiles(SRC)) {
    const src = readFileSync(file, "utf8");
    const got = extractClassNames(src);
    const rel = relative(SRC, file);
    for (const c of got.classes) bump(classes, c, rel);
    for (const p of got.prefixes) bump(prefixes, p, rel);
  }
  return { classes, prefixes };
}

describe("样式守门：className 必须真的有样式", () => {
  const css = readFileSync(CSS_PATH, "utf8");
  const defined = extractCssClasses(css);
  const used = collectUsed();

  it("styles.css 未被截断（三个高优先区块必须在）", () => {
    const lineCount = css.split("\n").length;
    expect(lineCount).toBeGreaterThan(2500);
    expect(css.length).toBeGreaterThan(50000);
    for (const must of [".fleet-canvas", ".hosts-matrix", ".edition-badge", ".sup-cell"]) {
      expect(css).toContain(must);
    }
  });

  it("每个用到的 class 都有对应 CSS 选择器（缺失即失败并列出清单）", () => {
    const missing = [...used.classes.keys()]
      .filter((c) => !defined.has(c) && !KNOWN_UNSTYLED.has(c))
      .sort()
      .map((c) => c + "  <-  " + [...used.classes.get(c)!].slice(0, 3).join(", "));

    expect(
      missing,
      "以下 class 在 .tsx/.ts 的 className 里被使用，但 styles.css 没有对应选择器。\n" +
        "这就是 2026-10-18 那次截断事故的形态：界面不会报错，只是安静地没有样式。\n" +
        "若是故意不留样式，请登记进 KNOWN_UNSTYLED 并写明理由。\n\n" +
        missing.join("\n"),
    ).toEqual([]);
  });

  it("动态前缀（如 src-）至少有一个具体变体被定义", () => {
    const dead = [...used.prefixes.keys()]
      .filter((p) => ![...defined].some((d) => d.startsWith(p)))
      .sort()
      .map((p) => p + "  <-  " + [...used.prefixes.get(p)!].slice(0, 3).join(", "));

    expect(
      dead,
      "以下动态前缀在代码里参与 className 拼接，但 styles.css 里没有任何以它开头的选择器。\n" +
        "这意味着那处拼接出来的 class 全都没有样式。\n\n" +
        dead.join("\n"),
    ).toEqual([]);
  });

  it("提取器能处理动态拼接，并且不把比较运算的状态值当成 class", () => {
    const sample = [
      '<b className={"nav-item" + (active ? " is-active" : "")} />',
      '<i className={`task-row${hot ? " is-hot" : ""}`} />',
      '<u className="chip chip-code" />',
      '<s className={mode === "graph" ? "active" : ""} />',
      '<t className={"src src-" + u.source} />',
    ].join("\n");
    const got = extractClassNames(sample);
    expect([...got.classes].sort()).toEqual(
      ["active", "chip", "chip-code", "is-active", "is-hot", "nav-item", "src", "task-row"].sort(),
    );
    expect([...got.prefixes]).toEqual(["src-"]);
  });

  it("纯变量 className 不产出候选（不假装能覆盖）", () => {
    const got = extractClassNames("<div className={cls} />");
    expect([...got.classes]).toEqual([]);
    expect([...got.prefixes]).toEqual([]);
  });

  it("CSS 提取器会忽略注释里的类名（注释不算定义）", () => {
    const got = extractCssClasses("/* .ghost-class 只是文档 */ .real { color: red }");
    expect(got.has("real")).toBe(true);
    expect(got.has("ghost-class")).toBe(false);
  });
});
