// 文本切分：SQLite unicode61 不切中文，因此入库前把 CJK 逐字空格分隔。
const CJK_CHAR = /[\u3400-\u9fff\uf900-\ufaff]/;
const CJK_RUN = /[\u3400-\u9fff\uf900-\ufaff]+/g;

export function segmentForIndex(text) {
  if (!text) return '';
  return String(text).replace(CJK_RUN, (run) => ' ' + run.split('').join(' ') + ' ');
}

export function normalizeForMatch(text) {
  return String(text || '').toLowerCase().replace(/\s+/g, ' ').trim();
}

// 把用户查询转为 FTS MATCH 表达式（AND 语义，转义引号与特殊符号）。
export function buildMatchQuery(query, options) {
  const opts = options || {};
  const raw = String(query || '').trim();
  if (!raw) return '';
  const tokens = raw
    .replace(/["*():^\-]/g, ' ')
    .split(/\s+/)
    .map((t) => t.trim())
    .filter(Boolean);
  if (!tokens.length) return '';
  const quoted = tokens.map((t) => '"' + segmentForIndex(t).replace(/\s+/g, ' ').trim() + '"');
  return quoted.join(opts.or ? ' OR ' : ' AND ');
}

// 短语近似：中文按「相邻字符对」覆盖率计算，英文按词命中率计算。
export function phraseScore(haystack, needle) {
  const hay = normalizeForMatch(segmentForIndex(haystack));
  const inner = normalizeForMatch(segmentForIndex(needle));
  if (!hay || !inner) return 0;
  if (hay.includes(inner)) return 1;
  const parts = inner.split(' ').filter(Boolean);
  if (!parts.length) return 0;
  let hit = 0;
  for (const p of parts) if (hay.includes(p)) hit++;
  return hit / parts.length;
}

export function snippet(text, query, radius) {
  const plain = String(text || '').replace(/\s+/g, ' ');
  const r = radius || 120;
  const q = String(query || '').trim();
  if (!q) return plain.slice(0, r * 2) + (plain.length > r * 2 ? '…' : '');
  const lower = plain.toLowerCase();
  const idx = lower.indexOf(q.toLowerCase());
  if (idx < 0) return plain.slice(0, r * 2) + (plain.length > r * 2 ? '…' : '');
  const start = Math.max(0, idx - r);
  const end = Math.min(plain.length, idx + q.length + r);
  return (start > 0 ? '…' : '') + plain.slice(start, end) + (end < plain.length ? '…' : '');
}
