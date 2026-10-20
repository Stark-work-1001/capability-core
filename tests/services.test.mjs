import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createContext, closeContext } from '../apps/desktop/src/main/services/context.js';

function freshCtx(name) {
  const home = path.join(os.tmpdir(), 'cc-ctx-' + name + '-' + Date.now());
  fs.mkdirSync(home, { recursive: true });
  // 能力资产库也放到临时目录：测试不得写进仓库的真实 assets/
  const assetDir = path.join(home, 'assets');
  const ctx = createContext({
    dbPath: path.join(home, 'test.db'),
    logDir: null,
    backupDir: path.join(home, 'backups'),
    assetDir
  });
  return { ctx, home, assetDir };
}

function makeSkill(home, name, body) {
  const dir = path.join(home, 'skills', name);
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'SKILL.md'), body || ('---\nname: ' + name + '\ndescription: ' + name + ' helper\nversion: 1.0.0\n---\n\n# ' + name + '\n\nDo ' + name + '.\n'));
  return dir;
}

function addSource(ctx, kind, rootPath, host, depth) {
  const id = 'src_' + Math.random().toString(36).slice(2, 9);
  ctx.db.run('INSERT INTO sources(id, kind, host, label, root_path, format, enabled, builtin, scan_interval, created_at, updated_at, config) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)',
    [id, kind, host || 'test', rootPath, rootPath, 'auto', 1, 0, 900, Date.now(), Date.now(), JSON.stringify({ depth: depth ?? 2 })]);
  return id;
}

test('扫描 → 入库 → 检索：端到端', async () => {
  const { ctx, home } = freshCtx('scan');
  try {
    makeSkill(home, 'pdf-reader', '---\nname: pdf-reader\ndescription: Extract text from PDF documents\nversion: 1.2.0\nauthor: alice\n---\n\n# PDF Reader\n\nUse pypdf to read PDF files.\n');
    makeSkill(home, 'excel-helper', '---\nname: excel-helper\ndescription: Build spreadsheets with formulas\n---\n\n# Excel\n\nUse openpyxl.\n');
    addSource(ctx, 'skill', path.join(home, 'skills'), 'dsh');
    const res = await ctx.services.scanner.scanAll({});
    assert.equal(res.errors.length, 0);
    assert.ok(res.found >= 2);
    const caps = ctx.services.registry.listCapabilities({});
    assert.equal(caps.length, 2);
    const hit = ctx.services.search.search('extract pdf', {});
    assert.ok(hit.items.length >= 1);
    assert.equal(hit.items[0].name, 'pdf-reader');
  } finally { closeContext(ctx); fs.rmSync(home, { recursive: true, force: true }); }
});

test('重复身份：同名能力合并为逻辑能力 + 多实例', async () => {
  const { ctx, home } = freshCtx('dup');
  try {
    makeSkill(home, 'dup-skill');
    const second = path.join(home, 'other');
    makeSkill(home, 'dup-skill');
    fs.cpSync(path.join(home, 'skills', 'dup-skill'), path.join(second, 'dup-skill'), { recursive: true });
    addSource(ctx, 'skill', path.join(home, 'skills'), 'dsh');
    addSource(ctx, 'skill', second, 'codex');
    await ctx.services.scanner.scanAll({});
    const caps = ctx.services.registry.listCapabilities({});
    assert.equal(caps.length, 1, '同身份应合并为一个逻辑能力');
    const detail = ctx.services.registry.getCapability(caps[0].id);
    assert.equal(detail.installations.length, 2);
    const faults = ctx.services.faults.diagnose();
    assert.ok(faults.detected >= 1, '应识别出重复实例故障');
  } finally { closeContext(ctx); fs.rmSync(home, { recursive: true, force: true }); }
});

test('去重裁决：保留最新版本会归档旧实例', async () => {
  const { ctx, home } = freshCtx('dedupe');
  try {
    const a = path.join(home, 'a'); const b = path.join(home, 'b');
    fs.mkdirSync(path.join(a, 'tool'), { recursive: true });
    fs.mkdirSync(path.join(b, 'tool'), { recursive: true });
    // 不同作者命名空间 → 两个逻辑能力，但名称归一相同，属于真实的「重复能力」
    fs.writeFileSync(path.join(a, 'tool', 'SKILL.md'), '---\nname: tool\ndescription: shared helper for data work\nversion: 1.0.0\nauthor: alice\n---\n\nold\n');
    fs.writeFileSync(path.join(b, 'tool', 'SKILL.md'), '---\nname: tool\ndescription: shared helper for data work\nversion: 2.0.0\nauthor: bob\n---\n\nnew body here\n');
    addSource(ctx, 'skill', a, 'dsh');
    addSource(ctx, 'skill', b, 'codex');
    await ctx.services.scanner.scanAll({});
    const scan = ctx.services.dedupe.scan();
    assert.ok(scan.groups >= 1);
    const groups = ctx.services.dedupe.list({ decision: 'pending' });
    assert.ok(groups.length >= 1);
    const g = groups[0];
    assert.equal(g.members.length, 2);
    const newest = g.members.find((m) => m.version === '2.0.0');
    ctx.services.dedupe.decide(g.id, 'keep_latest', newest.id);
    const old = ctx.services.registry.getCapability(g.members.find((m) => m.version !== '2.0.0').id);
    assert.equal(old.status, 'archived');
    const keep = ctx.services.registry.getCapability(newest.id);
    assert.equal(keep.status, 'active');
  } finally { closeContext(ctx); fs.rmSync(home, { recursive: true, force: true }); }
});

test('故障诊断与自动修复：缺 frontmatter 补齐且可回滚', async () => {
  const { ctx, home } = freshCtx('fault');
  try {
    const dir = path.join(home, 'skills', 'plain');
    fs.mkdirSync(dir, { recursive: true });
    const file = path.join(dir, 'SKILL.md');
    fs.writeFileSync(file, '# Plain skill\n\nDoes plain things.\n');
    addSource(ctx, 'skill', path.join(home, 'skills'), 'dsh');
    await ctx.services.scanner.scanAll({});
    const diag = ctx.services.faults.diagnose();
    assert.ok(diag.detected >= 1);
    const faults = ctx.services.faults.list({ status: 'open' });
    const fmFault = faults.find((f) => f.code === 'skill-missing-frontmatter');
    assert.ok(fmFault, '应识别缺少 frontmatter');
    const fix = await ctx.services.faults.autoFix(fmFault.id);
    assert.ok(fix.backupId);
    const after = fs.readFileSync(file, 'utf8');
    assert.ok(after.startsWith('---\nname: plain'));
    const repairs = ctx.services.faults.repairs(fmFault.id);
    assert.equal(repairs.length, 1);
    assert.equal(repairs[0].result, 'applied');
    // 回滚备份
    const restore = ctx.services.ecosystem.restore(fix.backupId);
    assert.ok(restore.ok);
    assert.ok(fs.readFileSync(file, 'utf8').startsWith('# Plain skill'));
  } finally { closeContext(ctx); fs.rmSync(home, { recursive: true, force: true }); }
});

test('生命周期：停用 → 删除 → 回收站 → 恢复', async () => {
  const { ctx, home } = freshCtx('lifecycle');
  try {
    const dir = makeSkill(home, 'temp-skill');
    addSource(ctx, 'skill', path.join(home, 'skills'), 'dsh');
    await ctx.services.scanner.scanAll({});
    const caps = ctx.services.registry.listCapabilities({});
    const id = caps[0].id;
    ctx.services.lifecycle.action(id, 'disable');
    assert.equal(ctx.services.registry.getCapability(id).status, 'disabled');
    ctx.services.lifecycle.action(id, 'activate');
    assert.equal(ctx.services.registry.getCapability(id).status, 'active');
    ctx.services.lifecycle.action(id, 'snapshot');
    const del = ctx.services.lifecycle.action(id, 'delete');
    assert.equal(del.status, 'archived');
    assert.ok(!fs.existsSync(dir), '原目录应已移入回收站');
    const restored = ctx.services.lifecycle.action(id, 'restore');
    assert.ok(restored.restored.length >= 1);
    assert.ok(fs.existsSync(dir), '恢复后目录应回到原位置');
  } finally { closeContext(ctx); fs.rmSync(home, { recursive: true, force: true }); }
});

test('导入预检与入库：复制到能力资产库并登记来源', async () => {
  const { ctx, home, assetDir } = freshCtx('import');
  try {
    const src = path.join(home, 'external', 'cool-skill');
    fs.mkdirSync(src, { recursive: true });
    fs.writeFileSync(path.join(src, 'SKILL.md'), '---\nname: cool-skill\ndescription: Cool imported skill\n---\n\nbody\n');
    const preview = ctx.services.imports.preview(src);
    assert.equal(preview.ok, true);
    assert.equal(preview.kind, 'skill');
    const res = ctx.services.imports.importPath(src);
    assert.ok(fs.existsSync(res.dest));
    assert.ok(res.dest.startsWith(path.resolve(assetDir)), '入库落点应在能力资产库内：' + res.dest);
    assert.ok(fs.existsSync(path.join(res.dest, 'SKILL.md')));
  } finally { closeContext(ctx); fs.rmSync(home, { recursive: true, force: true }); }
});

test('检索：中文与同义词扩展', async () => {
  const { ctx, home } = freshCtx('search');
  try {
    makeSkill(home, 'doc-reader', '---\nname: doc-reader\ndescription: 读取 PDF 与 Word 文档并提取文字\n---\n\n# 文档读取\n\n支持中文与英文文档。\n');
    makeSkill(home, 'spreadsheet', '---\nname: spreadsheet\ndescription: 生成 Excel 表格\n---\n\n# 表格\n\n使用 openpyxl 输出 xlsx。\n');
    addSource(ctx, 'skill', path.join(home, 'skills'), 'dsh');
    await ctx.services.scanner.scanAll({});
    const r1 = ctx.services.search.search('读取文档', {});
    assert.ok(r1.items.length >= 1);
    assert.ok(r1.items.some((i) => i.name === 'doc-reader'));
    const r2 = ctx.services.search.search('excel', {});
    assert.ok(r2.items.some((i) => i.name === 'spreadsheet'));
  } finally { closeContext(ctx); fs.rmSync(home, { recursive: true, force: true }); }
});