import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createContext, closeContext } from '../apps/desktop/src/main/services/context.js';
import { entryFileFor } from '../apps/desktop/src/main/services/assets.js';
import { ASSET_KINDS } from '../apps/desktop/src/main/core/paths.js';

// 能力资产库 + 智能体接入：全部在临时目录内完成，绝不写入仓库的真实 assets/ 与 data/。
function freshCtx(name) {
  const home = path.join(os.tmpdir(), 'cc-assets-' + name + '-' + Date.now() + '-' + Math.random().toString(36).slice(2, 7));
  const assetDir = path.join(home, 'assets');
  fs.mkdirSync(assetDir, { recursive: true });
  const ctx = createContext({
    dbPath: path.join(home, 'test.db'),
    logDir: null,
    backupDir: path.join(home, 'backups'),
    assetDir
  });
  return { ctx, home, assetDir };
}

function cleanup(ctx, home) {
  try { closeContext(ctx); } catch { /* ignore */ }
  try { fs.rmSync(home, { recursive: true, force: true }); } catch { /* ignore */ }
}

test('能力资产目录登记：新建资产同时落盘与入库（scope=own）', () => {
  const { ctx, home, assetDir } = freshCtx('create');
  try {
    const created = ctx.services.assets.create({
      kind: 'skill',
      name: 'pdf-reader-asset',
      content: '# PDF 读取\n\n提取 PDF 文本。\n',
      summary: '读取 PDF 并提取文本的能力资产'
    });
    // 物理目录与入口文件必须真实存在
    assert.ok(fs.existsSync(created.path), '资产目录应已创建：' + created.path);
    assert.ok(path.resolve(created.path).startsWith(path.resolve(assetDir)), '资产应落在能力资产库内');
    const entry = path.join(created.path, entryFileFor('skill'));
    assert.ok(fs.existsSync(entry), '入口文件应存在：' + entry);
    const body = fs.readFileSync(entry, 'utf8');
    assert.ok(body.startsWith('---'), 'skill 资产应自动补齐 frontmatter');
    assert.ok(body.includes('pdf-reader-asset'), 'frontmatter 应含名称');

    // 数据库登记：scope=own，可在资产库列表中检索到
    const row = ctx.db.get('SELECT * FROM capabilities WHERE id = ?', [created.id]);
    assert.equal(row.scope, 'own', '自有资产 scope 应为 own');
    assert.equal(row.kind, 'skill');
    const listed = ctx.services.assets.list({ kind: 'skill' });
    assert.ok(listed.items.some((i) => i.id === created.id), '应出现在能力资产库列表');
    assert.ok(listed.total >= 1);

    // 读取正文
    const got = ctx.services.assets.get(created.id);
    assert.equal(got.id, created.id);
    assert.ok(got.content.includes('PDF'), '应能读回正文');
    assert.ok(got.path.endsWith('SKILL.md'), '读取落点应为入口文件');
  } finally { cleanup(ctx, home); }
});

test('能力资产目录登记：stats 统计与宿主能力 scope 隔离', () => {
  const { ctx, home, assetDir } = freshCtx('stats');
  try {
    ctx.services.assets.create({ kind: 'skill', name: 'a1', content: '# A1\n' });
    ctx.services.assets.create({ kind: 'prompt', name: 'p1', content: '# P1\n' });

    const stats = ctx.services.assets.stats();
    assert.equal(stats.total, 2, '应有 2 条自有资产');
    assert.ok(stats.assetDir && path.resolve(stats.assetDir) === path.resolve(assetDir), 'stats.assetDir 应指向能力资产库');
    const kinds = new Map(stats.byKind.map((k) => [k.kind, k.c]));
    assert.equal(kinds.get('skill'), 1);
    assert.equal(kinds.get('prompt'), 1);
    assert.ok(stats.sizeBytes > 0, '应统计到文件体积');
    assert.ok(stats.fileCount >= 2);

    // 四种资产形态都能各自落到 assets/<kind>/ 下（按需建目录，而非依赖全局 ensureDirs）
    for (const kind of ASSET_KINDS) {
      const made = ctx.services.assets.create({ kind, name: 'k-' + kind, content: '' });
      assert.ok(fs.existsSync(path.join(assetDir, kind)), '缺少资产形态目录：' + kind);
      assert.ok(path.resolve(made.path).startsWith(path.join(path.resolve(assetDir), kind) + path.sep),
        kind + ' 资产应落在 assets/' + kind + '/ 下，实际 ' + made.path);
      assert.ok(fs.existsSync(path.join(made.path, entryFileFor(kind))), kind + ' 入口文件应存在：' + entryFileFor(kind));
    }

    // 自有资产不会混进宿主能力库（listCapabilities 默认不区分 scope，这里直接校验列值）
    const own = ctx.db.all("SELECT id, scope FROM capabilities WHERE scope = 'own'");
    assert.equal(own.length, 2 + ASSET_KINDS.length, '自有资产数应为初始 2 条 + 每种形态各 1 条');
    const host = ctx.db.all("SELECT id FROM capabilities WHERE scope = 'host'");
    assert.equal(host.length, 0, '测试环境不应有宿主能力');
  } finally { cleanup(ctx, home); }
});

test('能力资产目录登记：删除移入 .trash 且记录归档可追溯', () => {
  const { ctx, home, assetDir } = freshCtx('remove');
  try {
    const created = ctx.services.assets.create({ kind: 'skill', name: 'temp-asset', content: '# 临时\n' });
    assert.ok(fs.existsSync(created.path));
    const res = ctx.services.assets.remove({ id: created.id });
    assert.equal(res.ok, true);
    assert.ok(!fs.existsSync(created.path), '原目录应已被移走');
    const trashDir = path.join(assetDir, '.trash');
    assert.ok(fs.existsSync(trashDir), '应存在回收站目录');
    const trashed = fs.readdirSync(trashDir);
    assert.ok(trashed.length >= 1, '回收站中应有被删除的资产');
    const row = ctx.db.get('SELECT status FROM capabilities WHERE id = ?', [created.id]);
    assert.equal(row.status, 'archived', '记录应保留为 archived 以便追溯');
  } finally { cleanup(ctx, home); }
});

test('智能体连通检测：probe 返回基线契约结构且缺参不抛错', async () => {
  const { ctx, home } = freshCtx('probe');
  try {
    const agents = ctx.services.agents;
    // 基线 §10：agents.probe({ host }) -> { ok, latencyMs, checks: [{name, ok, detail}] }
    const probe = await agents.probe('dsh');
    assert.equal(typeof probe.ok, 'boolean', 'probe.ok 应为布尔');
    assert.equal(typeof probe.latencyMs, 'number', 'probe.latencyMs 应为数字');
    assert.equal(probe.host, 'dsh');
    assert.ok(Array.isArray(probe.checks), 'probe.checks 应为数组');
    assert.ok(probe.checks.length >= 5, '连通检测应覆盖多项检查，实际 ' + probe.checks.length);
    for (const c of probe.checks) {
      assert.equal(typeof c.name, 'string', 'check.name 应为字符串');
      assert.equal(typeof c.ok, 'boolean', 'check.ok 应为布尔');
      assert.equal(typeof c.detail, 'string', 'check.detail 应为字符串');
    }
    // 六项固定检查项
    const names = probe.checks.map((c) => c.name);
    for (const n of ['home', 'config', 'process', 'heartbeat', 'session', 'endpoint']) {
      assert.ok(names.includes(n), '缺少检查项：' + n);
    }
    // ok 由 home 与 config 决定，必须是二者的合取（契约一致性）
    const byName = new Map(probe.checks.map((c) => [c.name, c.ok]));
    assert.equal(probe.ok, byName.get('home') && byName.get('config'), 'ok 应等于 home 与 config 的合取');
    assert.equal(typeof probe.endpoint, 'string');
    assert.ok(probe.endpoint.startsWith('http://127.0.0.1:'), 'endpoint 应指向本机中枢');

    // 缺参不得抛错，返回 ok:false
    const empty = await agents.probe('');
    assert.equal(empty.ok, false, '缺少 host 时应返回 ok:false');
    assert.ok(Array.isArray(empty.checks) && empty.checks.length >= 1);
  } finally { cleanup(ctx, home); }
});

test('智能体连通检测：connect 对未知宿主安全降级不抛错', () => {
  const { ctx, home } = freshCtx('connect');
  try {
    // 基线 §10：agents.connect({ host }) -> { ok, endpoint, written[], backupId? }
    const res = ctx.services.agents.connect('__no_such_host__');
    assert.equal(typeof res.ok, 'boolean');
    assert.equal(typeof res.endpoint, 'string');
    assert.ok(Array.isArray(res.written), 'connect.written 应为数组');
    assert.ok(res.written.every((w) => typeof w === 'string'), 'written 应为路径字符串数组');
    // 不可写宿主：ok 为 false，但必须给出原因而不是抛错
    assert.equal(res.ok, false);
    assert.ok(typeof res.reason === 'string', '应给出 reason');
  } finally { cleanup(ctx, home); }
});
