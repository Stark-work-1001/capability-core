import React, { useState } from 'react';
import { api, HOST_LABEL, KIND_ICON, KIND_LABEL, type SearchResult } from '../lib/api';
import { Badge, Empty, Spinner } from '../components/ui';

const EXAMPLES = ['读取 PDF 并提取文本', 'blender 建模', 'mcp 服务器', '学术论文写作', '数据库查询', '浏览器自动化', '视频处理', '代码审查'];

export default function SearchLab({ toast, onOpen }: { toast: (m: string, t?: string) => void; onOpen?: (id: string) => void }) {
  const [q, setQ] = useState('');
  const [res, setRes] = useState<SearchResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [kind, setKind] = useState('');
  const [mode, setMode] = useState<'plain' | 'raw'>('plain');
  const [raw, setRaw] = useState<any>(null);

  const run = async (query?: string, over?: any) => {
    const text = (query ?? q).trim();
    if (!text) return;
    setQ(text);
    setLoading(true);
    try {
      const r = await api.search(text, { limit: 20, kind: (over?.kind ?? kind) || undefined });
      setRes(r);
      setRaw(r);
    } catch (e: any) { toast(e.message, 'error'); }
    finally { setLoading(false); }
  };

  return (
    <div>
      <div className="card mb16">
        <div className="card-body">
          <div className="search-hero">
            <input className="input" placeholder="描述你需要的能力，例如：把论文转成网页、读取 PDF、运行 MATLAB…" value={q}
              onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') run(); }} autoFocus />
            <select className="select" value={kind} onChange={(e) => { setKind(e.target.value); if (res) run(undefined, { kind: e.target.value }); }}>
              <option value="">全部类型</option>
              <option value="skill">技能</option>
              <option value="mcp">MCP</option>
              <option value="plugin">插件</option>
            </select>
            <button className="btn primary" onClick={() => run()} disabled={loading}>{loading ? <><Spinner /> 检索</> : '检索'}</button>
          </div>
          <div className="flex wrap mt12" style={{ gap: 6 }}>
            <span className="hint">试试：</span>
            {EXAMPLES.map((e) => <button key={e} className="btn small ghost" onClick={() => run(e)}>{e}</button>)}
          </div>
        </div>
      </div>

      {res ? (
        <div className="grid" style={{ gridTemplateColumns: 'minmax(0,1fr) 330px' }}>
          <div className="card">
            <div className="card-header">
              <div className="card-title">命中 {res.items.length} 项</div>
              <span className="hint">候选 {res.total} · {res.took}ms · 模式 {res.mode}</span>
              <div className="grow" />
              <div className="seg">
                <button className={mode === 'plain' ? 'active' : ''} onClick={() => setMode('plain')}>结果</button>
                <button className={mode === 'raw' ? 'active' : ''} onClick={() => setMode('raw')}>JSON</button>
              </div>
            </div>
            {mode === 'raw' ? (
              <div className="card-body"><div className="code">{JSON.stringify(raw, null, 2)}</div></div>
            ) : (
              <div>
                {res.items.map((it, idx) => (
                  <div key={it.id} className="list-item" onClick={() => onOpen ? onOpen(it.id) : api.openPath(it.id)}>
                    <div className="title">
                      <span className="faint mono" style={{ width: 20 }}>{idx + 1}</span>
                      <span className="kind-icon">{KIND_ICON[it.kind]}</span>
                      {it.displayName}
                      {it.version ? <span className="chip">{it.version}</span> : null}
                      <Badge tone={it.kind === 'skill' ? 'blue' : it.kind === 'mcp' ? 'violet' : 'green'}>{KIND_LABEL[it.kind]}</Badge>
                      <div className="grow" />
                      <span className="mono hint">{it.score.toFixed(2)}</span>
                    </div>
                    <div className="desc">{it.snippet || it.summary}</div>
                    <div className="meta">
                      <span>作者 {it.author || '未声明'}</span><span>·</span>
                      <span>{it.hosts.map((h) => h.label).join(' / ')}</span><span>·</span>
                      <span>{it.status === 'active' ? '可用' : it.status}</span>
                      {it.explanation && (
                        <>
                          <span>·</span>
                          <span>匹配：{[it.explanation.name && '名称', it.explanation.summary && '摘要', it.explanation.tags && '标签', it.explanation.trigram && '子串', it.explanation.fts && '全文'].filter(Boolean).join('+')}</span>
                        </>
                      )}
                    </div>
                    <div className="score-bar" style={{ width: 180 }}><span style={{ width: Math.min(100, (it.score / (res.items[0]?.score || 1)) * 100) + '%' }} /></div>
                  </div>
                ))}
                {!res.items.length ? <Empty title="没有匹配" hint="尝试更短或更通用的关键词，例如「pdf」「测试」「数据库」" /> : null}
              </div>
            )}
          </div>

          <div>
            <div className="card mb16">
              <div className="card-header"><div className="card-title">检索解释</div></div>
              <div className="card-body">
                <div className="hint mb8">查询扩展词（同义词，提升中英混合召回）：</div>
                <div className="flex wrap" style={{ gap: 4 }}>
                  {(res.terms || []).map((t) => <span key={t} className="chip accent">{t}</span>)}
                </div>
                <div className="section-title">打分构成</div>
                <div className="hint">
                  · 名称命中 ×2.4<br />
                  · 标签命中 ×0.9<br />
                  · 摘要命中 ×0.8<br />
                  · 结构化 LIKE ×2.2<br />
                  · 子串(trigram) ×1.6<br />
                  · BM25 全文分 ×1.0<br />
                  · 使用热度最多 +0.15
                </div>
              </div>
            </div>
            <div className="card">
              <div className="card-header"><div className="card-title">智能体如何调用</div></div>
              <div className="card-body">
                <div className="hint mb8">接入 MCP 后，智能体可直接调用：</div>
                <div className="code" style={{ fontSize: 11 }}>{`capability.search
  { "query": "${q || '读取 PDF'}", "limit": 5 }

capability.read_skill
  { "id": "cap_xxx" }`}</div>
                <button className="btn small mt12" onClick={() => { api.reindex(); toast('索引重建中…'); }}>重建检索索引</button>
              </div>
            </div>
          </div>
        </div>
      ) : (
        <div className="card"><Empty icon="⌕" title="输入关键词或描述开始检索" hint="检索会同时使用全文索引、子串匹配与结构化匹配，并在右侧展示打分构成" /></div>
      )}
    </div>
  );
}
