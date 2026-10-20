import fs from 'node:fs';
import path from 'node:path';
import { newId, now, jsonParse, readTextSafe, writeFileAtomic, sha256 } from '../core/util.js';
import { skillAdapter } from '../adapters/skill.js';
import { parseYaml, extractFrontmatter, stringifyYaml } from '../core/yaml.js';

// 故障识别与维修：
// 中枢负责「诊断 / 定位 / 生成修复方案」，执行由接入的智能体或内置安全修复完成。
// 每条故障都有 deterministic 诊断证据，修复动作分为 auto（内置安全）、agent（派单）、manual。
const RULES = [
  {
    code: 'skill-missing-frontmatter',
    severity: 'error',
    title: 'SKILL.md 缺少 frontmatter',
    detect(ctx, cap) {
      if (cap.kind !== 'skill' || !cap.inst) return false;
      const m = jsonParse(cap.inst.manifest, {});
      const meta = m.metadata || {};
      if (typeof meta.hasFrontmatter === 'boolean') return !meta.hasFrontmatter;
      return false;
    },
    autofix: true,
    fix(svc, cap) { return svc._addFrontmatter(cap); }
  },
  {
    code: 'skill-missing-description',
    severity: 'warning',
    title: '技能缺少 description，智能体无法判断何时使用',
    detect(ctx, cap) {
      if (cap.kind !== 'skill' || !cap.inst) return false;
      const m = jsonParse(cap.inst.manifest, {});
      if (!m.metadata || m.metadata.hasFrontmatter !== true) return false;
      return !String(m.summary || '').trim();
    },
    autofix: true,
    fix(svc, cap) { return svc._addDescription(cap); }
  },
  {
    code: 'skill-file-missing',
    severity: 'critical',
    title: '技能入口文件不存在',
    detect(ctx, cap) {
      if (cap.kind !== 'skill' || !cap.inst) return false;
      const base = String(cap.inst.path || '').split('#')[0];
      if (!base) return false;
      if (!fs.existsSync(base)) return true;
      try { if (fs.statSync(base).isDirectory()) return !skillAdapter.detect(base); } catch { return true; }
      return false;
    },
    autofix: false
  },
  {
    code: 'skill-empty-body',
    severity: 'error',
    title: '技能正文为空，智能体加载后无法执行',
    detect(ctx, cap) {
      if (cap.kind !== 'skill' || !cap.inst) return false;
      const m = jsonParse(cap.inst.manifest, {});
      // 只有在正文完全为空时才判故障；过短但非空的正文交给人工判断
      if (m.metadata && typeof m.metadata.bodyChars === 'number') return m.metadata.bodyChars === 0;
      return false;
    },
    autofix: false
  },
  {
    code: 'mcp-command-missing',
    severity: 'error',
    title: 'MCP 启动命令指向不存在的可执行文件',
    detect(ctx, cap) {
      if (cap.kind !== 'mcp' || !cap.inst) return false;
      const m = jsonParse(cap.inst.manifest, {});
      const rt = m.runtime || {};
      const cmd = rt.command || '';
      return Boolean(cmd) && path.isAbsolute(cmd) && !fs.existsSync(cmd);
    },
    autofix: false
  },
  {
    code: 'duplicate-capability',
    severity: 'warning',
    title: '存在重复能力（同身份多实例）',
    detect(ctx, cap) {
      return ctx.db.get('SELECT COUNT(*) AS c FROM installations WHERE capability_id = ?', [cap.id]).c > 1;
    },
    autofix: false
  },
  {
    code: 'plugin-missing-main',
    severity: 'error',
    title: '插件声明的入口文件不存在',
    detect(ctx, cap) {
      if (cap.kind !== 'plugin' || !cap.inst) return false;
      const m = jsonParse(cap.inst.manifest, {});
      const main = (m.runtime && m.runtime.main) || '';
      if (!main) return false;
      return !fs.existsSync(path.join(cap.inst.path, main));
    },
    autofix: false
  },
  {
    code: 'path-inaccessible',
    severity: 'critical',
    title: '安装路径不可访问',
    detect(ctx, cap) { return cap.inst && cap.inst.path && !fs.existsSync(String(cap.inst.path).split('#')[0]); },
    autofix: false
  },
  {
    code: 'parse-error',
    severity: 'error',
    title: '清单解析失败',
    detect(ctx, cap) { return Boolean((jsonParse(cap.metadata, {}).parseError)); },
    autofix: false
  }
];

export class FaultService {
  constructor(ctx) {
    this.ctx = ctx;
    this.db = ctx.db;
    this.bus = ctx.bus;
  }

  diagnose(options) {
    const opts = options || {};
    const caps = this.db.all("SELECT * FROM capabilities WHERE status != 'archived' AND status != 'disabled'");
    let detected = 0; let closed = 0;
    for (const c of caps) {
      const inst = this.db.get('SELECT * FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [c.id]);
      const cap = { ...c, inst };
      const triggered = [];
      for (const rule of RULES) {
        let hit = false;
        try { hit = rule.detect(this.ctx, cap); } catch { hit = false; }
        if (!hit) continue;
        triggered.push(rule);
        const existing = this.db.get("SELECT * FROM faults WHERE capability_id = ? AND code = ? AND status != 'resolved'", [c.id, rule.code]);
        if (!existing) {
          this.db.run(
            'INSERT INTO faults(id, capability_id, installation_id, severity, code, title, detail, evidence, status, auto_fixable, detected_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)',
            [newId('flt'), c.id, inst ? inst.id : null, rule.severity, rule.code, rule.title,
              this.explain(rule, cap), JSON.stringify({ path: inst ? inst.path : '', host: inst ? inst.host : '', code: rule.code }),
              'open', rule.autofix ? 1 : 0, now(), now()]);
          detected++;
          this.bus.warn('fault', rule.title + '：' + c.display_name, { capabilityId: c.id, code: rule.code });
        }
      }
      // 已消失的故障自动标记 resolved
      const open = this.db.all("SELECT * FROM faults WHERE capability_id = ? AND status != 'resolved'", [c.id]);
      for (const f of open) {
        if (!triggered.some((r) => r.code === f.code)) {
          this.db.run("UPDATE faults SET status='resolved', resolved_at=?, updated_at=? WHERE id=?", [now(), now(), f.id]);
          closed++;
        }
      }
      const health = triggered.some((r) => r.severity === 'critical') ? 'broken'
        : triggered.some((r) => r.severity === 'error') ? 'broken'
        : triggered.length ? 'warning' : (this.db.get("SELECT COUNT(*) AS c FROM faults WHERE capability_id=? AND status='open'", [c.id]).c ? 'warning' : 'healthy');
      this.db.run('UPDATE capabilities SET health=?, updated_at=updated_at WHERE id=?', [health, c.id]);
    }
    // 无故障的能力置为 healthy
    this.db.run("UPDATE capabilities SET health='healthy' WHERE id NOT IN (SELECT DISTINCT capability_id FROM faults WHERE status != 'resolved' AND capability_id IS NOT NULL) AND status = 'active'");
    if (detected || closed) this.bus.info('fault', '故障诊断：新增 ' + detected + '，自动关闭 ' + closed);
    return { detected, closed, open: this.db.get("SELECT COUNT(*) AS c FROM faults WHERE status != 'resolved'").c };
  }

  explain(rule, cap) {
    return rule.title + '（能力：' + cap.display_name + '，路径：' + (cap.inst ? cap.inst.path : '未知') + '）';
  }

  _addFrontmatter(cap, svc) {
    const file = resolveEntryFile(cap.inst.path);
    const content = cap.inst.content || readTextSafe(file) || '';
    const name = cap.canonical_name;
    const desc = (cap.summary || '能力：' + cap.display_name).replace(/\n/g, ' ');
    const fm = '---\nname: ' + name + '\ndescription: ' + JSON.stringify(desc) + '\n---\n\n';
    writeFileAtomic(file, fm + content);
    return { message: '已补充 frontmatter', before: content, after: fm + content, file };
  }

  _addDescription(cap, svc) {
    const file = resolveEntryFile(cap.inst.path);
    const content = cap.inst.content || readTextSafe(file) || '';
    const fm = extractFrontmatter(content);
    fm.data.description = cap.summary || '能力：' + cap.display_name;
    const header = '---\n' + Object.entries(fm.data).map(([k, v]) => k + ': ' + (typeof v === 'string' && /[:#]/.test(v) ? JSON.stringify(v) : String(v))).join('\n') + '\n---\n';
    const next = header + fm.body.replace(/^\n+/, '\n');
    writeFileAtomic(file, next);
    return { message: '已补充 description', before: content, after: next, file };
  }

  list(filter) {
    const f = filter || {};
    const where = []; const params = [];
    if (f.status && f.status !== 'all') { where.push('f.status = ?'); params.push(f.status); }
    if (f.severity) { where.push('f.severity = ?'); params.push(f.severity); }
    const sql = 'SELECT f.*, c.display_name, c.kind FROM faults f LEFT JOIN capabilities c ON c.id = f.capability_id ' +
      (where.length ? 'WHERE ' + where.join(' AND ') : '') + ' ORDER BY CASE f.severity WHEN \'critical\' THEN 0 WHEN \'error\' THEN 1 WHEN \'warning\' THEN 2 ELSE 3 END, f.detected_at DESC LIMIT 300';
    return this.db.all(sql, params).map((r) => ({
      id: r.id, capabilityId: r.capability_id, capabilityName: r.display_name, kind: r.kind,
      severity: r.severity, code: r.code, title: r.title, detail: r.detail, evidence: jsonParse(r.evidence, {}),
      status: r.status, autoFixable: Boolean(r.auto_fixable), fixKind: r.fix_kind, dispatchAgent: r.dispatch_agent,
      dispatchNote: r.dispatch_note, detectedAt: r.detected_at, resolvedAt: r.resolved_at
    }));
  }

  // 自动修复（只有规则声明 autofix 的才执行），修复前自动备份
  async autoFix(faultId) {
    const f = this.db.get('SELECT * FROM faults WHERE id = ?', [faultId]);
    if (!f) throw new Error('故障不存在');
    if (!f.auto_fixable) throw new Error('该故障不支持自动修复，请派单给智能体');
    const rule = RULES.find((r) => r.code === f.code);
    if (!rule || !rule.fix) throw new Error('没有可用的修复实现');
    const cap = this.db.get('SELECT * FROM capabilities WHERE id = ?', [f.capability_id]);
    const inst = this.db.get('SELECT * FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [f.capability_id]);
    if (!inst) throw new Error('缺少安装实例');
    const entryFile = resolveEntryFile(inst.path);
    const before = readTextSafe(entryFile) || '';
    const backupId = this.backupFile(entryFile, before, 'autofix:' + f.code);
    const result = rule.fix(this, { ...cap, inst });
    const after = readTextSafe(entryFile) || '';
    this.db.run('UPDATE installations SET content=?, body_hash=?, updated_at=? WHERE id=?', [after, sha256(after), now(), inst.id]);
    this.db.run('INSERT INTO repairs(id, fault_id, capability_id, actor, action, patch, before_hash, after_hash, result, message, created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)',
      [newId('rep'), faultId, f.capability_id, 'system', 'auto-fix:' + f.code, JSON.stringify({ file: insPath(inst), backupId }), sha256(before), sha256(after), 'applied', result.message, now()]);
    this.db.run("UPDATE faults SET status='resolved', resolved_at=?, updated_at=?, repair_log=? WHERE id=?",
      [now(), now(), JSON.stringify([{ at: now(), action: 'auto-fix', message: result.message }]), faultId]);
    this.db.run("UPDATE capabilities SET health='healthy', verify_state='verified', verify_message=? WHERE id=?", [result.message, f.capability_id]);
    this.bus.info('repair', '自动修复完成：' + f.title, { capabilityId: f.capability_id, faultId });
    return { ok: true, message: result.message, backupId, file: insPath(inst) };
  }

  // 派单给智能体（创建工单 + 生成结构化任务提示）
  dispatch(faultId, agentId, note) {
    const f = this.db.get('SELECT * FROM faults WHERE id = ?', [faultId]);
    if (!f) throw new Error('故障不存在');
    const cap = this.db.get('SELECT * FROM capabilities WHERE id = ?', [f.capability_id]);
    const inst = this.db.get('SELECT * FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [f.capability_id]);
    const prompt = [
      '# 能力维修工单',
      '',
      '- 能力：' + (cap ? cap.display_name : '未知') + '（' + (cap ? cap.kind : '') + '）',
      '- 路径：' + (inst ? inst.path : '未知'),
      '- 故障代码：' + f.code,
      '- 严重级别：' + f.severity,
      '- 现象：' + f.title,
      '- 证据：' + (f.evidence || '{}'),
      '',
      '## 要求',
      '1. 只修改上述路径下的文件，不要改动中枢数据库；',
      '2. 修复后调用能力核心（capability-core）的 capability.submit_patch 工具提交补丁（含完整文件内容与说明）；',
      '3. 中枢会校验摘要并记录维修历史，未通过校验的补丁不会落盘。',
      note ? '\n## 备注\n' + note : ''
    ].join('\n');
    this.db.run("UPDATE faults SET status='dispatched', dispatch_agent=?, dispatch_note=?, updated_at=? WHERE id=?", [agentId || 'auto', note || '', now(), faultId]);
    this.db.run('INSERT INTO repairs(id, fault_id, capability_id, actor, agent_id, action, result, message, created_at) VALUES (?,?,?,?,?,?,?,?,?)',
      [newId('rep'), faultId, f.capability_id, 'agent', agentId || null, 'dispatch', 'pending', prompt.slice(0, 4000), now()]);
    this.bus.info('repair', '故障已派单：' + f.title, { faultId, agentId });
    return { ok: true, prompt, faultId };
  }

  // 智能体回传补丁：校验路径边界 + 备份 + 写入 + 记录
  submitPatch(patch) {
    const p = patch || {};
    if (!p.faultId) throw new Error('缺少 faultId');
    const f = this.db.get('SELECT * FROM faults WHERE id = ?', [p.faultId]);
    if (!f) throw new Error('故障不存在');
    const inst = this.db.get('SELECT * FROM installations WHERE capability_id = ? ORDER BY updated_at DESC LIMIT 1', [f.capability_id]);
    const allowedRoot = inst ? path.resolve(String(inst.path).split('#')[0]) : null;
    const target = path.resolve(p.file || '');
    if (!allowedRoot || (target !== allowedRoot && !target.startsWith(path.dirname(allowedRoot) + path.sep))) {
      throw new Error('补丁路径越界：' + target);
    }
    const before = readTextSafe(target) || '';
    const backupId = this.backupFile(target, before, 'patch:' + f.code);
    writeFileAtomic(target, String(p.content || ''));
    const after = readTextSafe(target) || '';
    this.db.run('UPDATE installations SET content=?, body_hash=?, updated_at=? WHERE id=?', [after, sha256(after), now(), inst.id]);
    this.db.run('INSERT INTO repairs(id, fault_id, capability_id, actor, agent_id, action, patch, before_hash, after_hash, result, message, created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)',
      [newId('rep'), f.id, f.capability_id, 'agent', p.agentId || null, 'patch', JSON.stringify({ file: target, backupId }), sha256(before), sha256(after), 'applied', p.message || '', now()]);
    this.db.run("UPDATE faults SET status='resolved', resolved_at=?, updated_at=? WHERE id=?", [now(), now(), f.id]);
    this.bus.info('repair', '补丁已应用：' + f.title, { faultId: f.id });
    return { ok: true, backupId, file: target };
  }

  backupFile(file, content, reason) {
    const id = newId('bak');
    const dir = path.join(this.ctx.options.backupDir, 'files');
    fs.mkdirSync(dir, { recursive: true });
    const store = path.join(dir, id + '-' + path.basename(file));
    writeFileAtomic(store, content);
    const hash = sha256(content);
    this.db.run('INSERT INTO backup_snapshots(id, capability_id, kind, path, store_path, hash, size, reason, created_at) VALUES (?,?,?,?,?,?,?,?,?)',
      [id, null, 'file', file, store, hash, content.length, reason, now()]);
    return id;
  }

  repairs(faultId) {
    const rows = faultId
      ? this.db.all('SELECT * FROM repairs WHERE fault_id = ? ORDER BY created_at DESC', [faultId])
      : this.db.all('SELECT * FROM repairs ORDER BY created_at DESC LIMIT 200');
    return rows.map((r) => ({
      id: r.id, faultId: r.fault_id, capabilityId: r.capability_id, actor: r.actor, agentId: r.agent_id,
      action: r.action, patch: jsonParse(r.patch, {}), beforeHash: r.before_hash, afterHash: r.after_hash,
      result: r.result, message: r.message, createdAt: r.created_at
    }));
  }

  stats() {
    const bySeverity = this.db.all("SELECT severity, COUNT(*) c FROM faults WHERE status != 'resolved' GROUP BY severity");
    const byStatus = this.db.all('SELECT status, COUNT(*) c FROM faults GROUP BY status');
    return { bySeverity, byStatus, open: this.db.get("SELECT COUNT(*) c FROM faults WHERE status != 'resolved'").c };
  }
}

function insPath(inst) {
  return resolveEntryFile(inst && inst.path);
}

// 安装路径可能是目录（技能/插件包）或文件（MCP 配置项）；
// 技能修复必须落到目录内的 SKILL.md。
export function resolveEntryFile(rawPath) {
  const raw = String(rawPath || '').split('#')[0];
  if (!raw) return raw;
  try {
    if (fs.statSync(raw).isDirectory()) {
      for (const name of ['SKILL.md', 'skill.md', 'README.md', 'plugin.json', 'package.json']) {
        const candidate = path.join(raw, name);
        if (fs.existsSync(candidate)) return candidate;
      }
      return path.join(raw, 'SKILL.md');
    }
  } catch { /* 不存在时按文件处理 */ }
  return raw;
}
