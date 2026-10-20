import fs from 'node:fs';
import path from 'node:path';
import { normalizeManifest } from '../core/manifest.js';
import { jsonParse, oneLine, readTextSafe, truncate } from '../core/util.js';
import { parseYaml } from '../core/yaml.js';
import { parseToml } from '../core/toml.js';

// MCP：两种形态
// 1) 配置形态（mcp.json / .mcp.json / config.toml 的 mcp_servers / claude_desktop_config.json 等）
// 2) 服务器包形态（含 package.json 或 pyproject.toml 且声明 MCP 依赖的工程目录）
export const mcpAdapter = {
  id: 'mcp',
  label: 'Model Context Protocol',
  protocols: ['mcp'],

  detectConfigFile(file) {
    const base = path.basename(file).toLowerCase();
    if (['mcp.json', '.mcp.json', 'mcp-config.json', 'claude_desktop_config.json', 'mcp_config.json', 'mcp-servers.json'].includes(base)) return 'json';
    if (base === 'config.toml') return 'toml';
    if (base === 'config.yaml' || base === 'config.yml' || base === 'mcp.yaml') return 'yaml';
    return null;
  },

  parseConfigFile(file, host) {
    const kind = this.detectConfigFile(file);
    if (!kind) return [];
    const text = readTextSafe(file);
    if (!text) return [];
    let data = {};
    if (kind === 'json') data = jsonParse(text, {});
    else if (kind === 'yaml') data = parseYaml(text);
    else if (kind === 'toml') data = parseToml(text);
    return this.serversFromObject(data, host, file);
  },

  serversFromObject(obj, host, file) {
    const out = [];
    const candidates = [];
    const push = (m) => candidates.push(m);
    if (obj && obj.mcpServers && typeof obj.mcpServers === 'object') push(obj.mcpServers);
    if (obj && obj.servers && typeof obj.servers === 'object' && !obj.mcpServers) push(obj.servers);
    if (obj && obj.mcp && obj.mcp.servers && typeof obj.mcp.servers === 'object') push(obj.mcp.servers);
    if (obj && obj.mcp_servers && typeof obj.mcp_servers === 'object') push(obj.mcp_servers);
    if (obj && obj['mcp.servers'] && typeof obj['mcp.servers'] === 'object') push(obj['mcp.servers']);
    if (obj && obj.mcpServers === undefined && obj.mcp_servers === undefined) {
      // 兼容裸配置：{ "name": { command | url } }
      const entries = Object.entries(obj || {}).filter(([k, v]) => v && typeof v === 'object' && (v.command || v.url || v.type));
      if (entries.length) push(Object.fromEntries(entries));
    }
    for (const group of candidates) {
      for (const [name, server] of Object.entries(group)) {
        if (!server || typeof server !== 'object') continue;
        if (name.startsWith('_')) continue;
        const transport = server.type || (server.url ? 'http' : 'stdio');
        const enabled = server.disabled === true ? false : server.enabled !== false;
        const manifest = normalizeManifest({
          kind: 'mcp',
          name,
          displayName: name,
          summary: oneLine(server.description || (server.command ? 'MCP stdio 服务：' + server.command : 'MCP 远端服务：' + (server.url || '')), 300),
          description: truncate(server.description || '', 4000),
          host,
          hosts: host ? [host] : [],
          version: String(server.version || ''),
          author: String(server.author || ''),
          protocol: transport,
          protocols: ['mcp', 'mcp-' + transport],
          entry: server.url || server.command || '',
          runtime: {
            transport,
            command: server.command || '',
            args: server.args || [],
            url: server.url || '',
            env: server.env || {},
            cwd: server.cwd || '',
            headers: server.headers || {},
            timeout: server.timeout || null,
            enabled,
            configured: server.configured !== false
          },
          tags: ['mcp', transport],
          install: { mode: 'config', configFile: file, serverName: name },
          metadata: { configFile: file, raw: server },
          evidence: [{ type: 'config', path: file, server: name }]
        });
        out.push({ manifest, body: JSON.stringify(server, null, 2), entry: file });
      }
    }
    return out;
  },

  // 服务器包形态：package.json 声明 @modelcontextprotocol/sdk 或 bin 名称含 mcp
  detectPackage(dir) {
    if (!fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) return null;
    const pkgFile = path.join(dir, 'package.json');
    if (fs.existsSync(pkgFile)) {
      const pkg = jsonParse(readTextSafe(pkgFile) || '{}', {});
      const deps = { ...(pkg.dependencies || {}), ...(pkg.devDependencies || {}) };
      const isMcp = Object.keys(deps).some((d) => d === '@modelcontextprotocol/sdk' || d.includes('modelcontextprotocol'))
        || /mcp/i.test(String(pkg.name || '')) || /mcp/i.test(String(pkg.description || ''));
      if (isMcp) return { type: 'node', file: pkgFile, data: pkg };
    }
    for (const pyFile of ['pyproject.toml', 'setup.py', 'requirements.txt']) {
      const full = path.join(dir, pyFile);
      if (fs.existsSync(full)) {
        const text = readTextSafe(full) || '';
        if (/mcp/i.test(text)) return { type: 'python', file: full, data: { text } };
      }
    }
    const readme = ['README.md', 'readme.md'].map((f) => path.join(dir, f)).find((f) => fs.existsSync(f));
    if (readme) {
      const text = (readTextSafe(readme) || '').slice(0, 6000);
      if (/model context protocol|mcp server/i.test(text)) return { type: 'readme', file: readme, data: { text } };
    }
    return null;
  },

  parsePackage(dir, host) {
    const det = this.detectPackage(dir);
    if (!det) return null;
    let name = path.basename(dir);
    let description = '';
    let version = '';
    let author = '';
    let license = '';
    let homepage = '';
    const scripts = {};
    let command = '';
    if (det.type === 'node') {
      name = det.data.name || name;
      description = det.data.description || '';
      version = det.data.version || '';
      author = typeof det.data.author === 'string' ? det.data.author : (det.data.author && det.data.author.name) || '';
      license = det.data.license || '';
      homepage = det.data.homepage || (det.data.repository && (det.data.repository.url || det.data.repository)) || '';
      Object.assign(scripts, det.data.scripts || {});
      const bin = det.data.bin;
      if (typeof bin === 'string') command = bin;
      else if (bin && typeof bin === 'object') command = Object.values(bin)[0];
    } else if (det.type === 'python') {
      const text = det.data.text || '';
      const m = text.match(/name\s*=\s*["']([^"']+)["']/);
      if (m) name = m[1];
      const d = text.match(/description\s*=\s*["']([^"']+)["']/);
      if (d) description = d[1];
      const v = text.match(/version\s*=\s*["']([^"']+)["']/);
      if (v) version = v[1];
    } else {
      const text = det.data.text || '';
      const first = text.split('\n').find((l) => l.startsWith('# '));
      if (first) description = first.replace(/^#\s*/, '');
    }
    const manifest = normalizeManifest({
      kind: 'mcp',
      name,
      displayName: name,
      summary: oneLine(description, 300),
      description: truncate(description, 4000),
      version,
      author,
      license,
      homepage,
      hosts: host ? [host] : [],
      protocols: ['mcp'],
      runtime: { transport: 'stdio', command: command || '', packageDir: dir, scripts },
      tags: ['mcp', det.type],
      install: { mode: 'package', dir },
      metadata: { packageFile: det.file, packageType: det.type },
      evidence: [{ type: 'file', path: det.file }]
    });
    return { manifest, body: description, entry: det.file };
  }
};

