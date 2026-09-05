#!/usr/bin/env python3
"""Exercise actual fence/lease ordering with two concurrent client queues."""
from pathlib import Path
import subprocess,tempfile
source=(Path(__file__).resolve().parents[1]/'g17_drm.rs').read_text()
def method(marker):
 a=source.index(marker);e=source.index('{',a)+1;depth=1
 while depth:
  depth+=(source[e]=='{')-(source[e]=='}');e+=1
 return source[a:e]
wait=method('fn wait_input_fences(');ready=method('fn acquire_ready_lease(')
submit=source[source.index('impl queue::Queue for G17PQueue {'):]
assert submit.index('parser\n            .finish()') < submit.index('self.acquire_ready_lease(')
assert submit.index('self.acquire_ready_lease(') < submit.index('let output_fence:') < submit.index('match pending {\n            G17PPendingCommand::Render(translated) => {')
assert '.registry\n            .acquire_lease(' not in submit
preamble=r'''
#![allow(dead_code)]
use std::sync::{Arc,Mutex,Condvar};
use std::sync::atomic::{AtomicU32,Ordering};
use std::time::Duration;
use std::thread;
#[derive(Debug,PartialEq,Copy,Clone)]struct Error(i32);
impl Error{fn from_errno(v:i32)->Self{Self(v)}}
type Result<T=()> = core::result::Result<T,Error>;
mod kernel{pub mod task{pub const MAX_SCHEDULE_TIMEOUT:i64=i64::MAX;}}
struct FenceState { status:Mutex<i64>,changed:Condvar,entered:Arc<AtomicU32> }
#[derive(Clone)]struct Fence(Arc<FenceState>);
impl Fence {
 fn new(value:i64,entered:Arc<AtomicU32>)->Self{Self(Arc::new(FenceState{status:Mutex::new(value),changed:Condvar::new(),entered}))}
 fn raw(&self)->*const FenceState{Arc::as_ptr(&self.0)}
 fn signal(&self){*self.0.status.lock().unwrap()=1;self.0.changed.notify_all();}
}
mod file { pub(super)struct SyncItem {pub fence:Option<super::Fence>} }
mod bindings {
 pub(super) unsafe fn dma_fence_wait_timeout(raw:*const super::FenceState,_interruptible:bool,_timeout:i64)->i64 {
  let f=unsafe{&*raw};f.entered.fetch_add(1,super::Ordering::SeqCst);
  let status=f.status.lock().unwrap();
  let (status,timed)=f.changed.wait_timeout_while(status,super::Duration::from_secs(2),|s|*s==0).unwrap();
  if timed.timed_out()&&*status==0{-110}else{*status}
 }
}
#[derive(Default)]struct Registry{held:Mutex<bool>,changed:Condvar,acquisitions:AtomicU32}
struct G17PLeaseGuard{registry:Arc<Registry>}
impl Registry {
 fn acquire_lease(self:&Arc<Self>,_id:u64)->Result<G17PLeaseGuard>{
  let held=self.held.lock().unwrap();
  let(mut held,timed)=self.changed.wait_timeout_while(held,Duration::from_secs(1),|h|*h).unwrap();
  if timed.timed_out()&&*held{return Err(Error(-16))}
  *held=true;self.acquisitions.fetch_add(1,Ordering::SeqCst);
  Ok(G17PLeaseGuard{registry:self.clone()})
 }
}
impl Drop for G17PLeaseGuard{fn drop(&mut self){*self.registry.held.lock().unwrap()=false;self.registry.changed.notify_all();}}
struct Queue{registry:Arc<Registry>,id:u64}
impl Queue{fn stage_fail(_submission:u64,_stage:&str,error:Error)->Error{error}}
'''
tests=r'''
#[test]fn waiting_client_allows_dependency_signaller_to_execute(){
 let registry=Arc::new(Registry::default());let entered=Arc::new(AtomicU32::new(0));
 let fence=Fence::new(0,entered.clone());let events=Arc::new(Mutex::new(Vec::new()));
 let waiter={let registry=registry.clone();let fence=fence.clone();let events=events.clone();thread::spawn(move||{
  let a=Queue{registry,id:1};let syncs=[file::SyncItem{fence:Some(fence)}];
  let _lease=a.acquire_ready_lease(1,&syncs)?;events.lock().unwrap().push('A');Ok::<_,Error>(())
 })};
 for _ in 0..1000{if entered.load(Ordering::SeqCst)!=0{break}thread::sleep(Duration::from_millis(1));}
 assert_eq!(entered.load(Ordering::SeqCst),1,"waiter did not reach input fence");
 // The same physical queue lease must remain available to B while A waits.
 let b=Queue{registry:registry.clone(),id:2};
 let signaller=b.acquire_ready_lease(2,&[]);let result=signaller.as_ref().map(|_|()).map_err(|e|*e);
 if signaller.is_ok(){events.lock().unwrap().push('B');fence.signal();}
 drop(signaller);
 let waiter_result=waiter.join().unwrap();
 assert_eq!(result,Ok(()),"dependency signaller blocked behind waiting client's lease");
 assert_eq!(waiter_result,Ok(()));assert_eq!(*events.lock().unwrap(),['B','A']);
 assert_eq!(registry.acquisitions.load(Ordering::SeqCst),2);assert!(!*registry.held.lock().unwrap());
}
#[test]fn interrupted_dependency_never_acquires_hardware(){
 let registry=Arc::new(Registry::default());let entered=Arc::new(AtomicU32::new(0));
 let q=Queue{registry:registry.clone(),id:1};
 let syncs=[file::SyncItem{fence:Some(Fence::new(-512,entered))}];
 assert!(matches!(q.acquire_ready_lease(1,&syncs),Err(Error(-512))));
 assert_eq!(registry.acquisitions.load(Ordering::SeqCst),0);assert!(!*registry.held.lock().unwrap());
}
#[test]fn ready_clients_keep_execution_serialized(){
 let registry=Arc::new(Registry::default());let active=Arc::new(AtomicU32::new(0));
 let mut workers=Vec::new();for id in 1..=2{let registry=registry.clone();let active=active.clone();workers.push(thread::spawn(move||{
  let q=Queue{registry,id};let _lease=q.acquire_ready_lease(id,&[]).unwrap();
  assert_eq!(active.fetch_add(1,Ordering::SeqCst),0);
  thread::sleep(Duration::from_millis(10));assert_eq!(active.fetch_sub(1,Ordering::SeqCst),1);
 }));}for w in workers{w.join().unwrap();}
 assert_eq!(registry.acquisitions.load(Ordering::SeqCst),2);
}
'''
code=preamble+'impl Queue{\n'+wait+'\n'+ready+'\n}\n'+tests
with tempfile.TemporaryDirectory() as d:
 d=Path(d);s=d/'ready.rs';s.write_text(code);exe=d/'ready'
 subprocess.run(['rustc','--edition=2021','--test',str(s),'-o',str(exe)],check=True)
 subprocess.run([str(exe),'--nocapture'],check=True)
 # Reproduce the previous production ordering in the exact same harness.
 # It must fail within bounded mock waits instead of hanging the test process.
 old_ready='''fn acquire_ready_lease(&self, submission_id:u64, syncs:&[file::SyncItem])->Result<G17PLeaseGuard>{
 let lease=self.registry.acquire_lease(self.id)?;
 Self::wait_input_fences(syncs).map_err(|e|Self::stage_fail(submission_id,"wait-input-fences",e))?;
 Ok(lease)
 }'''
 old=d/'old.rs';old.write_text(preamble+'impl Queue{\n'+wait+'\n'+old_ready+'\n}\n'+tests)
 oldexe=d/'old';subprocess.run(['rustc','--edition=2021','--test',str(old),'-o',str(oldexe)],check=True)
 result=subprocess.run([str(oldexe),'waiting_client_allows_dependency_signaller_to_execute'],text=True,capture_output=True,timeout=5)
 assert result.returncode!=0 and 'dependency signaller blocked' in result.stdout,result.stdout+result.stderr
print('PASS actual ready-lease/fence methods: cross-client dependency progress, interruption and mutual exclusion; old lease-before-wait ordering reproduces bounded deadlock failure')
