import { sha256, stableStringify, slugify, normalizeName, oneLine } from '../core/util.js';

// 统一能力清单（Capability Manifest v1）：所有协议解析后都归一到此结构。
export const KINDS = ['skill', 'mcp', 'plugin', 'prompt', 'agent', 'hook'];

export function emptyManifest(kind, name) {
  return {
    schema: 'capability-core.manifest/v1',
    kind,
    name: name || '',
    displayName: name || '',
    summary: '',
    description: '',
    version: '',
    author: '',
    authorSource: 'unknown',
    homepage: '',
    license: '',
    tags: [],
    protocols: [],
    hosts: [],
    entry: '',
    triggers: [],
    allowedTools: [],
    runtime: {},
    install: {},
    resources: [],
    metadata: {},
    evidence: []
  };
}

export function normalizeManifest(input) {
  const m = emptyManifest(input.kind || 'skill', input.name || '');
  for (const key of Object.keys(m)) {
    if (input[key] !== undefined && input[key] !== null) m[key] = input[key];
  }
  m.kind = KINDS.includes(m.kind) ? m.kind : 'skill';
  m.name = String(m.name || '').trim();
  m.displayName = String(m.displayName || m.name).trim();
  m.summary = oneLine(m.summary || m.description || '', 600);
  m.version = String(m.version || '').trim();
  m.author = String(m.author || '').trim();
  m.tags = Array.isArray(m.tags) ? [...new Set(m.tags.map((t) => String(t).trim()).filter(Boolean))].slice(0, 40) : [];
  m.protocols = Array.isArray(m.protocols) ? [...new Set(m.protocols.map(String))] : [];
  m.hosts = Array.isArray(m.hosts) ? [...new Set(m.hosts.map(String))] : [];
  m.triggers = Array.isArray(m.triggers) ? m.triggers.map(String).slice(0, 30) : [];
  return m;
}

// 身份：同 kind 下按「作者命名空间 + 名称」归一；无作者时退化为名称。
export function identityOf(manifest) {
  const kind = manifest.kind;
  const name = normalizeName(manifest.name || manifest.displayName || 'unnamed');
  const author = normalizeName(manifest.author || '');
  const explicit = manifest.identity && manifest.identity.id;
  if (explicit) return { key: kind + ':' + String(explicit), name, author, basis: 'explicit' };
  if (author) return { key: kind + ':' + author + '/' + name, name, author, basis: 'author-name' };
  return { key: kind + ':' + name, name, author: '', basis: 'name' };
}

export function fingerprintOf(manifest, body) {
  return sha256(stableStringify({
    kind: manifest.kind,
    name: normalizeName(manifest.name),
    version: manifest.version || '',
    summary: manifest.summary || '',
    entry: manifest.entry || '',
    body: sha256(String(body || ''))
  }));
}

export function manifestToSearchText(manifest, body) {
  return [
    manifest.displayName,
    manifest.name,
    manifest.summary,
    manifest.author,
    (manifest.tags || []).join(' '),
    (manifest.triggers || []).join(' '),
    (manifest.hosts || []).join(' '),
    (manifest.protocols || []).join(' '),
    body || ''
  ].filter(Boolean).join('\n');
}

export function guessTags(text) {
  const dict = {
    pdf: ['pdf', '文档'], docx: ['word', '文档'], xlsx: ['excel', '表格'], pptx: ['ppt', '演示'],
    video: ['视频'], audio: ['音频'], image: ['图像'], sql: ['数据库'], mcp: ['mcp'],
    react: ['前端'], python: ['python'], rust: ['rust'], git: ['git'], test: ['测试'],
    browser: ['浏览器'], search: ['搜索'], memory: ['记忆'], deploy: ['部署'], api: ['api']
  };
  const lower = String(text || '').toLowerCase();
  const tags = new Set();
  for (const [key, vals] of Object.entries(dict)) if (lower.includes(key)) vals.forEach((v) => tags.add(v));
  return [...tags];
}
