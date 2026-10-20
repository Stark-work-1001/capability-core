import fs from 'node:fs';
import path from 'node:path';
import { extractFrontmatter } from '../core/yaml.js';
import { normalizeManifest, guessTags } from '../core/manifest.js';
import { oneLine, readTextSafe, sha256, truncate } from '../core/util.js';

// Agent Skills 规范：目录内 SKILL.md + YAML frontmatter（name/description/...）。
export const skillAdapter = {
  id: 'agent-skills',
  label: 'Agent Skills',
  protocols: ['agent-skills'],
  detect(dir) {
    if (!fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) return null;
    const entry = path.join(dir, 'SKILL.md');
    const lower = path.join(dir, 'skill.md');
    if (fs.existsSync(entry)) return entry;
    if (fs.existsSync(lower)) return lower;
    return null;
  },
  parse(dir) {
    const entry = this.detect(dir);
    if (!entry) return null;
    const text = readTextSafe(entry) || '';
    const fm = extractFrontmatter(text);
    const data = fm.data || {};
    const body = fm.body || '';
    const resources = listResources(dir);
    const name = data.name || path.basename(dir);
    const manifest = normalizeManifest({
      kind: 'skill',
      name: String(name),
      displayName: String(data.display_name || data.displayName || name),
      summary: String(data.description || data.summary || firstLine(body)),
      description: truncate(body, 8000),
      version: String(data.version || (data.metadata && data.metadata.version) || ''),
      author: String(data.author || (data.metadata && data.metadata.author) || ''),
      authorSource: data.author ? 'frontmatter' : 'unknown',
      homepage: String(data.homepage || (data.metadata && data.metadata.homepage) || ''),
      license: String(data.license || (data.metadata && data.metadata.license) || ''),
      tags: toArray(data.tags || data.keywords || data.categories).concat(guessTags(text)),
      hosts: [],
      entry: 'SKILL.md',
      triggers: toArray(data.triggers || data.when_to_use || data.whenToUse),
      allowedTools: toArray(data['allowed-tools'] || data.allowedTools || data.tools),
      resources,
      metadata: { frontmatter: data, resourceCount: resources.length, hasFrontmatter: /^---\s*\r?\n/.test(text), bodyChars: body.length },
      evidence: [{ type: 'file', path: entry }]
    });
    return { manifest, body, entry, resources, raw: text };
  }
};

function toArray(v) {
  if (!v) return [];
  if (Array.isArray(v)) return v.map(String);
  if (typeof v === 'string') return v.split(/[,;]/).map((s) => s.trim()).filter(Boolean);
  return [];
}

function firstLine(text) {
  const line = String(text || '').split('\n').map((l) => l.replace(/^#+\s*/, '').trim()).find(Boolean);
  return line ? oneLine(line, 200) : '';
}

export function listResources(dir, max) {
  const out = [];
  const cap = max || 200;
  const stack = [{ d: dir, depth: 0 }];
  while (stack.length && out.length < cap) {
    const cur = stack.pop();
    let entries = [];
    try { entries = fs.readdirSync(cur.d, { withFileTypes: true }); } catch { continue; }
    for (const ent of entries) {
      if (ent.name === 'SKILL.md' || ent.name === 'skill.md') continue;
      if (ent.name === 'node_modules' || ent.name === '.git') continue;
      const full = path.join(cur.d, ent.name);
      const rel = path.relative(dir, full).replace(/\\/g, '/');
      if (ent.isDirectory()) {
        if (cur.depth < 4) stack.push({ d: full, depth: cur.depth + 1 });
      } else if (ent.isFile()) {
        let size = 0;
        try { size = fs.statSync(full).size; } catch { /* ignore */ }
        out.push({ path: rel, size });
      }
    }
  }
  return out;
}

export function skillHash(dir) {
  const entry = skillAdapter.detect(dir);
  if (!entry) return null;
  const text = readTextSafe(entry) || '';
  return sha256(text);
}
