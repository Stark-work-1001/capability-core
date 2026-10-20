import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

export function now() { return Date.now(); }

export function newId(prefix) {
  const rand = crypto.randomBytes(8).toString('hex');
  return (prefix ? prefix + '_' : '') + Date.now().toString(36) + rand.slice(0, 8);
}

export function shortId(len) { return crypto.randomBytes(8).toString('hex').slice(0, len || 8); }

export function sha256(input) {
  return crypto.createHash('sha256').update(input).digest('hex');
}

export function sha256File(file) {
  try {
    const buf = fs.readFileSync(file);
    return crypto.createHash('sha256').update(buf).digest('hex');
  } catch { return null; }
}

export function stableStringify(value) {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return '[' + value.map(stableStringify).join(',') + ']';
  const keys = Object.keys(value).sort();
  return '{' + keys.map((k) => JSON.stringify(k) + ':' + stableStringify(value[k])).join(',') + '}';
}

export function jsonParse(text, fallback) {
  if (text === null || text === undefined || text === '') return fallback;
  try { return JSON.parse(text); } catch { return fallback; }
}

export function clamp(n, lo, hi) { return Math.max(lo, Math.min(hi, n)); }

export function slugify(input) {
  const s = String(input || '').trim().toLowerCase();
  const ascii = s.replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  if (ascii) return ascii.slice(0, 64);
  return 'cap-' + sha256(s || String(Date.now())).slice(0, 8);
}

export function normalizeName(input) {
  return String(input || '')
    .toLowerCase()
    .replace(/\s+/g, '-')
    .replace(/[_./\\]+/g, '-')
    .replace(/[^a-z0-9\u4e00-\u9fff-]+/g, '')
    .replace(/-+/g, '-')
    .replace(/^-+|-+$/g, '');
}

export function normalizeText(input) {
  return String(input || '').replace(/\r\n/g, '\n').replace(/\u0000/g, '').trim();
}

export function truncate(text, max) {
  const s = String(text || '');
  if (s.length <= max) return s;
  return s.slice(0, max) + '…';
}

export function oneLine(text, max) {
  return truncate(String(text || '').replace(/\s+/g, ' ').trim(), max || 300);
}

// ---------- 版本比较 ----------
export function parseVersion(v) {
  const raw = String(v || '').trim().replace(/^v/i, '');
  if (!raw) return null;
  const m = raw.match(/^(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:[-+]([0-9A-Za-z.-]+))?/);
  if (!m) return null;
  return { major: Number(m[1] || 0), minor: Number(m[2] || 0), patch: Number(m[3] || 0), pre: m[4] || '', raw };
}

export function compareVersions(a, b) {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  if (!pa && !pb) return 0;
  if (!pa) return -1;
  if (!pb) return 1;
  for (const k of ['major', 'minor', 'patch']) {
    if (pa[k] !== pb[k]) return pa[k] < pb[k] ? -1 : 1;
  }
  if (pa.pre === pb.pre) return 0;
  if (!pa.pre) return 1;      // 正式版高于预发布
  if (!pb.pre) return -1;
  return pa.pre < pb.pre ? -1 : 1;
}

export function versionKey(v) {
  const p = parseVersion(v);
  if (p) {
    return String(p.major).padStart(6, '0') + '.' + String(p.minor).padStart(6, '0') + '.' + String(p.patch).padStart(6, '0') + (p.pre ? '-pre-' + p.pre : '-rel');
  }
  const d = v ? Date.parse(v) : NaN;
  if (!Number.isNaN(d)) return 'date-' + String(d).padStart(16, '0');
  return 'raw-' + String(v || '0');
}

// ---------- 文本检索 ----------
const CJK = /[\u4e00-\u9fff]/;

export function tokenize(text) {
  const s = String(text || '').toLowerCase();
  const out = [];
  const latin = s.match(/[a-z0-9][a-z0-9_.+-]*/g) || [];
  out.push(...latin);
  const cjkRuns = s.match(/[\u4e00-\u9fff]+/g) || [];
  for (const run of cjkRuns) {
    if (run.length <= 2) out.push(run);
    else for (let i = 0; i < run.length - 1; i++) out.push(run.slice(i, i + 2));
  }
  return [...new Set(out)];
}

export function jaccard(aTokens, bTokens) {
  const A = new Set(aTokens); const B = new Set(bTokens);
  if (!A.size || !B.size) return 0;
  let inter = 0;
  for (const t of A) if (B.has(t)) inter++;
  return inter / (A.size + B.size - inter);
}

export function cosineCounter(a, b) {
  let dot = 0; let na = 0; let nb = 0;
  for (const [k, v] of a) { na += v * v; if (b.has(k)) dot += v * b.get(k); }
  for (const v of b.values()) nb += v * v;
  if (!na || !nb) return 0;
  return dot / Math.sqrt(na * nb);
}

export function countTokens(text) {
  const map = new Map();
  for (const t of tokenize(text)) map.set(t, (map.get(t) || 0) + 1);
  return map;
}

// ---------- 文件 ----------
export function walk(dir, options) {
  const opts = options || {};
  const maxDepth = opts.maxDepth ?? 12;
  const maxFiles = opts.maxFiles ?? 4000;
  const ignore = opts.ignore || [];
  const results = [];
  const stack = [{ dir, depth: 0 }];
  while (stack.length && results.length < maxFiles) {
    const cur = stack.pop();
    let entries;
    try { entries = fs.readdirSync(cur.dir, { withFileTypes: true }); } catch { continue; }
    for (const ent of entries) {
      if (ent.name === 'node_modules' || ent.name === '.git' || ent.name === '.venv' || ent.name === '__pycache__') continue;
      if (ignore.some((re) => re.test(ent.name))) continue;
      const full = path.join(cur.dir, ent.name);
      if (ent.isDirectory()) {
        if (cur.depth + 1 <= maxDepth) stack.push({ dir: full, depth: cur.depth + 1 });
      } else if (ent.isFile()) {
        results.push(full);
        if (results.length >= maxFiles) break;
      }
    }
  }
  return results;
}

export function readTextSafe(file, maxBytes) {
  try {
    const st = fs.statSync(file);
    if (st.size > (maxBytes || 2_000_000)) {
      const fd = fs.openSync(file, 'r');
      const buf = Buffer.alloc(maxBytes || 2_000_000);
      const n = fs.readSync(fd, buf, 0, buf.length, 0);
      fs.closeSync(fd);
      return buf.subarray(0, n).toString('utf8');
    }
    return fs.readFileSync(file, 'utf8');
  } catch { return null; }
}

export function writeFileAtomic(file, data) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const tmp = file + '.tmp-' + process.pid + '-' + Date.now();
  fs.writeFileSync(tmp, data);
  fs.renameSync(tmp, file);
}

export function copyDir(src, dest) {
  fs.mkdirSync(dest, { recursive: true });
  const entries = fs.readdirSync(src, { withFileTypes: true });
  for (const ent of entries) {
    if (ent.name === 'node_modules' || ent.name === '.git') continue;
    const s = path.join(src, ent.name);
    const d = path.join(dest, ent.name);
    if (ent.isDirectory()) copyDir(s, d);
    else if (ent.isFile()) fs.copyFileSync(s, d);
  }
}

export function removeDir(dir) {
  fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
}

export function formatBytes(n) {
  if (!n) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let i = 0; let v = n;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return (v >= 10 || i === 0 ? Math.round(v) : v.toFixed(1)) + ' ' + units[i];
}

export function toInt(v, fallback) {
  const n = Number(v);
  return Number.isFinite(n) ? Math.trunc(n) : (fallback || 0);
}

export function uniqueBy(list, keyFn) {
  const seen = new Set(); const out = [];
  for (const item of list) {
    const k = keyFn(item);
    if (seen.has(k)) continue;
    seen.add(k); out.push(item);
  }
  return out;
}

export function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

export function isProbablyText(buf) {
  if (!buf || !buf.length) return true;
  let suspicious = 0;
  const sample = buf.subarray(0, Math.min(buf.length, 4096));
  for (const b of sample) if (b === 0) return false; else if (b < 7 || (b > 13 && b < 32)) suspicious++;
  return suspicious / sample.length < 0.02;
}

export function escapeLike(s) { return String(s).replace(/[%_\\]/g, (m) => '\\' + m); }
