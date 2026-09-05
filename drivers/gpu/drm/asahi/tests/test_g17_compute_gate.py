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
guards='\n'.join(item(n) for n in ['pub(crate) struct G17PComputeLeaseGuard {','impl G17PComputeLeaseGuard {','impl Drop for G17PComputeLeaseGuard {','struct G17PLeaseGuard {','impl Drop for G17PLeaseGuard {'])
code=r'''
#![allow(dead_code)]
use std::sync::{Arc,atomic::{AtomicU32,Ordering}};use std::ops::{Deref,DerefMut};use std::time::Duration;
type Result<T=()>=std::result::Result<T,i32>;const EIO:i32=5;const ERESTARTSYS:i32=512;
struct Mutex<T>(std::sync::Mutex<T>);struct Guard<'a,T>(Option<std::sync::MutexGuard<'a,T>>);
impl<T>Mutex<T>{fn lock(&self)->Guard<'_,T>{Guard(Some(self.0.lock().unwrap()))}}
impl<T>Deref for Guard<'_,T>{type Target=T;fn deref(&self)->&T{self.0.as_ref().unwrap()}}
impl<T>DerefMut for Guard<'_,T>{fn deref_mut(&mut self)->&mut T{self.0.as_mut().unwrap()}}
struct CondVar(std::sync::Condvar);impl CondVar{fn wait_interruptible<T>(&self,g:&mut Guard<'_,T>)->bool{let(inner,t)=self.0.wait_timeout(g.0.take().unwrap(),Duration::from_secs(2)).unwrap();g.0=Some(inner);t.timed_out()}fn notify_all(&self){self.0.notify_all()}}
struct G17PQueueRegistry{lease:Mutex<Option<u64>>,lease_free:CondVar,compute_readers:AtomicU32,compute_faulted:AtomicU32}
impl G17PQueueRegistry{fn new()->Arc<Self>{Arc::new(Self{lease:Mutex(std::sync::Mutex::new(None)),lease_free:CondVar(std::sync::Condvar::new()),compute_readers:AtomicU32::new(0),compute_faulted:AtomicU32::new(0)})}
'''+methods+'\n}\n'+guards+r'''
#[test]fn two_retained_compute_owners_are_simultaneously_admitted(){
 let r=G17PQueueRegistry::new();let a=r.acquire_compute(false).unwrap();let b=r.acquire_compute(false).unwrap();assert_eq!(r.compute_readers.load(Ordering::SeqCst),2);assert!(r.lease.lock().is_none());drop(b);assert_eq!(r.compute_readers.load(Ordering::SeqCst),1);drop(a);assert_eq!(r.compute_readers.load(Ordering::SeqCst),0);
}
#[test]fn first_install_waits_for_retirement_and_excludes_other_publication(){
 let r=G17PQueueRegistry::new();let old=r.acquire_compute(false).unwrap();let(tx,rx)=std::sync::mpsc::channel();let(release,wait)=std::sync::mpsc::channel();let rr=r.clone();let worker=std::thread::spawn(move||{let install=rr.acquire_compute(true).unwrap();tx.send(()).unwrap();wait.recv_timeout(Duration::from_secs(1)).unwrap();drop(install);});
 assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());drop(old);rx.recv_timeout(Duration::from_secs(1)).unwrap();assert_eq!(*r.lease.lock(),Some(0));let(tx,rx)=std::sync::mpsc::channel();let rr=r.clone();let next=std::thread::spawn(move||{let _job=rr.acquire_compute(false).unwrap();tx.send(()).unwrap();});assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());release.send(()).unwrap();rx.recv_timeout(Duration::from_secs(1)).unwrap();worker.join().unwrap();next.join().unwrap();
}
#[test]fn quarantined_ownership_wakes_blocked_render_without_freeing_gpu_lease(){
 let r=G17PQueueRegistry::new();let q=r.acquire_compute(false).unwrap();let(tx,rx)=std::sync::mpsc::channel();let rr=r.clone();let w=std::thread::spawn(move||{tx.send(rr.acquire_lease(9).err()).unwrap();});assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());q.quarantine();assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(),Some(EIO));assert_eq!(r.compute_readers.load(Ordering::SeqCst),1);assert!(r.acquire_compute(false).is_err());w.join().unwrap();drop(q);assert_eq!(r.compute_readers.load(Ordering::SeqCst),0);
}
'''
with tempfile.TemporaryDirectory() as d:
 f=Path(d)/'test.rs';f.write_text(code);exe=Path(d)/'test';subprocess.run(['rustc','--edition=2021','--test',str(f),'-o',str(exe)],check=True);subprocess.run([str(exe),'--test-threads=1'],check=True,timeout=10)
print('PASS actual compute gate with host threads: retained parallel admission, quiescent first install, bounded fault wake with quarantined ownership')
