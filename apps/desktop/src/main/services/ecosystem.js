import fs from 'node:fs';
import path from 'node:path';
import { now, newId, jsonParse, writeFileAtomic, readTextSafe } from '../core/util.js';
import { hostHome } from './discovery.js';
import { parseYaml } from '../core/yaml.js';
import { parseToml } from '../core/toml.js';

// 生态同步：把中枢纳管的能力「推送」到各智能体原生配置，或从原生配置「拉取」。
// 写操作一律先备份（backup_snapshots），可回滚。
export class EcosystemService {
  constructor(ctx) {
    this.ctx = ctx;
    this.db = ctx.db;
    this.bus = ctx.bus;
  }

  targets() {
    const out = [];
    const add = (host, label, file, format, writeable) => {
      if (file && fs.existsSync(path.dirname(file))) out.push({ host, label, file, format, writeable });
    };
    const dsh = hostHome('dsh');
    if (dsh) {
      add('dsh', 'DSH 插件清单', path.join(dsh, 'profiles', 'desktop', 'package.json'), 'dsh-profile', true);
      add('dsh', 'DSH 能力配置', path.join(dsh, 'capability-core-mcp.json'), 'mcp-json', true);
    }
    const codex = hostHome('codex');
    if (codex) add('codex', 'Codex MCP 配置', path.join(codex, 'config.toml'), 'toml', false);
    const minimax = hostHome('minimax');
    if (minimax) add('minimax', 'MiniMax MCP 配置', path.join(minimax, 'mcp.json'), 'mcp-json', true);
    const claude = hostHome('claude');
    if (claude) add('claude', 'Claude 配置', path.join(claude, 'settings.json'), 'mcp-json', true);
    add('asset-center', '中枢自身 MCP 配置', path.join(path.dirname(this.ctx.options.home || '.'), 'capability-core-mcp.json'), 'mcp-json', true);
    return out;
  }

  // 为某个智能体生成 MCP 配置片段（不直接写盘，先预览）
  previewMcp(host, options) {
    const opts = options || {};
    const port = opts.port || 18297;
    const config = {
      mcpServers: {
        'capability-core': {
          type: 'streamableHttp',
          url: 'http://127.0.0.1:' + port + '/mcp',
          description: '能力核心：能力检索、读取、调用与故障上报'
        }
      }
    };
    return { host, config, text: JSON.stringify(config, null, 2) };
  }

  // 备份 + 写入
  applyMcp(host, options) {
    const target = this.targets().find((t) => t.host === host && t.format === 'mcp-json' && t.writeable);
    if (!target) throw new Error('该智能体没有可写的 MCP 配置：' + host);
    const before = readTextSafe(target.file) || '{}';
    const backupId = this.backup(target.file, before, 'ecosystem-sync');
    const preview = this.previewMcp(host, options);
    let data = {};
    try { data = JSON.parse(before); } catch { data = {}; }
    data.mcpServers = { ...(data.mcpServers || {}), ...preview.config.mcpServers };
    writeFileAtomic(target.file, JSON.stringify(data, null, 2));
    this.bus.info('ecosystem', '已写入 ' + target.label + ' 的 MCP 配置', { file: target.file, backupId });
    return { ok: true, file: target.file, backupId };
  }

  backup(file, content, reason) {
    const id = newId('bak');
    const store = path.join(this.ctx.options.backupDir, id + path.extname(file));
    fs.mkdirSync(path.dirname(store), { recursive: true });
    writeFileAtomic(store, content);
    const sha = simpleHash(content);
    this.db.run('INSERT INTO backup_snapshots(id, capability_id, kind, path, store_path, hash, size, reason, created_at) VALUES (?,?,?,?,?,?,?,?,?)',
      [id, null, 'file', file, store, sha, content.length, reason, now()]);
    return id;
  }

  restore(backupId) {
    const row = this.db.get('SELECT * FROM backup_snapshots WHERE id = ?', [backupId]);
    if (!row) throw new Error('备份不存在');
    const content = readTextSafe(row.store_path);
    if (content === null) throw new Error('备份文件丢失：' + row.store_path);
    writeFileAtomic(row.path, content);
    this.bus.info('ecosystem', '已回滚 ' + row.path, { backupId });
    return { ok: true, path: row.path };
  }

  backups() {
    return this.db.all('SELECT * FROM backup_snapshots ORDER BY created_at DESC LIMIT 200').map((r) => ({
      id: r.id, path: r.path, storePath: r.store_path, size: r.size, reason: r.reason, createdAt: r.created_at
    }));
  }
}

function simpleHash(s) {
  let h = 0;
  for (let i = 0; i < s.length; i++) { h = (h * 31 + s.charCodeAt(i)) | 0; }
  return 'h' + (h >>> 0).toString(16);
}
