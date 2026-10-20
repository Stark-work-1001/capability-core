import { now } from '../core/util.js';

// 事件总线 + 持久化事件流：所有扫描、导入、修复、检索都记录，供界面「工作流」与审计使用。
export class EventBus {
  constructor(db, options) {
    this.db = db;
    this.options = options || {};
    this.subscribers = new Set();
    this.ring = [];
    this.maxRing = 500;
  }

  on(fn) {
    this.subscribers.add(fn);
    return () => this.subscribers.delete(fn);
  }

  emit(topic, message, data) {
    const evt = {
      ts: now(),
      level: (data && data.level) || 'info',
      topic,
      message: String(message || ''),
      capabilityId: (data && data.capabilityId) || null,
      agentId: (data && data.agentId) || null,
      data: data || {}
    };
    this.ring.push(evt);
    if (this.ring.length > this.maxRing) this.ring.shift();
    if (this.db) {
      try {
        this.db.run(
          'INSERT INTO events(ts, level, topic, message, capability_id, agent_id, data) VALUES (?,?,?,?,?,?,?)',
          [evt.ts, evt.level, evt.topic, evt.message, evt.capabilityId, evt.agentId, JSON.stringify(evt.data)]
        );
      } catch { /* 事件失败不应中断主流程 */ }
    }
    for (const fn of this.subscribers) {
      try { fn(evt); } catch { /* subscriber isolation */ }
    }
    return evt;
  }

  info(topic, message, data) { return this.emit(topic, message, { ...(data || {}), level: 'info' }); }
  warn(topic, message, data) { return this.emit(topic, message, { ...(data || {}), level: 'warn' }); }
  error(topic, message, data) { return this.emit(topic, message, { ...(data || {}), level: 'error' }); }

  recent(limit) {
    return this.db
      ? this.db.all('SELECT * FROM events ORDER BY id DESC LIMIT ?', [limit || 100])
      : this.ring.slice(-(limit || 100)).reverse();
  }
}
