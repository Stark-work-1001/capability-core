import { now, jsonParse, tokenize, readTextSafe, clamp } from '../core/util.js';
import { buildMatchQuery, segmentForIndex, normalizeForMatch, snippet } from '../core/text-index.js';

// 同义词扩展：提升中英混合检索召回（不依赖外部向量模型，保证离线可用）。
const SYNONYMS = {
  pdf: ['pdf', '文档', 'document', 'reader'],
  excel: ['excel', 'xlsx', '表格', 'spreadsheet', 'csv'],
  word: ['word', 'docx', '文档'],
  ppt: ['ppt', 'pptx', '演示', 'presentation', 'slide'],
  mcp: ['mcp', 'model context protocol', '服务端'],
  skill: ['skill', '技能', '能力'],
  image: ['image', '图片', '图像', 'photo', 'vision'],
  video: ['video', '视频', 'movie'],
  audio: ['audio', '音频', '声音', 'music'],
  search: ['search', '搜索', '检索', 'query'],
  browser: ['browser', '浏览器', 'web', '网页'],
  database: ['database', '数据库', 'sql', 'sqlite'],
  test: ['test', '测试', 'pytest', 'vitest'],
  deploy: ['deploy', '部署', 'release', '发布'],
  memory: ['memory', '记忆', 'recall', '回忆'],
  code: ['code', '代码', '编程', 'programming'],
  git: ['git', '版本控制', 'commit'],
  research: ['research', '调研', '研究', '论文', 'paper'],
  science: ['science', '科学', '科研', 'academic'],
  three_d: ['3d', '三维', 'blender', '模型'],
  translate: ['translate', '翻译', 'translation'],
  summarize: ['summarize', '总结', '摘要', 'summary']
};

const HOST_LABEL = {
  dsh: 'DeepSeek Harness', codex: 'Codex', claude: 'Claude Code', minimax: 'MiniMax Code',
  gemini: 'Gemini / Antigravity', workbuddy: 'WorkBuddy', cursor: 'Cursor', 'asset-center': '能力资产库', custom: '自定义'
};

export function hostLabel(host) { return HOST_LABEL[host] || host; }

export function expandTerms(query) {
  const base = tokenize(query);
  const extra = new Set();
  for (const t of base) {
    for (const [key, syns] of Object.entries(SYNONYMS)) {
      if (syns.includes(t) || t.includes(key)) {
        syns.forEach((s) => extra.add(s));
      }
    }
  }
  return [...new Set([...base, ...extra])];
}

export class SearchService {
  constructor(ctx) {
    this.ctx = ctx;
    this.db = ctx.db;
  }

  search(query, options) {
    const t0 = Date.now();
    const opts = options || {};
    const q = String(query || '').trim();
    const limit = clamp(opts.limit || 20, 1, 100);
    if (!q) return this.browse(opts);

    const terms = expandTerms(q);
    const match = buildMatchQuery(terms.join(' '), { or: true });
    const results = new Map();

    // 第一路：FTS5（BM25）
    if (match) {
      try {
        const rows = this.db.all(
          `SELECT d.capability_id AS id, bm25(search_fts, 8.0, 4.0, 1.0, 2.0, 2.0, 1.5) AS score
           FROM search_fts JOIN search_docs d ON d.rowid = search_fts.rowid
           WHERE search_fts MATCH ? ORDER BY score LIMIT 200`, [match]);
        for (const r of rows) {
          results.set(r.id, { id: r.id, fts: -r.score });
        }
      } catch (err) { /* 查询语法问题降级 */ }
    }

    // 第二路：trigram 子串（中文短语/路径片段）
    try {
      const tri = this.db.all("SELECT rowid FROM search_trigram WHERE search_trigram MATCH ? LIMIT 200", ['"' + q.replace(/"/g, '') + '"']);
      for (const r of tri) {
        const d = this.db.get('SELECT capability_id FROM search_docs WHERE rowid = ?', [r.rowid]);
        if (d) {
          const cur = results.get(d.capability_id) || { id: d.capability_id, fts: 0 };
          cur.trigram = 1;
          results.set(d.capability_id, cur);
        }
      }
    } catch { /* ignore */ }

    // 第三路：结构化 LIKE（名称/作者/标签）
    const like = '%' + q + '%';
    const likeRows = this.db.all(
      "SELECT id, canonical_name FROM capabilities WHERE canonical_name LIKE ? OR display_name LIKE ? OR author LIKE ? OR tags LIKE ? LIMIT 100",
      [like, like, like, like]);
    for (const r of likeRows) {
      const cur = results.get(r.id) || { id: r.id, fts: 0 };
      cur.like = 1;
      results.set(r.id, cur);
    }

    // 排序与解释
    const names = normalizeForMatch(q);
    const scored = [];
    for (const item of results.values()) {
      const cap = this.db.get('SELECT * FROM capabilities WHERE id = ?', [item.id]);
      if (!cap) continue;
      if (cap.status === 'archived' && !opts.includeArchived) continue;
      const nameMatch = normalizeForMatch(cap.display_name + ' ' + cap.canonical_name).includes(names) ? 1 : 0;
      const summaryMatch = normalizeForMatch(cap.summary).includes(names) ? 1 : 0;
      const tagMatch = normalizeForMatch(jsonParse(cap.tags, []).join(' ')).includes(names) ? 1 : 0;
      const usageBoost = Math.min(0.15, (cap.usage_count || 0) * 0.005);
      const score = (item.fts || 0) * 1.0 + (item.trigram ? 1.6 : 0) + (item.like ? 2.2 : 0)
        + nameMatch * 2.4 + summaryMatch * 0.8 + tagMatch * 0.9 + usageBoost;
      if (opts.kind && cap.kind !== opts.kind) continue;
      if (opts.host && !jsonParse(cap.hosts, []).includes(opts.host)) continue;
      if (opts.status && cap.status !== opts.status) continue;
      scored.push({
        id: cap.id, kind: cap.kind, name: cap.canonical_name, displayName: cap.display_name,
        summary: cap.summary, author: cap.author, version: cap.version, status: cap.status, health: cap.health,
        tags: jsonParse(cap.tags, []), hosts: jsonParse(cap.hosts, []).map((h) => ({ host: h, label: hostLabel(h) })),
        protocols: jsonParse(cap.protocols, []), score: Number(score.toFixed(4)),
        explanation: {
          fts: Number((item.fts || 0).toFixed(3)), trigram: Boolean(item.trigram), name: Boolean(nameMatch),
          summary: Boolean(summaryMatch), tags: Boolean(tagMatch), like: Boolean(item.like)
        },
        snippet: snippet(cap.summary || cap.description, q, 100)
      });
    }
    scored.sort((a, b) => b.score - a.score);
    const top = scored.slice(0, limit);
    this.ctx.bus.info('search', '检索 "' + q + '" 命中 ' + top.length + ' 项（候选 ' + results.size + '）', { query: q, took: Date.now() - t0 });
    return { query: q, terms, took: Date.now() - t0, total: scored.length, items: top, mode: 'hybrid' };
  }

  browse(opts) {
    const rows = this.db.all(
      "SELECT * FROM capabilities WHERE status != 'archived' ORDER BY usage_count DESC, updated_at DESC LIMIT ?", [opts.limit || 30]);
    return {
      query: '', terms: [], took: 0, total: rows.length, mode: 'browse',
      items: rows.map((c) => ({
        id: c.id, kind: c.kind, name: c.canonical_name, displayName: c.display_name, summary: c.summary,
        author: c.author, version: c.version, status: c.status, health: c.health,
        tags: jsonParse(c.tags, []), hosts: jsonParse(c.hosts, []).map((h) => ({ host: h, label: hostLabel(h) })),
        protocols: jsonParse(c.protocols, []), score: 0, explanation: {}, snippet: c.summary
      }))
    };
  }
}
