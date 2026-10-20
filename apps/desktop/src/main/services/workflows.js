import { newId, now, jsonParse } from '../core/util.js';

// 工作流：把「扫描 → 诊断 → 去重 → 更新 → 索引」串成可观察、可复跑的流程。
// 每个步骤都记录开始/结束/结果，前端「工作流」页直接展示。
export class WorkflowService {
  constructor(ctx, services) {
    this.ctx = ctx;
    this.svc = services;
    this.db = ctx.db;
    this.seeds();
  }

  seeds() {
    const exist = this.db.get('SELECT COUNT(*) AS c FROM workflows').c;
    if (exist) return;
    const ts = now();
    const defs = [
      {
        name: '全量巡检', kind: 'seed',
        description: '扫描全部来源 → 故障诊断 → 去重识别 → 重建检索索引。日常维护的主流程。',
        steps: [
          { type: 'scan', label: '扫描全部来源' },
          { type: 'diagnose', label: '故障诊断' },
          { type: 'dedupe', label: '重复能力识别' },
          { type: 'reindex', label: '重建检索索引' },
          { type: 'notify', label: '汇总结果' }
        ]
      },
      {
        name: '能力入库', kind: 'seed',
        description: '导入外部能力目录 → 扫描能力资产库来源 → 诊断 → 去重。',
        steps: [
          { type: 'scan', label: '扫描能力资产库', source: 'asset-center' },
          { type: 'diagnose', label: '故障诊断' },
          { type: 'dedupe', label: '重复能力识别' }
        ]
      },
      {
        name: '更新巡检', kind: 'seed',
        description: '检查所有插件/技能的上游新版本，并列出可更新项。',
        steps: [
          { type: 'updates', label: '更新检查' },
          { type: 'notify', label: '汇总可更新项' }
        ]
      },
      {
        name: '智能体同步', kind: 'seed',
        description: '识别运行中的智能体 → 扫描其能力目录 → 刷新在线状态。',
        steps: [
          { type: 'agents', label: '识别智能体进程' },
          { type: 'scan', label: '扫描来源' }
        ]
      }
    ];
    for (const d of defs) {
      this.db.run('INSERT INTO workflows(id, name, kind, description, steps, schedule, enabled, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?)',
        [newId('wfl'), d.name, d.kind, d.description, JSON.stringify(d.steps), '', 1, ts, ts]);
    }
  }

  list() {
    return this.db.all('SELECT * FROM workflows ORDER BY created_at ASC').map((w) => ({
      id: w.id, name: w.name, kind: w.kind, description: w.description, steps: jsonParse(w.steps, []),
      schedule: w.schedule, enabled: Boolean(w.enabled), createdAt: w.created_at, updatedAt: w.updated_at,
      lastRun: this.db.get('SELECT * FROM workflow_runs WHERE workflow_id = ? ORDER BY started_at DESC LIMIT 1', [w.id])
    })).map((w) => ({
      ...w,
      lastRun: w.lastRun ? {
        id: w.lastRun.id, status: w.lastRun.status, startedAt: w.lastRun.started_at, endedAt: w.lastRun.ended_at,
        summary: jsonParse(w.lastRun.summary, {}), steps: jsonParse(w.lastRun.steps, [])
      } : null
    }));
  }

  async run(id, trigger) {
    const w = this.db.get('SELECT * FROM workflows WHERE id = ?', [id]);
    if (!w) throw new Error('工作流不存在');
    const steps = jsonParse(w.steps, []);
    const runId = newId('run');
    const startedAt = now();
    this.db.run('INSERT INTO workflow_runs(id, workflow_id, status, trigger, started_at, steps, summary) VALUES (?,?,?,?,?,?,?)',
      [runId, id, 'running', trigger || 'manual', startedAt, '[]', '{}']);
    this.ctx.bus.info('workflow', '开始工作流：' + w.name, { workflowId: id, runId });
    const results = [];
    let failed = false;
    for (let i = 0; i < steps.length; i++) {
      const step = steps[i];
      const t0 = now();
      const rec = { index: i, type: step.type, label: step.label, status: 'running', startedAt: t0 };
      this.ctx.bus.info('workflow', '步骤 ' + (i + 1) + '/' + steps.length + '：' + step.label, { runId });
      try {
        rec.result = await this.execute(step);
        rec.status = 'completed';
      } catch (err) {
        rec.status = 'failed';
        rec.error = String(err && err.message || err);
        failed = true;
        this.ctx.bus.error('workflow', '步骤失败：' + step.label + ' — ' + rec.error, { runId });
      }
      rec.endedAt = now();
      rec.ms = rec.endedAt - t0;
      results.push(rec);
      this.db.run('UPDATE workflow_runs SET steps = ? WHERE id = ?', [JSON.stringify(results), runId]);
      if (rec.status === 'failed' && step.critical !== false) break;
    }
    const summary = this.summarize(results);
    this.db.run('UPDATE workflow_runs SET status=?, ended_at=?, summary=? WHERE id=?',
      [failed ? 'failed' : 'completed', now(), JSON.stringify(summary), runId]);
    this.ctx.bus.info('workflow', '工作流结束：' + w.name + '（' + (failed ? '有失败' : '成功') + '，' + (now() - startedAt) + 'ms）', { runId });
    return { runId, status: failed ? 'failed' : 'completed', steps: results, summary };
  }

  async execute(step) {
    switch (step.type) {
      case 'scan': return await this.svc.scanner.scanAll({});
      case 'diagnose': return this.svc.faults.diagnose();
      case 'dedupe': return this.svc.dedupe.scan();
      case 'reindex': this.svc.registry.reindexSearch(); return { ok: true };
      case 'updates': return await this.svc.updates.checkAll({ useNpm: true });
      case 'agents': return await this.svc.agents.scan();
      case 'notify': {
        const stats = this.svc.registry.stats();
        this.ctx.bus.info('workflow', '汇总：能力 ' + stats.total + ' 项，故障 ' + stats.faultsOpen + '，重复 ' + stats.duplicatesPending + '，可更新 ' + stats.updatesAvailable);
        return stats;
      }
      default: throw new Error('未知步骤类型：' + step.type);
    }
  }

  summarize(results) {
    return {
      total: results.length,
      completed: results.filter((r) => r.status === 'completed').length,
      failed: results.filter((r) => r.status === 'failed').length,
      ms: results.reduce((s, r) => s + (r.ms || 0), 0)
    };
  }

  runs(limit) {
    return this.db.all('SELECT r.*, w.name FROM workflow_runs r LEFT JOIN workflows w ON w.id = r.workflow_id ORDER BY r.started_at DESC LIMIT ?', [limit || 50])
      .map((r) => ({ id: r.id, workflowId: r.workflow_id, name: r.name, status: r.status, trigger: r.trigger,
        startedAt: r.started_at, endedAt: r.ended_at, steps: jsonParse(r.steps, []), summary: jsonParse(r.summary, {}) }));
  }
}
