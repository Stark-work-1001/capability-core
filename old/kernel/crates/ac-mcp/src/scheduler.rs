//! Bounded round-robin admission, per-agent quotas and RAII release.
use std::{collections::{BTreeMap,VecDeque},sync::{Arc,Mutex,Condvar},time::{Duration,Instant}};
use serde::Serialize;
#[derive(Default)] struct State {active:usize,by_agent:BTreeMap<String,usize>,waiting:VecDeque<(u64,String)>,last_agent:Option<String>,next:u64,admitted:u64,rejected:u64}
pub struct Scheduler {state:Mutex<State>,changed:Condvar,limit:usize,per_agent:usize,queue_limit:usize}
pub struct Permit {owner:Arc<Scheduler>,agent:String}
pub fn global()->Arc<Scheduler>{static S:std::sync::OnceLock<Arc<Scheduler>>=std::sync::OnceLock::new();S.get_or_init(||Arc::new(Scheduler::default())).clone()}
#[derive(Serialize)] pub struct SchedulerStats {pub active:usize,pub queued:usize,pub admitted:u64,pub rejected:u64,pub max_active:usize,pub per_agent:usize,pub max_queued:usize}
impl Default for Scheduler {fn default()->Self{Self::new(8,2,64)}}
impl Scheduler {
 pub fn new(limit:usize,per_agent:usize,queue_limit:usize)->Self {Self{state:Mutex::new(State::default()),changed:Condvar::new(),limit:limit.max(1),per_agent:per_agent.max(1),queue_limit}}
 pub fn stats(&self)->SchedulerStats {let s=self.state.lock().unwrap_or_else(|e|e.into_inner());SchedulerStats{active:s.active,queued:s.waiting.len(),admitted:s.admitted,rejected:s.rejected,max_active:self.limit,per_agent:self.per_agent,max_queued:self.queue_limit}}
 pub fn acquire(self:&Arc<Self>,agent:&str,timeout:Duration)->Result<Permit,String>{
  let agent=if agent.is_empty(){"anonymous"}else{agent};
  let mut s=self.state.lock().map_err(|_|"调度锁不可用")?;
  if s.waiting.len()>=self.queue_limit || s.waiting.iter().filter(|(_,a)|a==agent).count() >= 8 {s.rejected+=1;return Err("请求队列已满，请稍后重试".into());}
  s.next+=1;let ticket=s.next;s.waiting.push_back((ticket,agent.into()));let until=Instant::now()+timeout;
  loop {
   let eligible=s.waiting.iter().position(|(_,a)|s.by_agent.get(a).copied().unwrap_or(0)<self.per_agent && s.last_agent.as_ref()!=Some(a))
    .or_else(||s.waiting.iter().position(|(_,a)|s.by_agent.get(a).copied().unwrap_or(0)<self.per_agent));
   if s.active<self.limit && eligible.is_some_and(|i|s.waiting[i].0==ticket){
    s.waiting.remove(eligible.unwrap());s.last_agent=Some(agent.into());s.active+=1;*s.by_agent.entry(agent.into()).or_default()+=1;s.admitted+=1;
    self.changed.notify_all();return Ok(Permit{owner:self.clone(),agent:agent.into()});
   }
   let remaining=until.saturating_duration_since(Instant::now());
   if remaining.is_zero(){s.waiting.retain(|(id,_)|*id!=ticket);s.rejected+=1;self.changed.notify_all();return Err("请求排队超时，请稍后重试".into());}
   s=self.changed.wait_timeout(s,remaining).map_err(|_|"调度锁不可用")?.0;
  }
 }
}
impl Drop for Permit {fn drop(&mut self){let mut s=self.owner.state.lock().unwrap_or_else(|e|e.into_inner());s.active=s.active.saturating_sub(1);if let Some(n)=s.by_agent.get_mut(&self.agent){*n=n.saturating_sub(1);if *n==0{s.by_agent.remove(&self.agent);}}self.owner.changed.notify_all();}}
#[cfg(test)] mod tests {use super::*;
 #[test]fn quota_does_not_block_other_agents(){let q=Arc::new(Scheduler::new(2,1,4));let _a=q.acquire("a",Duration::from_millis(1)).unwrap();assert!(q.acquire("a",Duration::from_millis(1)).is_err());let b=q.acquire("b",Duration::from_millis(1)).unwrap();assert_eq!(q.stats().active,2);drop(b);assert_eq!(q.stats().active,1);assert_eq!(q.stats().queued,0);}
 #[test]fn releases_on_unwind(){let q=Arc::new(Scheduler::default());let q2=q.clone();let _=std::panic::catch_unwind(move||{let _p=q2.acquire("a",Duration::from_millis(1)).unwrap();panic!("test")});assert_eq!(q.stats().active,0);}
}
