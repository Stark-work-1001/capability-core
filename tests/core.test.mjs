import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseYaml, extractFrontmatter } from '../apps/desktop/src/main/core/yaml.js';
import { parseToml } from '../apps/desktop/src/main/core/toml.js';
import { compareVersions, versionKey, tokenize, normalizeName } from '../apps/desktop/src/main/core/util.js';
import { identityOf, normalizeManifest, fingerprintOf } from '../apps/desktop/src/main/core/manifest.js';
import { buildMatchQuery, segmentForIndex, phraseScore } from '../apps/desktop/src/main/core/text-index.js';

test('frontmatter 解析：多行描述 / 数组 / 嵌套', () => {
  const src = '---\nname: pdf\ndescription: >\n  Read PDFs\n  and extract.\nallowed-tools: [Read, Bash]\nmetadata:\n  author: alice\n  version: 1.2.3\n---\n\n# Body\n';
  const fm = extractFrontmatter(src);
  assert.equal(fm.data.name, 'pdf');
  assert.equal(fm.data.description, 'Read PDFs and extract.');
  assert.deepEqual(fm.data['allowed-tools'], ['Read', 'Bash']);
  assert.equal(fm.data.metadata.author, 'alice');
  assert.equal(fm.body.trim(), '# Body');
});

test('YAML：多行纯量（description 换行续写）', () => {
  const y = parseYaml('name: x\ndescription:\n  first line\n  second line\nallowed-tools:\n  - Read\n  - Bash\n');
  assert.equal(y.description, 'first line second line');
  assert.deepEqual(y['allowed-tools'], ['Read', 'Bash']);
});

test('YAML：mcpServers 结构', () => {
  const y = parseYaml('mcpServers:\n  demo:\n    type: stdio\n    command: node\n    args:\n      - a.js\n    enabled: true\n');
  assert.equal(y.mcpServers.demo.command, 'node');
  assert.deepEqual(y.mcpServers.demo.args, ['a.js']);
  assert.equal(y.mcpServers.demo.enabled, true);
});

test('TOML：表与数组', () => {
  const t = parseToml('[mcp_servers.demo]\ncommand = "node"\nargs = ["x.js", "--flag"]\n[mcp_servers.demo.env]\nA = "1"\n');
  assert.equal(t.mcp_servers.demo.command, 'node');
  assert.deepEqual(t.mcp_servers.demo.args, ['x.js', '--flag']);
  assert.equal(t.mcp_servers.demo.env.A, '1');
});

test('版本比较遵循 semver 规则', () => {
  assert.equal(compareVersions('1.2.3', '1.2.3'), 0);
  assert.equal(compareVersions('1.10.0', '1.9.9'), 1);
  assert.equal(compareVersions('2.0.0-rc.1', '2.0.0'), -1);
  assert.equal(compareVersions('v1.0.0', '1.0'), 0);
  assert.ok(versionKey('1.10.0') > versionKey('1.9.0'));
});

test('身份键：作者命名空间优先，其次名称', () => {
  assert.equal(identityOf({ kind: 'skill', name: 'PDF Tool', author: 'Alice' }).key, 'skill:alice/pdf-tool');
  assert.equal(identityOf({ kind: 'skill', name: 'PDF Tool' }).key, 'skill:pdf-tool');
  assert.equal(normalizeName('Foo_Bar/Baz'), 'foo-bar-baz');
});

test('清单归一：类型、标签去重、摘要单行', () => {
  const m = normalizeManifest({ kind: 'unknown', name: 'x', summary: 'a\nb', tags: ['a', 'a', 'b'] });
  assert.equal(m.kind, 'skill');
  assert.equal(m.summary, 'a b');
  assert.deepEqual(m.tags, ['a', 'b']);
});

test('指纹稳定：字段顺序不影响结果', () => {
  const a = fingerprintOf({ kind: 'skill', name: 'x', version: '1', summary: 's', entry: 'SKILL.md' }, 'body');
  const b = fingerprintOf({ entry: 'SKILL.md', summary: 's', version: '1', name: 'x', kind: 'skill' }, 'body');
  assert.equal(a, b);
  assert.notEqual(a, fingerprintOf({ kind: 'skill', name: 'x', version: '2' }, 'body'));
});

test('中文切分与短语匹配', () => {
  const seg = segmentForIndex('读取PDF文件');
  assert.ok(seg.includes('读 取'));
  assert.ok(buildMatchQuery('读取 pdf').includes('"'));
  assert.equal(phraseScore('支持读取PDF文件', '读取PDF'), 1);
  assert.ok(phraseScore('支持读取PDF文件', '读取Word') > 0);
  assert.ok(tokenize('mcp server 服务器').includes('mcp'));
});
