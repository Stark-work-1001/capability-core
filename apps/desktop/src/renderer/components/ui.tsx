import React, { useEffect, useMemo, useRef, useState } from 'react';

export function Badge({ tone, children }: { tone?: string; children: React.ReactNode }) {
  return <span className={'badge ' + (tone || 'gray')}>{children}</span>;
}

export function kindTone(kind: string) {
  return kind === 'skill' ? 'blue' : kind === 'mcp' ? 'violet' : kind === 'plugin' ? 'green' : kind === 'agent' ? 'amber' : 'gray';
}

export function healthTone(health: string) {
  if (health === 'healthy') return 'green';
  if (health === 'warning') return 'amber';
  if (health === 'broken') return 'red';
  return 'gray';
}

export function statusTone(status: string) {
  if (status === 'active') return 'green';
  if (status === 'disabled') return 'gray';
  if (status === 'quarantined') return 'amber';
  if (status === 'archived') return 'gray';
  return 'gray';
}

export function Toast({ items, onClose }: { items: { id: number; text: string; tone: string }[]; onClose: (id: number) => void }) {
  return (
    <div className="toast-wrap">
      {items.map((t) => (
        <div key={t.id} className={'toast ' + t.tone} onClick={() => onClose(t.id)}>{t.text}</div>
      ))}
    </div>
  );
}

export function Modal({ title, children, footer, onClose }: { title: string; children: React.ReactNode; footer?: React.ReactNode; onClose: () => void }) {
  useEffect(() => {
    const h = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', h);
    return () => window.removeEventListener('keydown', h);
  }, [onClose]);
  return (
    <div className="modal-mask" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head"><div className="card-title">{title}</div><div className="grow" /><button className="btn ghost small" onClick={onClose}>✕</button></div>
        <div className="modal-body">{children}</div>
        {footer ? <div className="modal-foot">{footer}</div> : null}
      </div>
    </div>
  );
}

export function Spinner() { return <span className="spinner" />; }

export function SearchBox({ value, onChange, placeholder, onSubmit }: { value: string; onChange: (v: string) => void; placeholder?: string; onSubmit?: () => void }) {
  return (
    <input
      className="input"
      value={value}
      placeholder={placeholder || '搜索…'}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => { if (e.key === 'Enter' && onSubmit) onSubmit(); }}
    />
  );
}

export function Empty({ icon, title, hint }: { icon?: string; title: string; hint?: string }) {
  return (
    <div className="empty">
      <div className="big">{icon || '◇'}</div>
      <div>{title}</div>
      {hint ? <div className="hint mt8">{hint}</div> : null}
    </div>
  );
}

export function Stat({ label, value, foot, tone }: { label: string; value: React.ReactNode; foot?: string; tone?: string }) {
  return (
    <div className={'card stat ' + (tone || '')}>
      <div className="stat-glow" />
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
      {foot ? <div className="stat-foot">{foot}</div> : null}
    </div>
  );
}

export function Tabs({ tabs, active, onChange }: { tabs: { key: string; label: string; badge?: number }[]; active: string; onChange: (k: string) => void }) {
  return (
    <div className="tabs">
      {tabs.map((t) => (
        <div key={t.key} className={'tab ' + (t.key === active ? 'active' : '')} onClick={() => onChange(t.key)}>
          {t.label}{t.badge ? <span className="nav-badge" style={{ marginLeft: 6 }}>{t.badge}</span> : null}
        </div>
      ))}
    </div>
  );
}

// 轻量 Markdown 渲染（标题、列表、代码块、行内代码、粗体、链接、表格）
export function Markdown({ text }: { text: string }) {
  const html = useMemo(() => renderMarkdown(String(text || '')), [text]);
  return <div className="md" dangerouslySetInnerHTML={{ __html: html }} />;
}

function esc(s: string) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

export function renderMarkdown(src: string) {
  const lines = String(src || '').replace(/\r\n/g, '\n').split('\n');
  const out: string[] = [];
  let inCode = false; let codeBuf: string[] = []; let inList = false; let inTable = false;
  const inline = (s: string) => esc(s)
    .replace(/\`([^\`]+)\`/g, '<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/\[([^\]]+)\]\((https?:[^)]+)\)/g, '<a href="$2" target="_blank" rel="noreferrer">$1</a>');
  const closeList = () => { if (inList) { out.push('</ul>'); inList = false; } };
  const closeTable = () => { if (inTable) { out.push('</tbody></table>'); inTable = false; } };
  for (const raw of lines) {
    const line = raw;
    if (line.trim().startsWith('```')) {
      if (inCode) { out.push('<pre><code>' + esc(codeBuf.join('\n')) + '</code></pre>'); inCode = false; codeBuf = []; }
      else { closeList(); closeTable(); inCode = true; codeBuf = []; }
      continue;
    }
    if (inCode) { codeBuf.push(line); continue; }
    const h = line.match(/^(#{1,3})\s+(.*)$/);
    if (h) { closeList(); closeTable(); out.push('<h' + h[1].length + '>' + inline(h[2]) + '</h' + h[1].length + '>'); continue; }
    if (line.trim().startsWith('|') && line.includes('|')) {
      const cells = line.trim().replace(/^\||\|$/g, '').split('|').map((c) => c.trim());
      if (cells.every((c) => /^-+$/.test(c))) continue;
      if (!inTable) { out.push('<table><tbody>'); inTable = true; }
      out.push('<tr>' + cells.map((c) => '<td>' + inline(c) + '</td>').join('') + '</tr>');
      continue;
    } else closeTable();
    const li = line.match(/^\s*[-*]\s+(.*)$/);
    if (li) { if (!inList) { out.push('<ul>'); inList = true; } out.push('<li>' + inline(li[1]) + '</li>'); continue; }
    closeList();
    if (line.trim().startsWith('>')) { out.push('<blockquote>' + inline(line.replace(/^\s*>\s?/, '')) + '</blockquote>'); continue; }
    if (!line.trim()) { out.push(''); continue; }
    out.push('<p>' + inline(line) + '</p>');
  }
  if (inCode) out.push('<pre><code>' + esc(codeBuf.join('\n')) + '</code></pre>');
  closeList(); closeTable();
  return out.join('\n');
}

export function useAsync<T>(fn: () => Promise<T>, deps: any[], initial: T) {
  const [data, setData] = useState<T>(initial);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>('');
  const reload = useRef<() => void>(() => {});
  useEffect(() => {
    let alive = true;
    setLoading(true);
    fn().then((d) => { if (alive) { setData(d); setError(''); } })
      .catch((e) => { if (alive) setError(String(e && e.message || e)); })
      .finally(() => { if (alive) setLoading(false); });
    reload.current = () => {
      fn().then((d) => { if (alive) { setData(d); setError(''); } }).catch((e) => setError(String(e && e.message || e)));
    };
    return () => { alive = false; };
  }, deps);
  return { data, setData, loading, error, reload: () => reload.current() };
}
