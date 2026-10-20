import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { openDatabase } from '../apps/desktop/src/main/db/db.js';
import {
  ModelService, MODEL_ROLES, DEFAULT_PROVIDER_ID, DEFAULT_ROUTES,
  maskKey, localEmbed, localRerank
} from '../apps/desktop/src/main/services/models.js';

// 模型接入与本地兜底：全部用临时目录隔离，绝不触碰仓库内的 data/ 与 assets/。
function tmpDb(name) {
  const dir = path.join(os.tmpdir(), 'cc-models-' + name + '-' + Date.now() + '-' + Math.random().toString(36).slice(2, 7));
  fs.mkdirSync(dir, { recursive: true });
  const db = openDatabase(path.join(dir, 'test.db'));
  const bus = { info() {}, warn() {}, error() {}, emit() {} };
  const svc = new ModelService({ db, bus, logger: null });
  return { db, svc, dir };
}

function cleanup(db, dir) {
  try { db.close(); } catch { /* ignore */ }
  try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* ignore */ }
}

test('模型路由解析：默认提供商与四种角色幂等播种', () => {
  const { db, svc, dir } = tmpDb('seed');
  try {
    // 重复调用 ensureDefaults 不得产生重复行
    svc.ensureDefaults();
    svc.ensureDefaults();
    const providers = db.all('SELECT * FROM model_providers');
    assert.equal(providers.length, 1, '默认提供商应只有一条');
    assert.equal(providers[0].id, DEFAULT_PROVIDER_ID);
    assert.ok(providers[0].base_url.startsWith('https://api.siliconflow.cn'), 'base_url 应为硅基流动');

    const routes = svc.routes();
    assert.deepEqual(routes.roles, MODEL_ROLES);
    for (const role of MODEL_ROLES) {
      const r = routes.routes.find((x) => x.role === role);
      assert.ok(r, '缺少角色路由：' + role);
      assert.equal(r.providerId, DEFAULT_PROVIDER_ID, role + ' 应指向默认提供商');
      assert.ok(r.model, role + ' 应已解析出模型型号');
    }
    const emb = routes.routes.find((x) => x.role === 'embedding');
    assert.equal(emb.dim, 4096, 'embedding 维度应为 4096');
    assert.equal(emb.model, DEFAULT_ROUTES.find((r) => r.role === 'embedding').model);
    // getRoute 解析出 provider 实体
    const resolved = svc.getRoute('embedding');
    assert.equal(resolved.provider.id, DEFAULT_PROVIDER_ID);
  } finally { cleanup(db, dir); }
});

test('模型路由解析：切换角色型号并校验非法输入', () => {
  const { db, svc, dir } = tmpDb('route');
  try {
    svc.ensureDefaults();
    const after = svc.setRoute({ role: 'judge', providerId: DEFAULT_PROVIDER_ID, model: 'Qwen/Qwen3.8-27B-custom', params: { temperature: 0.1 } });
    const judge = after.routes.find((r) => r.role === 'judge');
    assert.equal(judge.model, 'Qwen/Qwen3.8-27B-custom');
    assert.equal(judge.params.temperature, 0.1);
    // 落库且可重新解析
    assert.equal(svc.getRoute('judge').model, 'Qwen/Qwen3.8-27B-custom');
    // 未知角色 / 未知提供商 / 空型号都必须拒绝
    assert.throws(() => svc.setRoute({ role: 'nope', providerId: DEFAULT_PROVIDER_ID, model: 'x' }), /未知模型角色/);
    assert.throws(() => svc.setRoute({ role: 'judge', providerId: 'ghost', model: 'x' }), /提供商不存在/);
    assert.throws(() => svc.setRoute({ role: 'judge', providerId: DEFAULT_PROVIDER_ID, model: '  ' }), /model 不能为空/);
  } finally { cleanup(db, dir); }
});

test('模型接入：密钥打码且对外视图永不回传明文', () => {
  const { db, svc, dir } = tmpDb('mask');
  try {
    svc.ensureDefaults();
    // 测试用的假密钥：形状与真实 key 一致（足够长），但明显是占位值，不含任何真实凭据
    const FAKE_KEY = 'test-' + 'x'.repeat(48);
    const masked = maskKey(FAKE_KEY);
    assert.ok(masked.startsWith('test-x'), '应保留前 6 位');
    assert.ok(masked.includes('••••'), '应包含打码段');
    assert.ok(masked.endsWith('xxxx'), '应保留后 4 位');
    assert.equal(masked.length, 6 + 4 + 4, '打码后长度应为 前6 + ••••(4) + 后4');
    assert.equal(maskKey('short'), '••••', '短密钥整体打码');
    assert.equal(maskKey(''), '');

    // 先注入假密钥（默认种子不带密钥，密钥只来自用户配置或环境变量）
    svc.saveProvider({ id: DEFAULT_PROVIDER_ID, name: '硅基流动', baseUrl: 'https://api.siliconflow.cn/v1', apiKey: FAKE_KEY });

    const view = svc.providers()[0];
    assert.equal(view.apiKey, masked);
    assert.equal(view.hasKey, true);
    assert.ok(!JSON.stringify(view).includes(FAKE_KEY), '对外视图不得含明文密钥');
    // 将打码串回传视为「不修改」，不得把密钥冲掉
    svc.saveProvider({ id: DEFAULT_PROVIDER_ID, name: '硅基流动', baseUrl: 'https://api.siliconflow.cn/v1', apiKey: masked });
    assert.equal(db.get('SELECT api_key FROM model_providers WHERE id = ?', [DEFAULT_PROVIDER_ID]).api_key, FAKE_KEY);
  } finally { cleanup(db, dir); }
});

test('本地兜底检索：云端不可用时向量与精排自动降级且不抛错', async () => {
  const { db, svc, dir } = tmpDb('fallback');
  try {
    svc.ensureDefaults();
    // 指向必然连不上的地址，模拟云端不可用（超时压到 300ms，测试不拖时间）
    svc.saveProvider({
      id: DEFAULT_PROVIDER_ID, name: '不可达', baseUrl: 'http://127.0.0.1:9/v1',
      apiKey: 'sk-test-1234567890', config: { timeoutMs: 300, retries: 1 }
    });

    const emb = await svc.embedOne('读取 PDF 文档');
    assert.equal(emb.local, true, '云端失败应降级本地');
    assert.equal(emb.vector.length, 4096, '本地向量维度应沿用路由登记的 4096');
    // 归一化：模长约等于 1
    const norm = Math.sqrt(emb.vector.reduce((s, v) => s + v * v, 0));
    assert.ok(Math.abs(norm - 1) < 1e-6, '本地向量应 L2 归一化，实际模长 ' + norm);

    const reranked = await svc.rerank('pdf 文档', ['读取 PDF 文档并提取文字', '生成 Excel 表格', '解析 PDF']);
    assert.equal(reranked.local, true);
    assert.equal(reranked.items.length, 3);
    assert.ok(reranked.items[0].score >= reranked.items[1].score, '应按分数降序');
    assert.ok(String(reranked.items[0].document).includes('PDF'), '相关文档应排第一');

    const judged = await svc.judge({ prompt: '这条能力是否重复？', kind: 'duplicate', similarity: 0.95 });
    assert.equal(judged.local, true);
    assert.ok(judged.text.includes('merge'), '判定应退回规则引擎并给出可执行结论');

    // 降级也要计入统计
    const usage = svc.usage({});
    assert.ok(usage.calls >= 3, '每次调用都应写入 model_calls');
    assert.ok(usage.failedCalls >= 3, '降级调用应记为失败');
  } finally { cleanup(db, dir); }
});

test('本地兜底算法：哈希向量确定性 + 词面精排排序', () => {
  const { db, dir } = tmpDb('local');
  try {
    const a = localEmbed('能力核心', 64);
    const b = localEmbed('能力核心', 64);
    assert.deepEqual(a, b, '同一输入应产出相同向量（确定性）');
    assert.equal(a.length, 64);
    assert.notDeepEqual(localEmbed('完全不同', 64), a, '不同输入应产出不同向量');

    const ranked = localRerank('检索 pdf', ['pdf 检索能力', 'excel 导出', '检索 pdf 全文']);
    assert.equal(ranked.length, 3);
    assert.ok(ranked[0].score >= ranked[2].score);
    // 返回项带原始下标，便于调用方回填
    assert.deepEqual(ranked.map((r) => r.index).sort(), [0, 1, 2]);
  } finally { cleanup(db, dir); }
});

test('能力资产目录登记：scope 列存在且默认 host', () => {
  const { db, dir } = tmpDb('scope');
  try {
    assert.ok(db.hasColumn('capabilities', 'scope'), 'capabilities 应含 scope 列');
    const cols = db.all('SELECT name FROM pragma_table_info(?)', ['capabilities']).map((c) => c.name);
    assert.ok(cols.includes('scope'));
    // 索引存在
    const idx = db.all("SELECT name FROM sqlite_master WHERE type='index' AND name='idx_cap_scope'");
    assert.equal(idx.length, 1, '应有 idx_cap_scope 索引');
  } finally { cleanup(db, dir); }
});
