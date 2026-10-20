//! 多智能体路由协调：分配、排队、冲突与引用计数。
//!
//! # 为什么只读接口也要协调
//!
//! 本轮对外接口**只读**，所以"两个智能体抢同一份可变资源"在本轮不会发生。
//! 但协调机制必须**现在**就立住，理由有二：
//!
//! 1. 接入是无序的。两个宿主可能同一秒注册，而谁先拿到某个能力的
//!    使用权必须由中枢说了算，不能由各自实现决定。
//! 2. 一旦将来放开写接口，租约语义就已经在库里、已经有测试覆盖，
//!    而不是和真正的写路径一起第一次被调试。
//!
//! # 策略：引用计数 + 有界排队 + 抢占式拒绝
//!
//! | 维度 | 选择 | 理由 |
//! |---|---|---|
//! | 并发上限 | 每个智能体可配 `max_concurrent`（默认 1） | 默认互斥是最保守的可用形态 |
//! | 排队 | 有界 FIFO，超 `MAX_QUEUE` 即拒 | 无限队列会把"慢"变成"挂死" |
//! | 争用同一能力 | **允许并发读**，按各自身份裁剪 | 只读语义下争用无副作用 |
//! | 冲突 | 租约持有期互斥，超时自动释放 | 超时释放防死锁；无超时必然死锁 |
//!
//! # 为什么超时必须存在
//!
//! 一个智能体拿到租约后崩溃是常态而非异常。没有超时，租约永远不会归还，
//! 第二个智能体会永远排队下去——而"永远排队"比"直接拒绝"更难排查。
//! `Lease` 因此带 `acquired_at`，`reap` 按 `LEASE_TTL_MS` 回收。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 单个能力上的最大排队长度。超出即拒绝，不无限增长。
pub const MAX_QUEUE: usize = 32;

/// 租约默认存活时长（毫秒）。超时未释放视为持有者失联，由 reap 回收。
pub const LEASE_TTL_MS: i64 = 60_000;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 路由错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterError {
    /// 能力不存在。
    UnknownCapability(String),
    /// 已达并发上限，且排队已满。
    QueueFull { capability_id: String, queued: usize },
    /// 存在互斥租约且未超时。
    Contended { capability_id: String, holder: String },
    /// 内部锁已中毒。
    Poisoned,
}

impl std::fmt::Display for RouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouterError::UnknownCapability(c) => write!(f, "unknown_capability: {c}"),
            RouterError::QueueFull { capability_id, queued } => {
                write!(f, "queue_full: {capability_id} 已排队 {queued} 个等待者")
            }
            RouterError::Contended { capability_id, holder } => {
                write!(f, "contended: {capability_id} 当前由 {holder} 持有")
            }
            RouterError::Poisoned => write!(f, "router_lock_poisoned"),
        }
    }
}

/// 租约模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseMode {
    /// 共享读：多个智能体可同时持有（只读接口的默认）。
    Shared,
    /// 互斥写：同一时刻只允许一个持有者。
    Exclusive,
}

/// 一张租约。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub capability_id: String,
    pub agent_id: String,
    pub mode: LeaseMode,
    pub acquired_at: i64,
    /// 排队时的序号，用于 FIFO 公平。
    pub ticket: u64,
}

/// 某能力的分配状态（只读快照）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allocation {
    pub capability_id: String,
    pub holders: Vec<String>,
    pub queued: Vec<String>,
    pub mode: LeaseMode,
}

/// 路由协调器。
///
/// 内部状态只有内存一份：**重启后租约归零**。这是刻意的——租约是
/// "现在谁在用"的瞬时事实，持久化一份过期租约比丢失它更危险。
/// 需要长期事实的，走 `agent_capability_usage` 审计表。
#[derive(Debug, Default)]
pub struct Router {
    state: Mutex<RouterState>,
}

#[derive(Debug, Default)]
struct RouterState {
    /// capability_id -> 当前持有者。
    holders: BTreeMap<String, Vec<Lease>>,
    /// capability_id -> 等待队列（agent_id 顺序即 FIFO）。
    queues: BTreeMap<String, Vec<String>>,
    /// 下一个排队号，保证 FIFO 公平且与到达顺序一致。
    next_ticket: u64,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    /// 申请一次使用许可。
    ///
    /// `mode=Shared` 时：只要该智能体没超出自身 `max_concurrent` 就立即授予。
    /// `mode=Exclusive` 时：若存在未超时的其它持有者则排队（队满即拒）。
    pub fn acquire(
        &self,
        capability_id: &str,
        agent_id: &str,
        mode: LeaseMode,
        max_concurrent: u32,
    ) -> Result<Lease, RouterError> {
        let mut st = self.state.lock().map_err(|_| RouterError::Poisoned)?;
        self.reap_locked(&mut st, now_ms());

        let held_by_self = st
            .holders
            .get(capability_id)
            .map(|v| v.iter().filter(|l| l.agent_id == agent_id).count())
            .unwrap_or(0) as u32;

        match mode {
            LeaseMode::Shared => {
                if held_by_self >= max_concurrent.max(1) {
                    return Self::enqueue(&mut st, capability_id, agent_id);
                }
            }
            LeaseMode::Exclusive => {
                let busy = st
                    .holders
                    .get(capability_id)
                    .map(|v| v.iter().any(|l| l.agent_id != agent_id))
                    .unwrap_or(false);
                if busy {
                    return Self::enqueue(&mut st, capability_id, agent_id);
                }
                if held_by_self >= max_concurrent.max(1) {
                    return Self::enqueue(&mut st, capability_id, agent_id);
                }
            }
        }

        let ticket = st.next_ticket;
        st.next_ticket += 1;
        let lease = Lease {
            capability_id: capability_id.to_string(),
            agent_id: agent_id.to_string(),
            mode,
            acquired_at: now_ms(),
            ticket,
        };
        st.holders
            .entry(capability_id.to_string())
            .or_default()
            .push(lease.clone());
        st.queues
            .get_mut(capability_id)
            .map(|q| q.retain(|a| a != agent_id));
        Ok(lease)
    }

    fn enqueue(
        st: &mut RouterState,
        capability_id: &str,
        agent_id: &str,
    ) -> Result<Lease, RouterError> {
        let q = st.queues.entry(capability_id.to_string()).or_default();
        if !q.iter().any(|a| a == agent_id) {
            if q.len() >= MAX_QUEUE {
                return Err(RouterError::QueueFull {
                    capability_id: capability_id.to_string(),
                    queued: q.len(),
                });
            }
            q.push(agent_id.to_string());
        }
        // 排队返回的 lease 是"等待中"的哨兵：mode 保持请求的语义，
        // 但调用方应据 Err/Ok 与队列快照判断是"已持有"还是"在排队"。
        let ticket = st.next_ticket;
        st.next_ticket += 1;
        Ok(Lease {
            capability_id: capability_id.to_string(),
            agent_id: agent_id.to_string(),
            mode: LeaseMode::Shared,
            acquired_at: 0, // 0 = 尚未获得
            ticket,
        })
    }

    /// 释放租约，并把等待队列的队首**提升**为持有者。
    ///
    /// # 为什么要提升而不是只删
    ///
    /// 只删不提升的话，队首要等到**自己再发一次请求**才拿得到许可——
    /// 而那次请求若因队列已满被拒，就永远拿不到。提升让"释放"这个事件
    /// 本身完成交接，这才是排队该有的语义（否则排队毫无用处）。
    ///
    /// 提升只在**腾出空位**时发生；Shared 模式下已有其他持有者时
    /// 不提升（共享读不需要排队）。
    pub fn release(&self, capability_id: &str, agent_id: &str) -> bool {
        let Ok(mut st) = self.state.lock() else {
            return false;
        };
        let now = now_ms();
        self.reap_locked(&mut st, now);
        // 先就地裁剪持有者，再统一删空键——分两步是为了避免
        // "持有 st.holders 的可变借用" 与 "再拿一次 st.holders" 同时存在。
        let released = match st.holders.get_mut(capability_id) {
            Some(v) => {
                let before = v.len();
                v.retain(|l| l.agent_id != agent_id);
                v.len() != before
            }
            None => {
                // 没有持有者，但可能仍在队列里（例如被提升后又超时）。
                return false;
            }
        };
        if released {
            if let Some(q) = st.queues.get_mut(capability_id) {
                q.retain(|a| a != agent_id);
            }
        }
        // 腾空后提升队首。
        if released && st.holders.get(capability_id).map(|v| v.is_empty()).unwrap_or(true) {
            let next = st
                .queues
                .get_mut(capability_id)
                .and_then(|q| if q.is_empty() { None } else { Some(q.remove(0)) });
            if let Some(agent) = next {
                let ticket = st.next_ticket;
                st.next_ticket += 1;
                st.holders
                    .entry(capability_id.to_string())
                    .or_default()
                    .push(Lease {
                        capability_id: capability_id.to_string(),
                        agent_id: agent,
                        mode: LeaseMode::Shared,
                        acquired_at: now,
                        ticket,
                    });
            }
            if st
                .queues
                .get(capability_id)
                .map(|q| q.is_empty())
                .unwrap_or(false)
            {
                st.queues.remove(capability_id);
            }
        }
        released
    }

    /// 回收超时租约，返回被回收的 (capability_id, agent_id)。
    pub fn reap(&self) -> Vec<(String, String)> {
        let Ok(mut st) = self.state.lock() else {
            return Vec::new();
        };
        self.reap_locked(&mut st, now_ms())
    }

    fn reap_locked(&self, st: &mut RouterState, now: i64) -> Vec<(String, String)> {
        let mut dropped = Vec::new();
        for (cap, leases) in st.holders.iter_mut() {
            leases.retain(|l| {
                let alive = now - l.acquired_at <= LEASE_TTL_MS;
                if !alive {
                    dropped.push((cap.clone(), l.agent_id.clone()));
                }
                alive
            });
            if leases.is_empty() {
                // 下面统一 remove，避免边遍历边删。
            }
        }
        st.holders.retain(|_, v| !v.is_empty());
        dropped
    }

    /// 某能力当前的分配快照。
    pub fn allocation(&self, capability_id: &str) -> Allocation {
        let st = match self.state.lock() {
            Ok(s) => s,
            Err(_) => {
                return Allocation {
                    capability_id: capability_id.to_string(),
                    holders: Vec::new(),
                    queued: Vec::new(),
                    mode: LeaseMode::Shared,
                }
            }
        };
        let holders: Vec<String> = st
            .holders
            .get(capability_id)
            .map(|v| v.iter().map(|l| l.agent_id.clone()).collect())
            .unwrap_or_default();
        let queued = st
            .queues
            .get(capability_id)
            .cloned()
            .unwrap_or_default();
        let mode = st
            .holders
            .get(capability_id)
            .and_then(|v| v.first().map(|l| l.mode))
            .unwrap_or(LeaseMode::Shared);
        Allocation {
            capability_id: capability_id.to_string(),
            holders,
            queued,
            mode,
        }
    }

    /// 当前所有有持有者或排队者的能力。
    pub fn active(&self) -> Vec<Allocation> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        let mut ids: Vec<String> = st
            .holders
            .keys()
            .chain(st.queues.keys())
            .cloned()
            .collect();
        ids.sort();
        ids.dedup();
        ids.into_iter()
            .map(|id| {
                let holders = st
                    .holders
                    .get(&id)
                    .map(|v| v.iter().map(|l| l.agent_id.clone()).collect())
                    .unwrap_or_default();
                let queued = st.queues.get(&id).cloned().unwrap_or_default();
                Allocation {
                    capability_id: id,
                    holders,
                    queued,
                    mode: LeaseMode::Shared,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_mode_allows_multiple_agents() {
        let r = Router::new();
        let a = r.acquire("cap::x", "agent-a", LeaseMode::Shared, 1).unwrap();
        let b = r.acquire("cap::x", "agent-b", LeaseMode::Shared, 1).unwrap();
        assert!(a.acquired_at > 0);
        assert!(b.acquired_at > 0);
        assert_eq!(r.allocation("cap::x").holders.len(), 2);
    }

    #[test]
    fn exclusive_mode_queues_second_agent() {
        let r = Router::new();
        let a = r.acquire("cap::x", "agent-a", LeaseMode::Exclusive, 1).unwrap();
        assert!(a.acquired_at > 0);
        let b = r.acquire("cap::x", "agent-b", LeaseMode::Exclusive, 1).unwrap();
        // b 应在排队（acquired_at == 0），不是直接拿到。
        assert_eq!(b.acquired_at, 0);
        let alloc = r.allocation("cap::x");
        assert_eq!(alloc.holders, vec!["agent-a".to_string()]);
        assert_eq!(alloc.queued, vec!["agent-b".to_string()]);
    }

    #[test]
    fn queue_is_bounded() {
        let r = Router::new();
        r.acquire("cap::x", "holder", LeaseMode::Exclusive, 1).unwrap();
        for i in 0..MAX_QUEUE {
            let l = r.acquire("cap::x", &format!("w{i}"), LeaseMode::Exclusive, 1);
            assert!(l.is_ok(), "第 {i} 个等待者应入队");
        }
        let overflow = r.acquire("cap::x", "too-many", LeaseMode::Exclusive, 1);
        assert!(matches!(overflow, Err(RouterError::QueueFull { .. })));
    }

    #[test]
    fn release_promotes_queue_head_instead_of_orphan_it() {
        let r = Router::new();
        r.acquire("cap::x", "agent-a", LeaseMode::Exclusive, 1).unwrap();
        // agent-b 抢不到独占，进队列。
        let b = r.acquire("cap::x", "agent-b", LeaseMode::Exclusive, 1).unwrap();
        assert_eq!(b.acquired_at, 0, "agent-b 应在排队");
        assert_eq!(r.allocation("cap::x").queued, vec!["agent-b".to_string()]);

        assert!(r.release("cap::x", "agent-a"));
        // 释放即交接：agent-b 被提升为持有者，而不是留在队列里等自己再请求一次。
        let alloc = r.allocation("cap::x");
        assert_eq!(alloc.holders, vec!["agent-b".to_string()]);
        assert!(alloc.queued.is_empty(), "队首被提升后队列应为空");
    }

    #[test]
    fn release_of_non_holder_is_false_and_harmless() {
        let r = Router::new();
        r.acquire("cap::x", "agent-a", LeaseMode::Shared, 1).unwrap();
        assert!(!r.release("cap::x", "not-a-holder"));
        // 释放不存在的持有者不应误伤真实持有者。
        assert_eq!(r.allocation("cap::x").holders, vec!["agent-a".to_string()]);
    }

    #[test]
    fn concurrent_acquire_does_not_panic_or_double_count() {
        use std::sync::Arc;
        let r = Arc::new(Router::new());
        let mut hs = Vec::new();
        for i in 0..8 {
            let rr = r.clone();
            hs.push(std::thread::spawn(move || {
                rr.acquire("cap::x", &format!("a{i}"), LeaseMode::Shared, 1)
                    .is_ok()
            }));
        }
        for h in hs {
            assert!(h.join().unwrap());
        }
        // 8 个不同 agent 各自并发上限 1，Shared 下都应拿到。
        assert_eq!(r.allocation("cap::x").holders.len(), 8);
    }

    #[test]
    fn reap_removes_expired_leases() {
        let r = Router::new();
        {
            let mut st = r.state.lock().unwrap();
            st.holders.insert(
                "cap::x".into(),
                vec![Lease {
                    capability_id: "cap::x".into(),
                    agent_id: "ghost".into(),
                    mode: LeaseMode::Exclusive,
                    acquired_at: 0, // 远古，已超时
                    ticket: 0,
                }],
            );
        }
        let dropped = r.reap();
        assert_eq!(dropped, vec![("cap::x".to_string(), "ghost".to_string())]);
        assert!(r.allocation("cap::x").holders.is_empty());
    }
}
