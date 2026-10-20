import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { skillAdapter } from '../apps/desktop/src/main/adapters/skill.js';
import { mcpAdapter } from '../apps/desktop/src/main/adapters/mcp.js';
import { pluginAdapter, parseAgentCard } from '../apps/desktop/src/main/adapters/plugin.js';

function tmp(name) {
  const dir = path.join(os.tmpdir(), 'cc-test-' + name + '-' + Date.now());
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

test('技能适配器：解析 SKILL.md 并列出资源', () => {
  const dir = tmp('skill');
  fs.writeFileSync(path.join(dir, 'SKILL.md'), '---\nname: demo-skill\ndescription: Demo skill for tests\nversion: 2.1.0\nauthor: Tester\nlicense: MIT\ntags: [test, demo]\n---\n\n# Demo\n\nDo things.\n');
  fs.mkdirSync(path.join(dir, 'scripts'));
  fs.writeFileSync(path.join(dir, 'scripts', 'run.py'), 'print(1)');
  const parsed = skillAdapter.parse(dir);
  assert.equal(parsed.manifest.name, 'demo-skill');
  assert.equal(parsed.manifest.version, '2.1.0');
  assert.equal(parsed.manifest.author, 'Tester');
  assert.ok(parsed.manifest.tags.includes('test'));
  assert.ok(parsed.manifest.tags.includes('demo'));
  assert.equal(parsed.manifest.metadata.hasFrontmatter, true);
  assert.ok(parsed.manifest.metadata.bodyChars > 5);
  assert.ok(parsed.resources.some((r) => r.path === 'scripts/run.py'));
});

test('技能适配器：无 frontmatter 时记录事实而非报错', () => {
  const dir = tmp('skill-nofm');
  fs.writeFileSync(path.join(dir, 'SKILL.md'), '# Plain skill\n\nNo metadata here.\n');
  const parsed = skillAdapter.parse(dir);
  assert.equal(parsed.manifest.metadata.hasFrontmatter, false);
  assert.equal(parsed.manifest.name, path.basename(dir));
});

test('MCP 配置适配器：识别三种常见键名', () => {
  const dir = tmp('mcp');
  const file = path.join(dir, 'mcp.json');
  fs.writeFileSync(file, JSON.stringify({ mcpServers: { a: { command: 'node', args: ['a.js'] }, b: { type: 'http', url: 'http://x' } } }));
  const items = mcpAdapter.parseConfigFile(file, 'dsh');
  assert.equal(items.length, 2);
  const a = items.find((i) => i.manifest.name === 'a');
  assert.equal(a.manifest.runtime.transport, 'stdio');
  assert.deepEqual(a.manifest.runtime.args, ['a.js']);
  const b = items.find((i) => i.manifest.name === 'b');
  assert.equal(b.manifest.runtime.transport, 'http');
});

test('MCP 配置适配器：TOML 中的 mcp_servers', () => {
  const dir = tmp('mcp-toml');
  const file = path.join(dir, 'config.toml');
  fs.writeFileSync(file, '[mcp_servers.demo]\ncommand = "python"\nargs = ["-m", "demo"]\n');
  const items = mcpAdapter.parseConfigFile(file, 'codex');
  assert.equal(items.length, 1);
  assert.equal(items[0].manifest.runtime.command, 'python');
});

test('插件适配器：DSH 插件包', () => {
  const dir = tmp('plugin');
  fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({
    name: 'dsh-demo', version: '0.5.1', description: 'demo plugin',
    author: { name: 'Bob' }, license: 'MIT',
    dsh: { client: { platform: 'web' }, bundle: { patch: './cordis.patch.yml' } }
  }));
  const parsed = pluginAdapter.parse(dir, 'dsh');
  assert.equal(parsed.manifest.name, 'dsh-demo');
  assert.ok(parsed.manifest.protocols.includes('dsh-plugin'));
  assert.equal(parsed.manifest.metadata.hasClient, true);
});

test('A2A Agent Card 与 OpenAPI 识别', () => {
  const dir = tmp('card');
  const a2a = path.join(dir, 'agent.json');
  fs.writeFileSync(a2a, JSON.stringify({ name: 'Helper', url: 'http://x', capabilities: {}, skills: [{ id: 's' }], description: 'd' }));
  const r1 = parseAgentCard(a2a, 'generic');
  assert.equal(r1.kind, 'agent');
  assert.ok(r1.protocols.includes('a2a'));
  const oa = path.join(dir, 'openapi.json');
  fs.writeFileSync(oa, JSON.stringify({ openapi: '3.0.0', info: { title: 'API', version: '1' }, paths: { '/a': {} } }));
  const r2 = parseAgentCard(oa, 'generic');
  assert.ok(r2.protocols.includes('openapi'));
  assert.equal(r2.name, 'API');
});