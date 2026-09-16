#!/usr/bin/env python3
"""Logical render queues retain affinity to their physical render pairs.

Fixed render-pool storage belongs to the VM and pair; its selected descriptor
context belongs to the current logical queue. Avoiding unnecessary pair moves
reduces context retargets and cross-VM cached-graph handoffs. Cache reuse must
still find the VM-owned storage when a sibling queue has a different context.

This compiles the driver's real lease state machine and drives two clients
alternating frames through it, counting how often a slot changes owner. It also
compiles the historical "lowest free slot" policy in the same harness, so the
count is shown to discriminate rather than being asserted in a vacuum.
"""
from pathlib import Path
import subprocess, tempfile

s = (Path(__file__).resolve().parents[1] / 'g17_drm.rs').read_text()


def item(needle):
    a = s.index(needle)
    i = s.index('{', a) + 1
    depth = 1
    while depth:
        depth += (s[i] == '{') - (s[i] == '}')
        i += 1
    return s[a:i]


slots = s[s.index('const G17P_RENDER_SLOTS: usize ='):]
slots = slots[:slots.index('const _: () = assert!(G17P_ACTIVE_RENDER_SLOTS > 0);')]
# Exercise the production two-pair capacity rather than a test-only rewrite.
assert 'const G17P_ACTIVE_RENDER_SLOTS: usize = G17P_RENDER_SLOTS;' in slots
state = '\n'.join(item(n) for n in [
    'struct G17PQueueLeaseState {',
    'impl G17PQueueLeaseState {',
])
methods = '\n'.join(item(n) for n in [
    '    fn acquire_lease(',
    '    fn quarantine_render_slot(',
    '    fn rehabilitate_render_slot_after_processor_stop(',
])
ready_lease = item('    fn acquire_ready_lease(')
assert 'self.id' in ready_lease
assert 'self.vm.id()' not in ready_lease
guards = '\n'.join(item(n) for n in [
    'struct G17PLeaseGuard {',
    'impl G17PLeaseGuard {',
    'impl Drop for G17PLeaseGuard {',
])

# Releasing a frame lease must preserve the queue's retained-slot affinity.
queue_drop = item('impl Drop for G17PQueue {')
assert 'release_render_claim' not in queue_drop, \
    'destroying one logical queue unexpectedly clears registry affinity'

manager = (Path(__file__).resolve().parents[1] / 'g17_manager.rs').read_text()
handoff_start = manager.index('pub(crate) fn handoff_retained_render_vm(')
handoff_end = manager.index('    fn preserve_immediate_retired_render_graph(', handoff_start)
handoff = manager[handoff_start:handoff_end]
assert 'execution_context: Arc<mmu::T8140ComputeExecutionContext>' in handoff
assert 'u16::try_from(execution_context.context_id())' in handoff
assert 'retained_graph_matches_vm(graph, vm)' in handoff
matcher_start = manager.index('fn retained_graph_matches_vm(')
matcher_end = manager.index('    /// The retained graph and global aliases', matcher_start)
matcher = manager[matcher_start:matcher_end]
assert 'graph.bind.matches(vm)' in matcher
assert 'graph.storage.descriptor_context_id() ==' not in matcher
assert 'vm.render_pool_mappings_match(' in matcher

# The lease is what decides which retained graph a submission lands on, so the
# real methods are compiled; everything below them is host-side scaffolding.
preamble = r'''
#![allow(dead_code)]
#![allow(unused_variables)]
use std::sync::{Arc,atomic::{AtomicU32,Ordering}};
use std::ops::{Deref,DerefMut};
use std::time::Duration;
type Result<T=()>=std::result::Result<T,i32>;
const EIO:i32=5;const EBUSY:i32=16;const ERESTARTSYS:i32=512;
macro_rules! pr_warn{($($t:tt)*)=>{{}}}
mod g17_resources { pub struct G17PRenderQueuePair; impl G17PRenderQueuePair { pub const SLOT_COUNT:u8=2; } }
struct Mutex<T>(std::sync::Mutex<T>);
struct Guard<'a,T>(Option<std::sync::MutexGuard<'a,T>>);
impl<T>Mutex<T>{fn lock(&self)->Guard<'_,T>{Guard(Some(self.0.lock().unwrap()))}}
impl<T>Deref for Guard<'_,T>{type Target=T;fn deref(&self)->&T{self.0.as_ref().unwrap()}}
impl<T>DerefMut for Guard<'_,T>{fn deref_mut(&mut self)->&mut T{self.0.as_mut().unwrap()}}
struct CondVar(std::sync::Condvar);
impl CondVar{
 fn wait_interruptible<T>(&self,g:&mut Guard<'_,T>)->bool{
  let(inner,t)=self.0.wait_timeout(g.0.take().unwrap(),Duration::from_secs(2)).unwrap();
  g.0=Some(inner);t.timed_out()
 }
 fn notify_all(&self){self.0.notify_all()}
}
'''+slots+r'''
#[derive(Default)]
'''+state+r'''
struct G17PQueueRegistry{lease:Mutex<G17PQueueLeaseState>,lease_free:CondVar,compute_faulted:AtomicU32}
impl G17PQueueRegistry{
 fn new()->Arc<Self>{Arc::new(Self{
  lease:Mutex(std::sync::Mutex::new(G17PQueueLeaseState::default())),
  lease_free:CondVar(std::sync::Condvar::new()),
  compute_faulted:AtomicU32::new(0)})}
'''+methods+r'''
}
'''+guards+r'''

/// Which logical queue's retained render storage each slot holds. Zero means the
/// slot has never built one.
#[derive(Default)]
struct RetainedStorage{owner:[u64;G17P_RENDER_SLOTS],rebuilds:u32}
impl RetainedStorage{
 /// One submission. Returns true when the incoming VM had to take over another
 /// client's slot, which in the driver means the whole graph is destroyed and
 /// rebuilt in the incoming VM before this frame can be staged.
 fn submit(&mut self,slot:u8,queue:u64)->bool{
  let previous=self.owner[slot as usize];
  self.owner[slot as usize]=queue;
  if previous!=0&&previous!=queue{self.rebuilds+=1;return true}
  false
 }
}

/// Historical policy: take the lowest free slot regardless of who built the
/// graph on it. Kept here so the counts below are shown to discriminate.
fn acquire_lowest_free(state:&mut G17PQueueLeaseState,_owner:u64)->Option<u8>{
 let free=(!state.render_slots)&G17PQueueLeaseState::render_mask();
 if free==0{return None}
 let slot=free.trailing_zeros() as u8;
 state.render_slots|=1u8<<slot;
 Some(slot)
}
'''

tests = r'''
/// Two clients alternating frames, exactly the GNOME-plus-vkcube shape.
#[test]fn two_alternating_clients_keep_their_own_retained_storage(){
 let r=G17PQueueRegistry::new();
 let mut storage=RetainedStorage::default();
 let mut slots=Vec::new();
 for frame in 0..32u64{
  let queue=if frame%2==0{11}else{22};
  let lease=r.acquire_lease(queue).unwrap();
  storage.submit(lease.slot(),queue);
  slots.push((queue,lease.slot()));
  drop(lease);
 }
 // The first frame of each client claims a slot; after that nothing may move.
 assert_eq!(storage.rebuilds,0,"a client took over another client's retained graph");
 let a:Vec<u8>=slots.iter().filter(|(vm,_)|*vm==11).map(|(_,s)|*s).collect();
 let b:Vec<u8>=slots.iter().filter(|(vm,_)|*vm==22).map(|(_,s)|*s).collect();
 assert!(a.windows(2).all(|w|w[0]==w[1]),"client A moved between slots: {:?}",a);
 assert!(b.windows(2).all(|w|w[0]==w[1]),"client B moved between slots: {:?}",b);
 assert_ne!(a[0],b[0],"both clients were given the same slot");
}

/// The same traffic under the old policy rebuilds a graph on every alternation.
/// Without this the assertion above could pass for the wrong reason.
#[test]fn lowest_free_slot_policy_rebuilds_on_every_alternation(){
 let mut state=G17PQueueLeaseState::default();
 let mut storage=RetainedStorage::default();
 for frame in 0..32u64{
  let vm=if frame%2==0{11}else{22};
  let slot=acquire_lowest_free(&mut state,vm).unwrap();
  storage.submit(slot,vm);
  state.release_render_slot(slot);
 }
 assert!(storage.rebuilds>=30,"expected the old policy to thrash, saw {}",storage.rebuilds);
}

/// Overlapping submissions: while A holds its slot, B must be given the other
/// one rather than waiting or failing.
#[test]fn a_second_client_runs_concurrently_on_its_own_slot(){
 let r=G17PQueueRegistry::new();
 let a=r.acquire_lease(11).unwrap();
 let b=r.acquire_lease(22).unwrap();
 assert_ne!(a.slot(),b.slot());
 drop(b);
 // B comes back to the slot holding its own graph even though A's is free.
 let b=r.acquire_lease(22).unwrap();
 assert_ne!(a.slot(),b.slot());
 drop(b);drop(a);
}

/// Two queues in one VM own different descriptor contexts and therefore cannot
/// share one retained graph while both physical pairs are available.
#[test]fn one_vm_two_queues_do_not_share_a_graph(){
 let r=G17PQueueRegistry::new();
 let first=r.acquire_lease(11).unwrap();
 let second=r.acquire_lease(12).unwrap();
 assert_ne!(first.slot(),second.slot());
}

/// More clients than slots still has to make progress. A third client blocks
/// and is then served -- it is never refused, because an EBUSY here reaches
/// userspace as a lost device.
#[test]fn a_third_client_waits_rather_than_failing(){
 let r=G17PQueueRegistry::new();
 let a=r.acquire_lease(11).unwrap();
 let _b=r.acquire_lease(22).unwrap();
 let (tx,rx)=std::sync::mpsc::channel();
 let rr=r.clone();
 let w=std::thread::spawn(move||{tx.send(rr.acquire_lease(33).map(|l|l.slot())).unwrap();});
 assert!(rx.recv_timeout(Duration::from_millis(20)).is_err(),"third client was not held back");
 let freed=a.slot();
 drop(a);
 assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(),Ok(freed));
 w.join().unwrap();
}

/// An abandoned render leaves firmware possibly still running the group, so its
/// slot is retired. No client may be handed it again, and once every slot is
/// retired a submission fails instead of sleeping forever.
#[test]fn a_retired_slot_is_never_handed_out_again(){
 let r=G17PQueueRegistry::new();
 let first=r.acquire_lease(11).unwrap();
 let retired=first.slot();
 drop(first);
 r.quarantine_render_slot(retired);
 for vm in [11u64,22,33]{
  let lease=r.acquire_lease(vm).unwrap();
  assert_ne!(lease.slot(),retired);
 }
 let survivor=r.acquire_lease(11).unwrap().slot();
 r.quarantine_render_slot(survivor);
 assert_eq!(r.acquire_lease(11).map(|l|l.slot()),Err(EIO));
}

/// Releasing a frame must not erase its queue's retained ownership.
#[test]fn releasing_one_frame_does_not_forget_the_queue_s_storage(){
 let r=G17PQueueRegistry::new();
 let mut storage=RetainedStorage::default();
 let owned={let l=r.acquire_lease(11).unwrap();storage.submit(l.slot(),11);l.slot()};
 let other={let l=r.acquire_lease(22).unwrap();storage.submit(l.slot(),22);l.slot()};
 assert_ne!(owned,other);
 let next_frame=r.acquire_lease(11).unwrap();
 assert_eq!(next_frame.slot(),owned);
 assert!(!storage.submit(next_frame.slot(),11));
}

/// Retiring a slot must wake a client already waiting for it, rather than
/// leaving it asleep on a slot that will never come back.
#[test]fn retiring_the_last_slot_wakes_a_waiter(){
 let r=G17PQueueRegistry::new();
 let a=r.acquire_lease(11).unwrap();
 let b=r.acquire_lease(22).unwrap();
 r.quarantine_render_slot(a.slot());
 let (tx,rx)=std::sync::mpsc::channel();
 let rr=r.clone();
 let w=std::thread::spawn(move||{tx.send(rr.acquire_lease(33).map(|l|l.slot())).unwrap();});
 assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
 r.quarantine_render_slot(b.slot());
 assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(),Err(EIO));
 w.join().unwrap();
 drop(a);drop(b);
}

/// A proven processor stop discards the abandoned hardware graph. Clear its
/// quarantine while its old completion still holds the lease, then make the
/// slot available exactly when that lease drops.
#[test]fn processor_stop_rehabilitates_a_quarantined_slot(){
 let r=G17PQueueRegistry::new();
 let a=r.acquire_lease(11).unwrap();
 let b=r.acquire_lease(22).unwrap();
 let slot=a.slot();
 r.quarantine_render_slot(slot);
 r.rehabilitate_render_slot_after_processor_stop(slot);
 drop(a);
 let recovered=r.acquire_lease(11).unwrap();
 assert_eq!(recovered.slot(),slot);
 drop(recovered);drop(b);
}
'''

code = preamble + tests
with tempfile.TemporaryDirectory() as d:
    d = Path(d)
    f = d / 'affinity.rs'
    f.write_text(code)
    exe = d / 'affinity'
    subprocess.run(['rustc', '--edition=2021', '--test', str(f), '-o', str(exe)], check=True)
    subprocess.run([str(exe), '--test-threads=1'], check=True, timeout=60)
print('PASS dormant two-pair affinity: clients keep retained storage, a third '
      'waits instead of failing, and retired slots stay retired')
