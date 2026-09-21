#!/usr/bin/env python3
"""Actual gate methods: parallel retained compute, quiescent install, fault wake."""
from pathlib import Path
import subprocess,tempfile
s=(Path(__file__).resolve().parents[1]/'g17_drm.rs').read_text()
def item(needle):
 a=s.index(needle);start=s.index('{',a);d=1;i=start+1
 while d:d+=(s[i]=='{')-(s[i]=='}');i+=1
 return s[a:i]
methods='\n'.join(item(n) for n in ['    fn acquire_compute(', '    fn acquire_lease('])
# The slot-count constant lives beside the state, not inside it, so extract
# it too rather than re-declaring a copy that could drift.
slots=s[s.index('const G17P_RENDER_SLOTS: usize ='):]
slots=slots[:slots.index('const _: () = assert!(G17P_ACTIVE_RENDER_SLOTS > 0);')]
state='\n'.join(item(n) for n in ['struct G17PQueueLeaseState {','impl Default for G17PQueueLeaseState {','impl G17PQueueLeaseState {'])
guards='\n'.join(item(n) for n in ['pub(crate) struct G17PComputeLeaseGuard {','impl G17PComputeLeaseGuard {','impl Drop for G17PComputeLeaseGuard {','struct G17PLeaseGuard {','impl G17PLeaseGuard {','impl Drop for G17PLeaseGuard {'])
code=r'''
#![allow(dead_code)]
use std::sync::{Arc,atomic::{AtomicU32,Ordering}};use std::ops::{Deref,DerefMut};use std::time::Duration;
type Result<T=()>=std::result::Result<T,i32>;const EINVAL:i32=22;const ENOSPC:i32=28;const EIO:i32=5;const EBUSY:i32=16;const ERESTARTSYS:i32=512;
#[derive(Default)]struct G17PComputeBurstState{arrived:u8,install_intervened:bool}
mod g17_resources { pub const G17P_RENDER_MAX_IN_FLIGHT:usize=2; pub struct G17PRenderQueuePair; impl G17PRenderQueuePair { pub const SLOT_COUNT:u8=64; } }
'''+slots+r'''
struct Mutex<T>(std::sync::Mutex<T>);struct Guard<'a,T>(Option<std::sync::MutexGuard<'a,T>>);
impl<T>Mutex<T>{fn lock(&self)->Guard<'_,T>{Guard(Some(self.0.lock().unwrap()))}}
impl<T>Deref for Guard<'_,T>{type Target=T;fn deref(&self)->&T{self.0.as_ref().unwrap()}}
impl<T>DerefMut for Guard<'_,T>{fn deref_mut(&mut self)->&mut T{self.0.as_mut().unwrap()}}
struct CondVar(std::sync::Condvar);impl CondVar{fn wait_interruptible<T>(&self,g:&mut Guard<'_,T>)->bool{let(inner,t)=self.0.wait_timeout(g.0.take().unwrap(),Duration::from_secs(2)).unwrap();g.0=Some(inner);t.timed_out()}fn notify_all(&self){self.0.notify_all()}}
'''+state+r'''
struct G17PQueueRegistry{lease:Mutex<G17PQueueLeaseState>,lease_free:CondVar,compute_readers:AtomicU32,compute_faulted:AtomicU32}
impl G17PQueueRegistry{fn new()->Arc<Self>{Arc::new(Self{lease:Mutex(std::sync::Mutex::new(G17PQueueLeaseState::default())),lease_free:CondVar(std::sync::Condvar::new()),compute_readers:AtomicU32::new(0),compute_faulted:AtomicU32::new(0)})}
'''+methods+'\n}\n'+guards+r'''
#[test]fn two_retained_compute_owners_are_simultaneously_admitted(){
 let r=G17PQueueRegistry::new();let a=r.acquire_compute(false).unwrap();let b=r.acquire_compute(false).unwrap();assert_eq!(r.compute_readers.load(Ordering::SeqCst),2);assert!(!r.lease.lock().compute_install);drop(b);assert_eq!(r.compute_readers.load(Ordering::SeqCst),1);drop(a);assert_eq!(r.compute_readers.load(Ordering::SeqCst),0);
}
#[test]fn first_install_waits_for_retirement_and_excludes_other_publication(){
 let r=G17PQueueRegistry::new();let old=r.acquire_compute(false).unwrap();let(tx,rx)=std::sync::mpsc::channel();let(release,wait)=std::sync::mpsc::channel();let rr=r.clone();let worker=std::thread::spawn(move||{let install=rr.acquire_compute(true).unwrap();tx.send(()).unwrap();wait.recv_timeout(Duration::from_secs(1)).unwrap();drop(install);});
 assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());drop(old);rx.recv_timeout(Duration::from_secs(1)).unwrap();assert!(r.lease.lock().compute_install);let(tx,rx)=std::sync::mpsc::channel();let rr=r.clone();let next=std::thread::spawn(move||{let _job=rr.acquire_compute(false).unwrap();tx.send(()).unwrap();});assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());release.send(()).unwrap();rx.recv_timeout(Duration::from_secs(1)).unwrap();worker.join().unwrap();next.join().unwrap();
}
#[test]fn steady_compute_overlaps_many_independently_owned_renders(){
 let r=G17PQueueRegistry::new();let q=r.acquire_compute(false).unwrap();
 let leases:Vec<_>=(1..=12).map(|owner|r.acquire_lease(owner).unwrap()).collect();
 assert_eq!(r.lease.lock().render_slots.count_ones(),12);
 assert_eq!(r.compute_readers.load(Ordering::SeqCst),1);
 for (index,lease) in leases.iter().enumerate(){assert_eq!(lease.slot() as usize,index+1);}
 drop(leases);drop(q);
}
#[test]fn quarantine_wakes_same_owner_waiter_without_releasing_live_slots(){
 let r=G17PQueueRegistry::new();let q=r.acquire_compute(false).unwrap();let _a0=r.acquire_lease(7).unwrap();let _a1=r.acquire_lease(7).unwrap();let _b=r.acquire_lease(8).unwrap();let(tx,rx)=std::sync::mpsc::channel();let rr=r.clone();let w=std::thread::spawn(move||{tx.send(rr.acquire_lease(7).err()).unwrap();});assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());q.quarantine();assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(),Some(EIO));assert_eq!(r.lease.lock().render_slots,14);w.join().unwrap();drop(q);
}
#[test]fn processor_stop_clears_compute_poison_before_the_old_lease_drops(){
 let r=G17PQueueRegistry::new();let q=r.acquire_compute(false).unwrap();q.quarantine();
 assert_eq!(r.acquire_compute(false).err(),Some(EIO));
 q.rehabilitate_after_processor_stop();drop(q);
 let recovered=r.acquire_compute(false).unwrap();drop(recovered);
 let render=r.acquire_lease(7).unwrap();drop(render);
}
'''
code += f'\n#[path = "{Path(__file__).resolve().parents[1] / "g17_queue_limits.rs"}"] mod g17_queue_limits;\n'
with tempfile.TemporaryDirectory() as d:
 f=Path(d)/'test.rs';f.write_text(code);exe=Path(d)/'test';subprocess.run(['rustc','--edition=2021','--test',str(f),'-o',str(exe)],check=True);subprocess.run([str(exe),'--test-threads=1'],check=True,timeout=10)
print('PASS bounded queue gate with host threads: retained compute overlap, independent render owners, per-owner backpressure and fault wake')
