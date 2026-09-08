#!/usr/bin/env python3
"""Exercise actual completion methods: error notification is not GPU retirement."""
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[1]


def item(text, marker):
    start = text.index(marker)
    end = text.index('{', start) + 1
    depth = 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]


drm = (root / 'g17_drm.rs').read_text()
job = (root / 'g17_job.rs').read_text()
live = (root / 'g17_live_boot.rs').read_text()
compute = (root / 'g17_async_compute.rs').read_text()
render_impl = item(drm, 'impl G17PRenderCompletion {')
compute_impl = item(job, 'impl G17PJobCompletion {')
timeout = item(live, '    pub(crate) fn timeout_deferred_render(')
assert 'if retired {\n                signal.complete(Err(EIO));\n            } else {\n                signal.fail_while_owned(error);' in timeout
for marker in ('    fn service_async_compute_completions(',
               '    pub(crate) fn fail_outstanding_jobs_for_vm(',
               '    pub(crate) fn quarantine_async_compute('):
    body = item(compute, marker)
    assert '.complete(Err(' not in body, marker
    assert '.fail_while_owned(error)' in body, marker

source = r'''
#![allow(dead_code)]
use std::sync::{Arc, Mutex as StdMutex};
use std::sync::atomic::{AtomicU32, AtomicUsize, AtomicBool, AtomicI32, Ordering};
#[derive(Clone,Copy,Debug)] struct Error(i32);
impl Error {fn to_errno(self)->i32 {self.0}}
const EIO:Error=Error(-5); const EINVAL:Error=Error(-22);
type Result<T=()> = std::result::Result<T,Error>;
struct Mutex<T>(StdMutex<T>);
impl<T> Mutex<T> {fn lock(&self)->std::sync::MutexGuard<'_,T>{self.0.lock().unwrap()}}
#[derive(Default)] struct Memory {active:AtomicUsize,closed:AtomicBool,unmapped:AtomicBool}
impl Memory {
 fn close(&self) { self.closed.store(true,Ordering::SeqCst); if self.active.load(Ordering::SeqCst)==0 {self.unmapped.store(true,Ordering::SeqCst);} }
}
struct VmGuard(Arc<Memory>);
impl VmGuard {fn new(m:&Arc<Memory>)->Self{m.active.fetch_add(1,Ordering::SeqCst);Self(m.clone())}}
impl Drop for VmGuard {fn drop(&mut self){if self.0.active.fetch_sub(1,Ordering::SeqCst)==1 && self.0.closed.load(Ordering::SeqCst){self.0.unmapped.store(true,Ordering::SeqCst);}}}
struct Lease(Arc<AtomicUsize>);
impl Drop for Lease {fn drop(&mut self){self.0.fetch_add(1,Ordering::SeqCst);}}
#[derive(Default)] struct Status(AtomicI32);
impl Status {fn record(&self,e:i32){let _=self.0.compare_exchange(0,e,Ordering::AcqRel,Ordering::Acquire);}}
struct Fence {count:AtomicUsize,error:AtomicI32,status:Arc<Status>,status_at_signal:AtomicI32,active_at_signal:AtomicUsize,memory:Arc<Memory>}
impl Fence {
 fn set_error(&self,e:Error){self.error.store(e.0,Ordering::SeqCst);}
 fn signal(&self){self.status_at_signal.store(self.status.0.load(Ordering::SeqCst),Ordering::SeqCst);self.active_at_signal.store(self.memory.active.load(Ordering::SeqCst),Ordering::SeqCst);self.count.fetch_add(1,Ordering::SeqCst);}
}
fn fence(status:&Arc<Status>,memory:&Arc<Memory>)->Fence {Fence{count:AtomicUsize::new(0),error:AtomicI32::new(0),status:status.clone(),status_at_signal:AtomicI32::new(0),active_at_signal:AtomicUsize::new(99),memory:memory.clone()}}
mod g17_completion {pub struct PairedRenderCompletion {pub complete:bool}}
struct Mapping;
impl Mapping {fn with_cpu_bytes<F:FnOnce(&mut [u8])->Result>(&self,f:F)->Result{f(&mut [0;16])}}
struct Destination {mapping:Mapping,offset:usize}
'''

for name, impl, extra in [
    ('Render', render_impl, 'lease:Mutex<Option<Lease>>,'),
    ('Compute', compute_impl, 'destinations:[Option<Destination>;2],'),
]:
    source += f'''struct {name} {{ fence:Fence,status:Arc<Status>,vm_job:Mutex<Option<VmGuard>>,signalled:AtomicU32,{extra}}}
impl {name} {{
'''
    if '--negative-control' in sys.argv:
        # The former runtime wiring used complete(Err) for unresolved jobs.
        source += 'fn fail_while_owned(&self,error:Error){self.complete(Err(error));}'
    else:
        source += item(impl, '    pub(crate) fn fail_while_owned(')
    source += item(impl, '    pub(crate) fn complete(')
    source += '\n}\n'

source += r'''
fn render(m:&Arc<Memory>,released:&Arc<AtomicUsize>)->Arc<Render>{
 let s=Arc::new(Status::default()); Arc::new(Render{fence:fence(&s,m),status:s,vm_job:Mutex(StdMutex::new(Some(VmGuard::new(m)))),signalled:AtomicU32::new(0),lease:Mutex(StdMutex::new(Some(Lease(released.clone()))))})
}
fn compute(m:&Arc<Memory>)->Arc<Compute>{let s=Arc::new(Status::default());Arc::new(Compute{fence:fence(&s,m),status:s,vm_job:Mutex(StdMutex::new(Some(VmGuard::new(m)))),signalled:AtomicU32::new(0),destinations:[None,None]})}
#[test] fn render_error_keeps_mapping_and_slot_through_file_and_scheduler_close(){
 let m=Arc::new(Memory::default());let released=Arc::new(AtomicUsize::new(0));
 let runtime=render(&m,&released);let scheduler=runtime.clone();
 m.close();runtime.fail_while_owned(EIO);drop(scheduler);
 assert!(!m.unmapped.load(Ordering::SeqCst));assert_eq!(released.load(Ordering::SeqCst),0);
 assert_eq!(runtime.fence.count.load(Ordering::SeqCst),1);
 assert_eq!(runtime.fence.status_at_signal.load(Ordering::SeqCst),-5);
 assert_eq!(runtime.fence.active_at_signal.load(Ordering::SeqCst),1);
 runtime.fail_while_owned(EIO);assert_eq!(runtime.fence.count.load(Ordering::SeqCst),1);
 // Runtime quarantine owner is dropped only after proven processor stop.
 drop(runtime);assert!(m.unmapped.load(Ordering::SeqCst));assert_eq!(released.load(Ordering::SeqCst),1);
}
#[test] fn compute_error_keeps_mapping_until_quarantine_owner_drops(){
 let m=Arc::new(Memory::default());let runtime=compute(&m);let scheduler=runtime.clone();
 runtime.fail_while_owned(EIO);m.close();drop(scheduler);
 assert!(!m.unmapped.load(Ordering::SeqCst));assert_eq!(runtime.fence.active_at_signal.load(Ordering::SeqCst),1);
 assert_eq!(runtime.fence.status_at_signal.load(Ordering::SeqCst),-5);
 runtime.fail_while_owned(EIO);assert_eq!(runtime.fence.count.load(Ordering::SeqCst),1);
 drop(runtime);assert!(m.unmapped.load(Ordering::SeqCst));
}
#[test] fn retired_render_releases_before_success_fence_despite_scheduler_arc(){
 let m=Arc::new(Memory::default());let released=Arc::new(AtomicUsize::new(0));let c=render(&m,&released);m.close();
 c.complete(Ok(g17_completion::PairedRenderCompletion{complete:true}));
 assert!(m.unmapped.load(Ordering::SeqCst));assert_eq!(released.load(Ordering::SeqCst),1);
 assert_eq!(c.fence.active_at_signal.load(Ordering::SeqCst),0);
 c.fail_while_owned(EIO);assert_eq!(c.fence.count.load(Ordering::SeqCst),1);assert_eq!(c.fence.error.load(Ordering::SeqCst),0);
}
#[test] fn proven_retired_error_releases_but_unresolved_error_does_not(){
 let m=Arc::new(Memory::default());let released=Arc::new(AtomicUsize::new(0));let c=render(&m,&released);m.close();
 c.complete(Err(EIO));assert!(m.unmapped.load(Ordering::SeqCst));assert_eq!(released.load(Ordering::SeqCst),1);
 assert_eq!(c.fence.active_at_signal.load(Ordering::SeqCst),0);assert_eq!(c.fence.status_at_signal.load(Ordering::SeqCst),-5);
}
#[test] fn retired_compute_releases_before_signal(){
 let m=Arc::new(Memory::default());let c=compute(&m);m.close();c.complete(Ok([1,2]));
 assert!(m.unmapped.load(Ordering::SeqCst));assert_eq!(c.fence.active_at_signal.load(Ordering::SeqCst),0);
}
#[test] fn competing_error_notifications_keep_ownership_and_signal_once(){
 let m=Arc::new(Memory::default());let released=Arc::new(AtomicUsize::new(0));let c=render(&m,&released);m.close();
 let mut threads=vec![];for _ in 0..8{let c=c.clone();threads.push(std::thread::spawn(move||c.fail_while_owned(EIO)));}
 for t in threads{t.join().unwrap();}assert_eq!(c.fence.count.load(Ordering::SeqCst),1);
 assert!(!m.unmapped.load(Ordering::SeqCst));assert_eq!(released.load(Ordering::SeqCst),0);
 drop(c);assert!(m.unmapped.load(Ordering::SeqCst));
}
'''
with tempfile.TemporaryDirectory() as tmp:
    tmp = Path(tmp)
    src, exe = tmp / 'ownership.rs', tmp / 'ownership'
    src.write_text(source)
    subprocess.run(['rustc', '--edition=2021', '--test', str(src), '-o', str(exe)], check=True)
    run = subprocess.run([str(exe), '--test-threads=1'])
    if '--negative-control' in sys.argv:
        assert run.returncode != 0, 'old complete(Err) wiring unexpectedly passed'
        print('PASS negative control: old unresolved-error wiring is rejected')
        sys.exit(0)
    run.check_returncode()
print('PASS actual completion methods: error signalling preserves unresolved VM/slot ownership')
