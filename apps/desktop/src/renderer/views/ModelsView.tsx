import React, { useCallback, useEffect, useMemo, useState } from 'react';
import {
  api, fmtTime, MODEL_ROLE_ICON, MODEL_ROLE_LABEL, type ModelHealth, type ModelProviderRow,
  type ModelRouteRow, type ModelRoutesPayload, type ModelUsagePayload
} from '../lib/api';
import { Badge, Empty, Modal, Spinner } from '../components/ui';

const ROLES = ['embedding', 'rerank', 'judge', 'general'];
const ROLE_HINT: Record<string, string> = {
  embedding: '把能力资产与查询转成向量，供语义检索使用',
  rerank: '对检索候选做精排，提升命中顺序',
  judge: '语义判定层：去重裁决、相似度与归类的判定',
  general: '通用替代模型，其余场景的默认兜底'
};

/* ---------------- 字段兼容：主进程可能返回 camelCase 或 snake_case ---------------- */

function providerBaseUrl(p: ModelProviderRow): string { return String(p.baseUrl ?? p.base_url ?? ''); }
function providerApiKey(p: ModelProviderRow): string { return String(p.apiKey ?? p.api_key ?? ''); }
function providerIsDefault(p: ModelProviderRow): boolean { return Boolean(p.isDefault ?? p.is_default); }
function providerEnabled(p: ModelProviderRow): boolean { return p.enabled === undefined ? true : Boolean(p.enabled); }
function providerCheckStatus(p: ModelProviderRow): string { return String(p.lastCheckStatus ?? p.last_check_status ?? 'never'); }
function providerCheckMessage(p: ModelProviderRow): string { return String(p.lastCheckMessage ?? p.last_check_message ?? ''); }
function providerCheckAt(p: ModelProviderRow): number | undefined {
  const v = p.lastCheckAt ?? p.last_check_at;
  return typeof v === 'number' ? v : undefined;
}
function routeProviderId(r: ModelRouteRow): string { return String(r.providerId ?? r.provider_id ?? ''); }

/** 主进程已经打码时原样展示；万一回传了明文，这里再兜一层：前 6 位 + •••• + 后 4 位 */
function maskKey(key: string): string {
  if (!key) return '（未配置）';
  if (key.includes('•')) return key;
  if (key.length <= 12) return '••••';
  return key.slice(0, 6) + '••••' + key.slice(-4);
}

function normalizeRoutes(payload: ModelRoutesPayload | ModelRouteRow[] | null): ModelRoutesPayload {
  if (!payload) return { routes: [] };
  if (Array.isArray(payload)) return { routes: payload };
  return { routes: payload.routes || [], models: payload.models };
}

/** byRole 兼容 { role, c }[] 与 { [role]: count } */
function normalizeByRole(byRole: ModelUsagePayload['byRole'] | undefined): { role: string; c: number }[] {
  if (!byRole) return [];
  if (Array.isArray(byRole)) return byRole.map((x) => ({ role: String(x.role || '未标注'), c: Number(x.c) || 0 }));
  return Object.keys(byRole).map((k) => ({ role: k, c: Number((byRole as Record<string, number>)[k]) || 0 }));
}

export default function ModelsView({ toast, onChanged }: { toast: (m: string, t?: string) => void; onChanged: () => void }) {
  const [providers, setProviders] = useState<ModelProviderRow[]>([]);
  const [routes, setRoutes] = useState<ModelRouteRow[]>([]);
  const [models, setModels] = useState<Record<string, string[]> | string[] | undefined>(undefined);
  const [usage, setUsage] = useState<ModelUsagePayload | null>(null);
  const [health, setHealth] = useState<ModelHealth | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string>('');

  // 提供商编辑
  const [editOpen, setEditOpen] = useState(false);
  const [editId, setEditId] = useState<string | null>(null);
  const [form, setForm] = useState({ name: '', baseUrl: '', apiKey: '', kind: 'openai-compatible', enabled: true });

  // 角色路由编辑
  const [routeDraft, setRouteDraft] = useState<Record<string, { providerId: string; model: string }>>({});

  const load = useCallback(async () => {
    setLoading(true);
    const [pv, rt, us, hl] = await Promise.all([
      api.modelProviders().catch((e) => { toast('读取提供商失败：' + e.message, 'error'); return [] as ModelProviderRow[]; }),
      api.modelRoutes().catch(() => null),
      api.modelUsage(50).catch(() => null),
      api.modelHealth().catch(() => null)
    ]);
    setProviders(pv || []);
    const rp = normalizeRoutes(rt);
    setRoutes(rp.routes);
    setModels(rp.models);
    setUsage(us);
    setHealth(hl);
    const draft: Record<string, { providerId: string; model: string }> = {};
    for (const r of rp.routes) draft[r.role] = { providerId: routeProviderId(r), model: String(r.model || '') };
    setRouteDraft(draft);
    setLoading(false);
  }, [toast]);

  useEffect(() => { load(); }, [load]);

  const openEdit = (p?: ModelProviderRow) => {
    setEditId(p ? p.id : null);
    setForm({
      name: p ? p.name : '',
      baseUrl: p ? providerBaseUrl(p) : 'https://api.siliconflow.cn/v1',
      apiKey: '',
      kind: p ? String(p.kind || 'openai-compatible') : 'openai-compatible',
      enabled: p ? providerEnabled(p) : true
    });
    setEditOpen(true);
  };

  const saveProvider = async () => {
    if (!form.name.trim() || !form.baseUrl.trim()) { toast('请填写名称与 base_url', 'error'); return; }
    setBusy('save');
    try {
      const list = await api.saveModelProvider({
        id: editId || undefined,
        name: form.name.trim(),
        baseUrl: form.baseUrl.trim(),
        // 留空表示不修改已有密钥（主进程不会回传明文，所以编辑时永远不回填）
        apiKey: form.apiKey.trim() || undefined,
        kind: form.kind,
        enabled: form.enabled
      });
      setProviders(Array.isArray(list) && list.length ? list : await api.modelProviders().catch(() => []));
      toast('提供商已保存', 'success');
      setEditOpen(false); onChanged();
    } catch (e: any) { toast('保存失败：' + e.message, 'error'); }
    finally { setBusy(''); }
  };

  const testProvider = async (p: ModelProviderRow) => {
    setBusy('test:' + p.id);
    try {
      const r = await api.testModelProvider(p.id);
      toast((r.ok ? '连通正常' : '连通失败') + ' · ' + r.latencyMs + 'ms · ' + r.message, r.ok ? 'success' : 'error');
      await load();
    } catch (e: any) { toast('测试失败：' + e.message, 'error'); }
    finally { setBusy(''); }
  };

  const removeProvider = async (p: ModelProviderRow) => {
    if (!confirm('删除提供商「' + p.name + '」？使用它的角色路由需要重新指定。')) return;
    setBusy('remove:' + p.id);
    try { await api.removeModelProvider(p.id); toast('已删除', 'success'); await load(); onChanged(); }
    catch (e: any) { toast('删除失败：' + e.message, 'error'); }
    finally { setBusy(''); }
  };

  const saveRoute = async (role: string) => {
    const draft = routeDraft[role];
    if (!draft || !draft.providerId || !draft.model.trim()) { toast('请选择提供商并填写模型名', 'error'); return; }
    setBusy('route:' + role);
    try {
      const r = await api.setModelRoute({ role, providerId: draft.providerId, model: draft.model.trim() });
      const rp = normalizeRoutes(r as any);
      if (rp.routes.length) setRoutes(rp.routes); else await load();
      toast(MODEL_ROLE_LABEL[role] || role + ' 路由已更新', 'success');
      onChanged();
    } catch (e: any) { toast('更新路由失败：' + e.message, 'error'); }
    finally { setBusy(''); }
  };

  const routeOf = (role: string) => routes.find((r) => r.role === role);
  const providerName = (id: string) => {
    const p = providers.find((x) => x.id === id);
    return p ? p.name : (id || '未指定');
  };
  const byRole = useMemo(() => normalizeByRole(usage?.byRole), [usage]);
  const modelOptions = (providerId: string): string[] => {
    if (!models) return [];
    if (Array.isArray(models)) return models;
    return models[providerId] || [];
  };
  const failedCalls = (usage?.recent || []).filter((c) => c.ok === false || c.ok === 0).length;
  const recent = (usage?.recent || []).slice(0, 25);
  const fallbackOn = health ? health.localFallback !== false : true;

  return (
    <div>
      <div className="hero mb16">
        <div className="hero-title">模型接入 · 云端增强，本地兜底</div>
        <div className="hero-sub">
          提供商负责出算力，角色路由决定「谁来做向量嵌入、精排与判定」。任何云端不可用时，能力核心都会退回本地检索与规则链路——
          FTS5 + trigram + BM25 打分与规则判定始终可用，不会因为云端失败而中断。
        </div>
      </div>

      <div className="grid cols-4 mb16">
        <div className="card stat green"><div className="stat-glow" /><div className="stat-label">提供商</div><div className="stat-value">{providers.length}</div><div className="stat-foot">{providers.filter((p) => providerEnabled(p)).length} 个启用</div></div>
        <div className="card stat blue"><div className="stat-glow" /><div className="stat-label">累计调用</div><div className="stat-value">{usage?.calls ?? 0}</div><div className="stat-foot">计入 model_calls</div></div>
        <div className="card stat amber"><div className="stat-glow" /><div className="stat-label">近期失败</div><div className="stat-value">{failedCalls}</div><div className="stat-foot">最近 {(usage?.recent || []).length} 次调用</div></div>
        <div className="card stat violet"><div className="stat-glow" /><div className="stat-label">本地兜底</div><div className="stat-value" style={{ fontSize: 18 }}>{fallbackOn ? '已启用' : '未启用'}</div><div className="stat-foot">云端失败时自动退回</div></div>
      </div>

      <div className="toolbar">
        <button className="btn ghost" onClick={load}>{loading ? <><Spinner /> 刷新中</> : '↻ 刷新'}</button>
        <div className="grow" />
        <button className="btn primary" onClick={() => openEdit()}>＋ 添加提供商</button>
      </div>

      <div className="card mb16">
        <div className="card-header"><div className="card-title">提供商</div><div className="grow" /><span className="hint">密钥仅存本地数据库，界面打码显示</span></div>
        <div className="card-body" style={{ padding: 0 }}>
          <table className="table">
            <thead><tr><th>名称</th><th>base_url</th><th style={{ width: 170 }}>API Key</th><th style={{ width: 90 }}>协议</th><th style={{ width: 150 }}>最近检测</th><th style={{ width: 220 }}></th></tr></thead>
            <tbody>
              {providers.map((p) => {
                const status = providerCheckStatus(p);
                return (
                  <tr key={p.id} style={{ cursor: 'default' }}>
                    <td>
                      <div style={{ fontWeight: 550, display: 'flex', alignItems: 'center', gap: 6 }}>
                        {p.name}
                        {providerIsDefault(p) ? <Badge tone="blue">默认</Badge> : null}
                        {providerEnabled(p) ? <Badge tone="green">启用</Badge> : <Badge tone="gray">停用</Badge>}
                      </div>
                      <div className="hint mono">{p.id}</div>
                    </td>
                    <td className="mono hint" style={{ maxWidth: 260, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{providerBaseUrl(p) || '—'}</td>
                    <td className="mono" style={{ color: '#7fe6c7' }}>{maskKey(providerApiKey(p))}</td>
                    <td><span className="chip">{String(p.kind || 'openai-compatible')}</span></td>
                    <td className="hint">
                      {status === 'ok' ? <Badge tone="green">正常</Badge> : status === 'fail' ? <Badge tone="red">失败</Badge> : <Badge tone="gray">未检测</Badge>}
                      <div className="mt8">{fmtTime(providerCheckAt(p))}</div>
                      {providerCheckMessage(p) ? <div style={{ maxWidth: 150, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={providerCheckMessage(p)}>{providerCheckMessage(p)}</div> : null}
                    </td>
                    <td className="right">
                      <button className="btn small" disabled={busy === 'test:' + p.id} onClick={() => testProvider(p)}>{busy === 'test:' + p.id ? '测试中' : '测试连通'}</button>
                      <button className="btn small ghost" onClick={() => openEdit(p)}>编辑</button>
                      <button className="btn small ghost danger" disabled={busy === 'remove:' + p.id} onClick={() => removeProvider(p)}>删除</button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          {loading && !providers.length ? <div className="empty"><Spinner /> 读取提供商…</div> : null}
          {!loading && !providers.length ? <Empty title="还没有接入提供商" hint="点击「添加提供商」填写任意 OpenAI 兼容的 base_url 与 API Key" /> : null}
        </div>
      </div>

      <div className="card mb16">
        <div className="card-header"><div className="card-title">角色路由</div><div className="grow" /><span className="hint">角色与具体型号解耦，随时可换</span></div>
        <div className="card-body">
          <div className="grid cols-2">
            {ROLES.map((role) => {
              const cur = routeOf(role);
              const draft = routeDraft[role] || { providerId: '', model: '' };
              const options = modelOptions(draft.providerId);
              return (
                <div key={role} className="card" style={{ background: '#0d151c' }}>
                  <div className="card-body" style={{ padding: 12 }}>
                    <div className="flex mb8">
                      <span className="kind-icon">{MODEL_ROLE_ICON[role]}</span>
                      <span style={{ fontWeight: 600 }}>{MODEL_ROLE_LABEL[role]}</span>
                      <span className="chip">{role}</span>
                      <div className="grow" />
                      {cur ? <Badge tone="green">已配置</Badge> : <Badge tone="amber">未配置</Badge>}
                    </div>
                    <div className="hint mb8">{ROLE_HINT[role]}</div>
                    <dl className="kv mb8">
                      <dt>当前提供商</dt><dd>{cur ? providerName(routeProviderId(cur)) : '—'}</dd>
                      <dt>当前模型</dt><dd className="mono">{cur ? String(cur.model || '—') : '—'}</dd>
                      {cur && cur.dim ? <><dt>向量维度</dt><dd className="mono">{cur.dim}</dd></> : null}
                      <dt>更新时间</dt><dd className="hint">{cur ? fmtTime(cur.updatedAt) : '—'}</dd>
                    </dl>
                    <div className="flex wrap" style={{ gap: 6 }}>
                      <select className="select" style={{ maxWidth: 170 }} value={draft.providerId}
                        onChange={(e) => setRouteDraft({ ...routeDraft, [role]: { providerId: e.target.value, model: '' } })}>
                        <option value="">选择提供商…</option>
                        {providers.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
                      </select>
                      <input className="input" style={{ maxWidth: 190 }} list={'models-' + role} placeholder="模型名"
                        value={draft.model}
                        onChange={(e) => setRouteDraft({ ...routeDraft, [role]: { providerId: draft.providerId, model: e.target.value } })} />
                      <datalist id={'models-' + role}>{options.map((m) => <option key={m} value={m} />)}</datalist>
                      <button className="btn small primary" disabled={busy === 'route:' + role} onClick={() => saveRoute(role)}>{busy === 'route:' + role ? '保存中' : '保存路由'}</button>
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      </div>

      <div className="grid cols-2">
        <div className="card">
          <div className="card-header"><div className="card-title">调用统计</div><div className="grow" /><span className="hint">model_calls</span></div>
          <div className="card-body">
            <div className="flex mb12">
              <span className="hint">按角色</span><div className="grow" /><span className="mono hint">{usage?.calls ?? 0} 次总计</span>
            </div>
            {byRole.length ? byRole.map((r) => {
              const maxC = Math.max(1, ...byRole.map((x) => x.c));
              return (
                <div key={r.role} className="mb8">
                  <div className="flex" style={{ marginBottom: 4 }}>
                    <span style={{ fontSize: 11.5 }}>{MODEL_ROLE_LABEL[r.role] || r.role}</span>
                    <div className="grow" />
                    <span className="hint mono">{r.c}</span>
                  </div>
                  <div className="progress-line"><span style={{ width: Math.round((r.c / maxC) * 100) + '%' }} /></div>
                </div>
              );
            }) : <div className="hint">还没有调用记录。测试连通或执行检索后会在这里累计。</div>}
            <div className="section-title">最近调用</div>
            <div style={{ maxHeight: 260, overflowY: 'auto' }}>
              {recent.map((c, i) => (
                <div key={(c.id ?? i) + '-' + c.ts} className="flex" style={{ padding: '5px 0', borderBottom: '1px solid #131e27', fontSize: 11.5 }}>
                  <Badge tone={c.ok === false || c.ok === 0 ? 'red' : 'green'}>{c.ok === false || c.ok === 0 ? '失败' : '成功'}</Badge>
                  <span className="mono hint">{MODEL_ROLE_LABEL[String(c.role)] || c.role || '—'}</span>
                  <span className="mono hint" style={{ maxWidth: 150, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{c.model || '—'}</span>
                  <div className="grow" />
                  <span className="hint mono">{c.latencyMs ?? c.latency_ms ?? 0}ms</span>
                  <span className="hint mono">{fmtTime(c.ts)}</span>
                </div>
              ))}
              {!recent.length ? <div className="hint">暂无调用</div> : null}
            </div>
          </div>
        </div>

        <div className="card">
          <div className="card-header"><div className="card-title">本地兜底</div><div className="grow" />{fallbackOn ? <Badge tone="green">已启用</Badge> : <Badge tone="amber">检查中</Badge>}</div>
          <div className="card-body">
            <div className="hint" style={{ lineHeight: 1.95 }}>
              <b>云端可用时</b>：向量嵌入走 embedding 角色、候选精排走 rerank 角色、语义判定走 judge 角色，
              其余场景交由 general 角色兜底。<br /><br />
              <b>云端失败时</b>：<br />
              · 检索自动退回本地 FTS5 + trigram + 结构化 LIKE 三路融合，精排退回 BM25 分数；<br />
              · 判定退回内置规则引擎；<br />
              · 任何路径都不会因云端异常而报错中断，只在事件流里留一条降级记录。<br /><br />
              <b>调用约束</b>：单次请求默认 15 秒超时、失败重试 1 次，结果全部计入 model_calls 统计表。
            </div>
            <div className="section-title">回调信息</div>
            <dl className="kv">
              <dt>提供商</dt><dd>{providers.length} 个（{providers.filter((p) => providerEnabled(p)).length} 启用）</dd>
              <dt>路由</dt><dd>{routes.length} / {ROLES.length} 个角色已配置</dd>
              <dt>健康检查</dt><dd>{health ? (health.providers || []).length + ' 提供商 / ' + (health.routes || []).length + ' 路由' : '—'}</dd>
              <dt>兜底状态</dt><dd>{fallbackOn ? '云端不可用时自动接管' : '不可用'}</dd>
            </dl>
          </div>
        </div>
      </div>

      {editOpen ? (
        <Modal title={editId ? '编辑提供商' : '添加提供商'} onClose={() => setEditOpen(false)}
          footer={<>
            <button className="btn" onClick={() => setEditOpen(false)}>取消</button>
            <button className="btn primary" disabled={busy === 'save'} onClick={saveProvider}>{busy === 'save' ? '保存中' : '保存'}</button>
          </>}>
          <div className="hint mb12">支持任意 OpenAI 兼容端点。密钥只写入本地 SQLite，接口与界面都不会回传明文。</div>
          <div className="flex mb12"><span className="hint" style={{ width: 88 }}>名称</span>
            <input className="input grow" placeholder="例如：硅基流动" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} /></div>
          <div className="flex mb12"><span className="hint" style={{ width: 88 }}>base_url</span>
            <input className="input grow" placeholder="https://api.siliconflow.cn/v1" value={form.baseUrl} onChange={(e) => setForm({ ...form, baseUrl: e.target.value })} /></div>
          <div className="flex mb12"><span className="hint" style={{ width: 88 }}>API Key</span>
            <input className="input grow" type="password" placeholder={editId ? '留空表示不修改' : 'sk-…'} value={form.apiKey} onChange={(e) => setForm({ ...form, apiKey: e.target.value })} /></div>
          <div className="flex mb12"><span className="hint" style={{ width: 88 }}>协议</span>
            <select className="select" value={form.kind} onChange={(e) => setForm({ ...form, kind: e.target.value })}>
              <option value="openai-compatible">openai-compatible</option>
            </select></div>
          <div className="flex"><span className="hint" style={{ width: 88 }}>启用</span>
            <input type="checkbox" checked={form.enabled} onChange={(e) => setForm({ ...form, enabled: e.target.checked })} />
            <span className="hint">停用后该提供商不参与任何路由调用</span></div>
        </Modal>
      ) : null}
    </div>
  );
}
