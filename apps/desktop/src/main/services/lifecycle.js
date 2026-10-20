import fs from 'node:fs';
import path from 'node:path';
import { newId, now, jsonParse, sha256, removeDir, copyDir, writeFileAtomic, formatBytes } from '../core/util.js';
import { ASSET_DIR, TRASH_DIR, SNAPSHOT_DIR } from '../core/paths.js';

// 生命周期：启用 / 停用 / 删除（进回收站）/ 快照 / 回滚 / 导出。
// 原则：删除不销毁证据——物理文件进 .trash，数据库记录保留可恢复。
export class LifecycleService {
  constructor(ctx, services) {
    this.ctx = ctx;
    this.svc = services;
    this.db = ctx.db;
  }

  action(capabilityId, action, value) {
    const cap = this.db.get('SELECT * FROM capabilities WHERE id = ?', [capabilityId]);
    if (!cap) throw new Error('能力不存在');
    const inst = this.db.get('SELECT * FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [capabilityId]);
    const ts = now();
    switch (action) {
      case 'activate':
        this.db.run("UPDATE capabilities SET status='active', updated_at=? WHERE id=?", [ts, capabilityId]);
        this.ctx.bus.info('lifecycle', '已启用：' + cap.display_name, { capabilityId });
        return { ok: true, status: 'active' };
      case 'disable':
        this.db.run("UPDATE capabilities SET status='disabled', updated_at=? WHERE id=?", [ts, capabilityId]);
        this.ctx.bus.info('lifecycle', '已停用：' + cap.display_name, { capabilityId });
        return { ok: true, status: 'disabled' };
      case 'quarantine':
        this.db.run("UPDATE capabilities SET status='quarantined', updated_at=? WHERE id=?", [ts, capabilityId]);
        this.ctx.bus.warn('lifecycle', '已隔离：' + cap.display_name, { capabilityId });
        return { ok: true, status: 'quarantined' };
      case 'pin':
        this.db.run('UPDATE capabilities SET pinned=?, updated_at=? WHERE id=?', [value ? 1 : 0, ts, capabilityId]);
        return { ok: true, pinned: Boolean(value) };
      case 'snapshot':
        return this.snapshot(capabilityId, value || 'manual');
      case 'delete':
        return this.remove(capabilityId);
      case 'restore':
        return this.restoreDeleted(capabilityId);
      case 'export':
        return this.exportCapability(capabilityId);
      case 'verify':
        return this.verify(capabilityId);
      default:
        throw new Error('未知动作：' + action);
    }
  }

  snapshot(capabilityId, reason) {
    const inst = this.db.get('SELECT * FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [capabilityId]);
    if (!inst) throw new Error('没有安装实例可快照');
    const cap = this.db.get('SELECT * FROM capabilities WHERE id = ?', [capabilityId]);
    const id = newId('snap');
    const dir = path.join(SNAPSHOT_DIR, id);
    fs.mkdirSync(dir, { recursive: true });
    const raw = String(inst.path).split('#')[0];
    let storePath = '';
    if (fs.existsSync(raw) && fs.statSync(raw).isDirectory()) {
      storePath = path.join(dir, 'content');
      copyDir(raw, storePath);
    } else if (fs.existsSync(raw)) {
      storePath = path.join(dir, 'content');
      fs.mkdirSync(path.dirname(storePath), { recursive: true });
      fs.copyFileSync(raw, storePath);
    }
    writeFileAtomic(path.join(dir, 'manifest.json'), JSON.stringify({
      capabilityId, name: cap.canonical_name, version: cap.version, path: raw, createdAt: now(), reason: reason || 'manual'
    }, null, 2));
    fs.mkdirSync(path.dirname(path.join(SNAPSHOT_DIR, id)), { recursive: true });
    this.db.run('INSERT INTO backup_snapshots(id, capability_id, kind, path, store_path, hash, size, reason, created_at) VALUES (?,?,?,?,?,?,?,?,?)',
      [id, capabilityId, 'dir', raw, storePath, sha256(String(Date.now())), inst.size || 0, reason || 'manual', now()]);
    this.ctx.bus.info('lifecycle', '已创建快照：' + cap.display_name, { capabilityId, snapshotId: id });
    return { ok: true, snapshotId: id, storePath, size: inst.size };
  }

  remove(capabilityId) {
    const cap = this.db.get('SELECT * FROM capabilities WHERE id = ?', [capabilityId]);
    const insts = this.db.all('SELECT * FROM installations WHERE capability_id = ?', [capabilityId]);
    const ts = now();
    const moved = [];
    for (const inst of insts) {
      const raw = String(inst.path).split('#')[0];
      if (!fs.existsSync(raw)) continue;
      const dest = path.join(TRASH_DIR, newId('trash') + '-' + path.basename(raw));
      fs.mkdirSync(TRASH_DIR, { recursive: true });
      try { fs.renameSync(raw, dest); } catch (err) {
        try { copyDir(raw, dest); removeDir(raw); } catch (err2) { continue; }
      }
      moved.push({ from: raw, to: dest });
    }
    this.db.run("UPDATE capabilities SET status='archived', archive_path=?, updated_at=? WHERE id=?", [JSON.stringify(moved), ts, capabilityId]);
    this.ctx.bus.warn('lifecycle', '已删除（移入回收站）：' + cap.display_name, { capabilityId, moved });
    return { ok: true, status: 'archived', moved };
  }

  restoreDeleted(capabilityId) {
    const cap = this.db.get('SELECT * FROM capabilities WHERE id = ?', [capabilityId]);
    const moved = jsonParse(cap.archive_path || '[]', []);
    const restored = [];
    const errors = [];
    for (const m of moved) {
      if (!fs.existsSync(m.to)) { errors.push('回收站文件缺失：' + m.to); continue; }
      try {
        fs.mkdirSync(path.dirname(m.from), { recursive: true });
        try {
          fs.renameSync(m.to, m.from);
        } catch (renameErr) {
          // 跨盘或占用时退化为复制 + 删除
          if (fs.statSync(m.to).isDirectory()) copyDir(m.to, m.from); else fs.copyFileSync(m.to, m.from);
          removeDir(m.to);
        }
        restored.push(m.from);
      } catch (err) {
        errors.push(m.from + '：' + String(err && err.message));
      }
    }
    this.db.run("UPDATE capabilities SET status='active', archive_path='', updated_at=? WHERE id=?", [now(), capabilityId]);
    this.ctx.bus.info('lifecycle', '已从回收站恢复：' + cap.display_name, { capabilityId, restored });
    return { ok: restored.length > 0, restored, errors };
  }

  exportCapability(capabilityId) {
    const cap = this.svc.registry.getCapability(capabilityId);
    if (!cap) throw new Error('能力不存在');
    const dir = path.join(this.ctx.options.exportDir || path.join(ASSET_DIR, '..', 'exports'), cap.kind + '-' + cap.name.replace(/[^a-z0-9._-]/gi, '_'));
    fs.mkdirSync(dir, { recursive: true });
    const inst = cap.installations[0];
    if (inst) {
      const raw = String(inst.path).split('#')[0];
      if (fs.existsSync(raw)) {
        if (fs.statSync(raw).isDirectory()) copyDir(raw, path.join(dir, 'content'));
        else fs.copyFileSync(raw, path.join(dir, path.basename(raw)));
      }
    }
    writeFileAtomic(path.join(dir, 'capability-manifest.json'), JSON.stringify({
      capability: cap, exportedAt: now(), schema: 'capability-core.export/v1'
    }, null, 2));
    return { ok: true, dir };
  }

  verify(capabilityId) {
    const cap = this.svc.registry.getCapability(capabilityId);
    if (!cap) throw new Error('能力不存在');
    const issues = [];
    if (!cap.installations.length) issues.push('没有安装实例');
    for (const inst of cap.installations) {
      const raw = String(inst.path).split('#')[0];
      if (!fs.existsSync(raw)) issues.push('路径不存在：' + raw);
    }
    if (cap.kind === 'mcp') {
      const m = cap.installations[0] && cap.installations[0].manifest;
      if (m && m.runtime && m.runtime.transport === 'stdio' && !m.runtime.command) issues.push('缺少启动命令');
    }
    const state = issues.length ? 'invalid' : 'verified';
    this.db.run('UPDATE capabilities SET verify_state=?, verify_message=?, updated_at=? WHERE id=?',
      [state, issues.join('；') || '校验通过', now(), capabilityId]);
    this.ctx.bus.info('lifecycle', '校验：' + cap.displayName + ' → ' + state, { capabilityId, issues });
    return { ok: !issues.length, state, issues };
  }

  trash() {
    const rows = this.db.all("SELECT * FROM capabilities WHERE status = 'archived' AND archive_path != '' ORDER BY updated_at DESC LIMIT 100");
    return rows.map((r) => ({ id: r.id, name: r.display_name, kind: r.kind, archivedAt: r.updated_at, files: jsonParse(r.archive_path, []) }));
  }
}
