import { newId, now, jsonParse, tokenize, jaccard, compareVersions, normalizeName, cosineCounter, countTokens } from '../core/util.js';

// 去重与版本裁决：
// 1) 身份键相同 → 同一逻辑能力（upsert 阶段已合并）；
// 2) 名称归一相同或描述高度相似 → 候选组；
// 3) 默认建议「保留最新版本」，同时把旧版本标记 shadowed 而非删除。
export class DedupeService {
  constructor(ctx) {
    this.ctx = ctx;
    this.db = ctx.db;
    this.bus = ctx.bus;
  }

  scan() {
    const db = this.db;
    const caps = db.all("SELECT * FROM capabilities WHERE status != 'archived'");
    const byName = new Map();
    for (const c of caps) {
      const n = normalizeName(c.canonical_name);
      if (!byName.has(n)) byName.set(n, []);
      byName.get(n).push(c);
    }
    const groups = [];
    const handled = new Set();

    for (const [name, list] of byName) {
      if (list.length > 1) {
        const sig = list.map((c) => c.id).sort().join('|');
        if (!handled.has(sig)) { handled.add(sig); groups.push(this.buildGroup(name, list, 'same-name')); }
      }
    }

    // 名称不同但描述相似（跨命名空间重复）
    for (let i = 0; i < caps.length; i++) {
      for (let j = i + 1; j < caps.length; j++) {
        const a = caps[i]; const b = caps[j];
        if (a.kind !== b.kind) continue;
        const na = normalizeName(a.canonical_name); const nb = normalizeName(b.canonical_name);
        if (na === nb) continue;
        const sim = this.similarity(a, b);
        if (sim >= 0.86) {
          const ids = [a.id, b.id].sort();
          const sig = ids.join('|');
          if (handled.has(sig)) continue;
          const groupName = na.length <= nb.length ? na : nb;
          const group = this.buildGroup(groupName, [a, b], 'similar-description', sim);
          if (!this.overlapsExisting(group.memberIds)) { handled.add(sig); groups.push(group); }
        }
      }
    }

    // 落库（保留已有决策）
    let created = 0; let updated = 0;
    for (const g of groups) {
      const existing = db.get("SELECT * FROM duplicate_groups WHERE member_ids = ? AND decision != 'resolved'", [JSON.stringify(g.memberIds)]);
      if (existing) {
        db.run('UPDATE duplicate_groups SET similarity=?, reason=?, recommended=?, updated_at=? WHERE id=?', [g.similarity, g.reason, g.recommended, now(), existing.id]);
        updated++;
      } else {
        const id = newId('dup');
        db.run('INSERT INTO duplicate_groups(id, kind, canonical_name, similarity, reason, recommended, decision, member_ids, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?)',
          [id, g.kind, g.canonicalName, g.similarity, g.reason, g.recommended, 'pending', JSON.stringify(g.memberIds), now(), now()]);
        created++;
      }
    }
    if (groups.length) this.bus.info('dedupe', '识别到 ' + groups.length + ' 组重复候选（新增 ' + created + '）');
    return { groups: groups.length, created, updated };
  }

  similarity(a, b) {
    const ta = tokenize(a.canonical_name + ' ' + a.summary + ' ' + jsonParse(a.tags, []).join(' '));
    const tb = tokenize(b.canonical_name + ' ' + b.summary + ' ' + jsonParse(b.tags, []).join(' '));
    const nameSim = jaccard(tokenize(normalizeName(a.canonical_name)), tokenize(normalizeName(b.canonical_name)));
    const descSim = jaccard(ta, tb);
    return 0.6 * descSim + 0.4 * nameSim;
  }

  overlapsExisting(ids) {
    const set = new Set(ids);
    const rows = this.db.all("SELECT member_ids FROM duplicate_groups WHERE decision != 'resolved'");
    for (const r of rows) {
      const members = jsonParse(r.member_ids, []);
      if (members.some((m) => set.has(m))) return true;
    }
    return false;
  }

  buildGroup(name, list, reason, sim) {
    const sorted = [...list].sort((a, b) => compareVersions(b.version || '0', a.version || '0'));
    const memberIds = sorted.map((c) => c.id).sort();
    // 推荐保留：版本最高，其次最近更新
    const best = [...list].sort((a, b) => {
      const v = compareVersions(b.version || '0', a.version || '0');
      if (v !== 0) return v;
      return b.updated_at - a.updated_at;
    })[0];
    const similarity = sim ?? this.similarity(list[0], list[1] || list[0]);
    return {
      kind: list[0].kind,
      canonicalName: name,
      similarity,
      reason,
      recommended: best.id,
      memberIds
    };
  }

  list(filter) {
    const decision = filter && filter.decision;
    const rows = this.db.all(
      'SELECT * FROM duplicate_groups ' + (decision && decision !== 'all' ? 'WHERE decision = ?' : '') + ' ORDER BY updated_at DESC LIMIT 300',
      decision && decision !== 'all' ? [decision] : []
    );
    return rows.map((r) => ({
      id: r.id, kind: r.kind, canonicalName: r.canonical_name, similarity: r.similarity, reason: r.reason,
      recommended: r.recommended, decision: r.decision, chosenId: r.chosen_id, createdAt: r.created_at, updatedAt: r.updated_at,
      members: jsonParse(r.member_ids, []).map((id) => {
        const c = this.db.get('SELECT * FROM capabilities WHERE id = ?', [id]);
        if (!c) return null;
        const inst = this.db.get('SELECT path, host FROM installations WHERE capability_id = ? LIMIT 1', [id]);
        return { id: c.id, name: c.canonical_name, displayName: c.display_name, version: c.version, author: c.author, status: c.status,
          updatedAt: c.updated_at, path: inst ? inst.path : '', host: inst ? inst.host : '', size: 0 };
      }).filter(Boolean)
    }));
  }

  decide(groupId, action, chosenId) {
    const g = this.db.get('SELECT * FROM duplicate_groups WHERE id = ?', [groupId]);
    if (!g) throw new Error('重复组不存在');
    const members = jsonParse(g.member_ids, []);
    const target = chosenId || g.recommended;
    const ts = now();
    if (action === 'keep_latest') {
      for (const id of members) {
        this.db.run('UPDATE capabilities SET status = ? WHERE id = ?', [id === target ? 'active' : 'archived', id]);
        this.db.run('UPDATE installations SET visibility = ? WHERE capability_id = ?', [id === target ? 'primary' : 'shadowed', id]);
      }
      this.db.run("UPDATE duplicate_groups SET decision='keep_latest', chosen_id=?, updated_at=? WHERE id=?", [target, ts, groupId]);
      this.bus.info('dedupe', '保留最新版本：' + this.db.get('SELECT display_name FROM capabilities WHERE id=?', [target]).display_name);
    } else if (action === 'coexist') {
      for (const id of members) this.db.run("UPDATE capabilities SET status='active' WHERE id=?", [id]);
      this.db.run("UPDATE duplicate_groups SET decision='coexist', chosen_id=?, updated_at=? WHERE id=?", [target, ts, groupId]);
      this.bus.info('dedupe', '允许共存：' + g.canonical_name);
    } else if (action === 'ignore') {
      this.db.run("UPDATE duplicate_groups SET decision='ignore', chosen_id=?, updated_at=? WHERE id=?", [target, ts, groupId]);
      this.bus.info('dedupe', '忽略重复候选：' + g.canonical_name);
    } else if (action === 'merge') {
      const merged = this.mergeMembers(members, target);
      this.db.run("UPDATE duplicate_groups SET decision='merge', chosen_id=?, updated_at=? WHERE id=?", [target, ts, groupId]);
      this.bus.info('dedupe', '合并重复能力到 ' + merged.displayName);
      return { merged };
    } else {
      throw new Error('未知裁决动作：' + action);
    }
    return { ok: true };
  }

  mergeMembers(members, target) {
    const targetCap = this.db.get('SELECT * FROM capabilities WHERE id = ?', [target]);
    const rest = members.filter((m) => m !== target);
    for (const id of rest) {
      const c = this.db.get('SELECT * FROM capabilities WHERE id = ?', [id]);
      if (!c) continue;
      const tags = new Set([...jsonParse(targetCap.tags, []), ...jsonParse(c.tags, [])]);
      const hosts = new Set([...jsonParse(targetCap.hosts, []), ...jsonParse(c.hosts, [])]);
      this.db.run('UPDATE capabilities SET tags=?, hosts=?, updated_at=? WHERE id=?', [JSON.stringify([...tags]), JSON.stringify([...hosts]), now(), target]);
      this.db.run('UPDATE installations SET capability_id=?, visibility=? WHERE capability_id=?', [target, 'shadowed', id]);
      this.db.run("UPDATE capabilities SET status='archived' WHERE id=?", [id]);
      this.db.run('UPDATE faults SET capability_id=? WHERE capability_id=?', [target, id]);
    }
    this.db.run('UPDATE installations SET visibility=? WHERE capability_id=?', ['primary', target]);
    this.db.run("UPDATE capabilities SET status='active' WHERE id=?", [target]);
    return { id: target, displayName: targetCap.display_name };
  }
}
