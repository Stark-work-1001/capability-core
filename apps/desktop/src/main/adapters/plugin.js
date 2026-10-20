import fs from 'node:fs';
import path from 'node:path';
import { normalizeManifest, guessTags } from '../core/manifest.js';
import { jsonParse, oneLine, readTextSafe, truncate } from '../core/util.js';
import { parseYaml, extractFrontmatter } from '../core/yaml.js';
import { listResources } from './skill.js';

// 插件形态：DSH 插件（package.json + dsh.bundle）、Claude/Codex 插件（plugin.json / .claude-plugin）
export const pluginAdapter = {
  id: 'plugin',
  label: 'Agent Plugin',
  protocols: ['dsh-plugin', 'claude-plugin', 'codex-plugin'],

  detect(dir) {
    if (!fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) return null;
    for (const f of ['plugin.json', '.claude-plugin/plugin.json', '.codex-plugin/plugin.json']) {
      const full = path.join(dir, f);
      if (fs.existsSync(full)) return { kind: 'plugin-json', file: full };
    }
    const pkg = path.join(dir, 'package.json');
    if (fs.existsSync(pkg)) {
      const data = jsonParse(readTextSafe(pkg) || '{}', {});
      if (data.dsh || data.claude || data.openai || /dsh-|claude-|codex-/i.test(String(data.name || ''))) {
        return { kind: 'package', file: pkg, data };
      }
    }
    const mcp = path.join(dir, '.mcp.json');
    if (fs.existsSync(mcp)) return { kind: 'mcp-plugin', file: mcp };
    return null;
  },

  parse(dir, host) {
    const det = this.detect(dir);
    if (!det) return null;
    let data = {};
    if (det.kind === 'plugin-json') data = jsonParse(readTextSafe(det.file) || '{}', {});
    else if (det.kind === 'package') data = det.data || jsonParse(readTextSafe(det.file) || '{}', {});
    else data = jsonParse(readTextSafe(det.file) || '{}', {});
    const pkg = det.kind === 'package' ? data : jsonParse(readTextSafe(path.join(dir, 'package.json')) || '{}', {});
    const name = data.name || pkg.name || path.basename(dir);
    const authorRaw = data.author || pkg.author;
    const author = typeof authorRaw === 'string' ? authorRaw : (authorRaw && authorRaw.name) || '';
    const description = data.description || pkg.description || '';
    const version = data.version || pkg.version || '';
    const homepage = data.homepage || (data.repository && (data.repository.url || data.repository)) || pkg.homepage || '';
    const license = data.license || pkg.license || '';
    const dshMeta = (pkg.dsh) || (data.dsh) || null;
    const protocols = ['plugin'];
    if (dshMeta) protocols.push('dsh-plugin');
    if (data.claude || fs.existsSync(path.join(dir, '.claude-plugin'))) protocols.push('claude-plugin');
    if (data.openai || fs.existsSync(path.join(dir, '.codex-plugin'))) protocols.push('codex-plugin');
    const resources = listResources(dir, 300);
    const skillFiles = resources.filter((r) => /(^|\/)SKILL\.md$/i.test(r.path));
    const mcpFiles = resources.filter((r) => /(^|\/)(mcp|server)\.json$|(^|\/)\.mcp\.json$/i.test(r.path));
    const readme = resources.find((r) => /readme\.md$/i.test(r.path));
    let body = '';
    if (readme) body = readTextSafe(path.join(dir, readme.path)) || '';
    const manifest = normalizeManifest({
      kind: 'plugin',
      name,
      displayName: data.displayName || data.title || name,
      summary: oneLine(description, 300),
      description: truncate(body || description, 12000),
      version,
      author,
      authorSource: author ? (typeof authorRaw === 'string' ? 'manifest' : 'manifest') : 'unknown',
      homepage,
      license,
      hosts: host ? [host] : [],
      protocols,
      tags: [...guessTags(description + ' ' + name), 'plugin'],
      entry: path.relative(dir, det.file).replace(/\\/g, '/'),
      resources,
      runtime: { packageManager: pkg.packageManager || 'npm', main: pkg.main || '', engines: pkg.engines || {} },
      metadata: {
        dsh: dshMeta,
        skillCount: skillFiles.length,
        mcpCount: mcpFiles.length,
        hasClient: Boolean(dshMeta && dshMeta.client),
        detectKind: det.kind
      },
      evidence: [{ type: 'file', path: det.file }]
    });
    return { manifest, body, entry: det.file, resources };
  }
};

// MCP 服务器工程目录：.mcp/<name>
export const mcpProjectAdapter = {
  id: 'mcp-project',
  label: 'MCP 服务端工程',
  protocols: ['mcp'],
  detect(dir) {
    if (!fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) return null;
    if (fs.existsSync(path.join(dir, 'SKILL.md'))) return null;
    const candidates = ['package.json', 'pyproject.toml', 'setup.py', 'go.mod', 'Dockerfile', 'README.md'];
    return candidates.some((f) => fs.existsSync(path.join(dir, f))) ? dir : null;
  }
};

export function parseAgentCard(file, host) {
  // A2A Agent Card 或 OpenAPI 声明
  const text = readTextSafe(file);
  if (!text) return null;
  const data = jsonParse(text, null);
  if (!data) return null;
  if (data.capabilities && (data.name || data.url) && data.skills) {
    return {
      kind: 'agent',
      protocols: ['a2a'],
      name: data.name || path.basename(path.dirname(file)),
      summary: data.description || '',
      description: data.description || '',
      version: data.version || '',
      author: data.provider && data.provider.organization || '',
      homepage: data.url || '',
      entry: file,
      skills: data.skills || []
    };
  }
  if (data.openapi || data.swagger) {
    return {
      kind: 'mcp',
      protocols: ['openapi'],
      name: data.info && data.info.title || path.basename(path.dirname(file)),
      summary: data.info && data.info.description || '',
      description: data.info && data.info.description || '',
      version: data.info && data.info.version || '',
      author: data.info && data.info.contact && data.info.contact.name || '',
      homepage: data.servers && data.servers[0] && data.servers[0].url || '',
      entry: file,
      operations: Object.keys(data.paths || {}).length
    };
  }
  return null;
}
