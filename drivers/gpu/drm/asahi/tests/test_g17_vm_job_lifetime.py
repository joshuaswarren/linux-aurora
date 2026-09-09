#!/usr/bin/env python3
"""Exercise production G17P VM job admission and deferred-unmap ownership."""
from pathlib import Path
import subprocess
import tempfile

s = (Path(__file__).resolve().parents[1] / "mmu.rs").read_text()


def item(needle):
    a = s.index(needle)
    start = s.index("{", a)
    depth = 1
    i = start + 1
    while depth:
        depth += (s[i] == "{") - (s[i] == "}")
        i += 1
    return s[a:i]


state = item("struct T8140VmJobLifetime {")
gate_state = item("struct T8140VmMappingGateState {")
gate_type = item("struct T8140VmMappingGate {").replace("    #[pin]\n", "")
guard = "\n".join([
    item("pub(crate) struct T8140VmJobGuard {"),
    item("impl T8140VmJobGuard {"),
    item("impl Drop for T8140VmJobGuard {"),
])
quarantine = item("    pub(crate) fn quarantine(")
retain = item("    pub(crate) fn retain_t8140_job(")
unmap_users = item("    pub(crate) fn unmap_user_ranges(")
unmap_wrapper = item("    pub(crate) fn unmap_user_range(")
unmap_one = item("    pub(crate) fn unmap_prepared_user_range(")
defer_batch = item("    fn defer_blocked_user_bind_batch(")
commit_batch = item("    pub(crate) fn commit_prepared_user_bind_batch(")
enter_commit = item("    fn enter_user_mapping_commit(")
drop_object = item("    pub(crate) fn drop_mappings(")

assert "self.vm.unmap_user_ranges_now" in guard
assert "self.vm.drop_mappings_now" in guard
assert "self.unmap_user_ranges_now" in unmap_users
assert "self.drop_mappings_now" in drop_object
assert "self.prepare_user_unmap" in unmap_wrapper
assert "self.unmap_prepared_user_range" in unmap_wrapper
assert "PreparedUserBindBatch::new(1)" in unmap_one
assert "commit_prepared_user_bind_batch" in unmap_one
assert "deferred_unmaps" in defer_batch
assert ".push((unmap.iova, unmap.size), GFP_KERNEL)" in defer_batch
assert "mapping_gate.state.lock()" in retain
assert quarantine.index("gate.blocked = true") < quarantine.index("while gate.commits != 0")
assert quarantine.index("while gate.commits != 0") < quarantine.index(
    "job_lifetime.lock().quarantined = true"
)
assert commit_batch.count("gate.blocked") == 1
assert "exec_lock_array" not in commit_batch
assert "gate.blocked" in enter_commit
assert commit_batch.index("exec_lock(Some(&map.gem), true)") < commit_batch.index(
    "enter_user_mapping_commit"
)
assert commit_batch.index("core::mem::drop(inner)") < commit_batch.index(
    "core::mem::drop(commit)"
)

source = r'''
#![allow(dead_code, unused_variables)]
use std::ops::{Deref, DerefMut, Range};
use std::sync::{Arc, Condvar as StdCondvar, atomic::{AtomicBool, AtomicUsize, Ordering}};
type Error=i32;
type Result<T=()>=std::result::Result<T,Error>;
const ENOENT:i32=2;
const EBUSY:i32=16;
const EIO:i32=5;
const EOVERFLOW:i32=75;
const GFP_KERNEL:u32=0;
macro_rules! pr_err{($($t:tt)*)=>{()}}
struct KVec<T>(Vec<T>);
impl<T>KVec<T>{fn new()->Self{Self(vec![])} fn push(&mut self,v:T,_f:u32)->Result{self.0.push(v);Ok(())}}
impl<T>Deref for KVec<T>{type Target=Vec<T>;fn deref(&self)->&Vec<T>{&self.0}}
impl<T>IntoIterator for KVec<T>{type Item=T;type IntoIter=std::vec::IntoIter<T>;fn into_iter(self)->Self::IntoIter{self.0.into_iter()}}
struct Mutex<T>(std::sync::Mutex<T>);
struct Guard<'a,T>(Option<std::sync::MutexGuard<'a,T>>);
impl<T>Deref for Guard<'_,T>{type Target=T;fn deref(&self)->&T{self.0.as_deref().unwrap()}}
impl<T>DerefMut for Guard<'_,T>{fn deref_mut(&mut self)->&mut T{self.0.as_deref_mut().unwrap()}}
impl<T>Mutex<T>{fn lock(&self)->Guard<'_,T>{Guard(Some(self.0.lock().unwrap()))}}
struct CondVar(StdCondvar);
impl CondVar {
    fn wait<T>(&self,guard:&mut Guard<'_,T>) {
        let held=guard.0.take().unwrap();
        guard.0=Some(self.0.wait(held).unwrap());
    }
    fn notify_all(&self) { self.0.notify_all(); }
}
mod gem{#[derive(Clone)]pub struct Object(pub u64);}
struct PreparedUserUnmap{iova:u64,size:u64}
struct ARef<T>(Arc<T>);
impl<T>Deref for ARef<T>{type Target=T;fn deref(&self)->&T{&self.0}}
impl From<&gem::Object> for ARef<gem::Object>{fn from(v:&gem::Object)->Self{Self(Arc::new(v.clone()))}}
#[derive(Clone)]
struct Vm {
    job_lifetime:Arc<Mutex<T8140VmJobLifetime>>,
    mapping_gate:Arc<T8140VmMappingGate>,
    unmaps:Arc<AtomicUsize>,
    object_unmaps:Arc<AtomicUsize>,
    block_cleanup:Arc<AtomicBool>,
    cleanup_gate:Arc<(std::sync::Mutex<(bool,bool)>,StdCondvar)>,
}
''' + state + "\n" + gate_state + "\n" + gate_type + "\n" + guard + "\nimpl Vm {\n" + retain + "\n" + unmap_users + "\n" + drop_object + r'''
    fn unmap_user_ranges_now(&self,_user:Range<u64>,_kernel:Range<u64>)->Result {
        self.unmaps.fetch_add(1,Ordering::SeqCst);
        self.maybe_block_cleanup();
        Ok(())
    }
    fn unmap_range(&self,_iova:u64,_size:u64)->Result {
        self.unmaps.fetch_add(1,Ordering::SeqCst);
        Ok(())
    }
    fn unmap_prepared_range(&self,_prepared:PreparedUserUnmap)->Result {
        self.unmaps.fetch_add(1,Ordering::SeqCst);
        Ok(())
    }
    fn unmap_user_range(&self,iova:u64,size:u64)->Result {
        let gate=self.mapping_gate.state.lock();
        if gate.blocked {
            let mut state=self.job_lifetime.lock();
            if state.closed { return Err(ENOENT); }
            if !state.quarantined { return Err(EBUSY); }
            if !state.deferred_unmaps.iter().any(|r|*r==(iova,size)) {
                state.deferred_unmaps.push((iova,size),GFP_KERNEL)?;
            }
            return Ok(());
        }
        drop(gate);
        self.unmap_prepared_range(PreparedUserUnmap{iova,size})
    }
    fn drop_mappings_now(&self,_gem:&gem::Object)->Result {
        self.object_unmaps.fetch_add(1,Ordering::SeqCst);
        self.maybe_block_cleanup();
        Ok(())
    }
    fn maybe_block_cleanup(&self) {
        if !self.block_cleanup.load(Ordering::SeqCst) { return; }
        let (lock,cv)=&*self.cleanup_gate;
        let mut state=lock.lock().unwrap();
        state.0=true;
        cv.notify_all();
        while !state.1 { state=cv.wait(state).unwrap(); }
    }
    fn wait_cleanup_entered(&self) {
        let (lock,cv)=&*self.cleanup_gate;
        let mut state=lock.lock().unwrap();
        while !state.0 { state=cv.wait(state).unwrap(); }
    }
    fn release_cleanup(&self) {
        let (lock,cv)=&*self.cleanup_gate;
        let mut state=lock.lock().unwrap();
        state.1=true;
        cv.notify_all();
    }
    fn bo_deferred_cleanup(&self){}
    fn new()->Self { Self {
        job_lifetime:Arc::new(Mutex(std::sync::Mutex::new(T8140VmJobLifetime{
            active:0,quarantined:false,draining:false,closed:false,close_ranges:None,
            deferred_unmaps:KVec::new(),closed_objects:KVec::new()
        }))),
        mapping_gate:Arc::new(T8140VmMappingGate{
            state:Mutex(std::sync::Mutex::new(T8140VmMappingGateState{
                blocked:false,commits:0,
            })),
            idle:CondVar(StdCondvar::new()),
        }),
        unmaps:Arc::new(AtomicUsize::new(0)),
        object_unmaps:Arc::new(AtomicUsize::new(0)),
        block_cleanup:Arc::new(AtomicBool::new(false)),
        cleanup_gate:Arc::new((std::sync::Mutex::new((false,false)),StdCondvar::new())),
    }}
}

#[test]
fn file_and_object_close_preserve_two_outstanding_jobs() {
    let v=Vm::new();
    let a=v.retain_t8140_job().unwrap();
    let b=v.retain_t8140_job().unwrap();
    v.drop_mappings(&gem::Object(7)).unwrap();
    v.unmap_user_ranges(0..100,80..90).unwrap();
    assert!(v.retain_t8140_job().is_err());
    assert_eq!(v.unmaps.load(Ordering::SeqCst),0);
    assert_eq!(v.object_unmaps.load(Ordering::SeqCst),0);
    drop(b);
    assert_eq!(v.unmaps.load(Ordering::SeqCst),0);
    drop(a);
    assert_eq!(v.unmaps.load(Ordering::SeqCst),1);
    assert_eq!(v.object_unmaps.load(Ordering::SeqCst),1);
}

#[test]
fn deferred_cleanup_excludes_new_job_admission_until_unmap_finishes() {
    use std::sync::mpsc;
    use std::time::Duration;
    let v=Vm::new();
    let job=v.retain_t8140_job().unwrap();
    v.drop_mappings(&gem::Object(8)).unwrap();
    v.block_cleanup.store(true,Ordering::SeqCst);
    let retire=std::thread::spawn(move || drop(job));
    v.wait_cleanup_entered();
    let contender=v.clone();
    let (tx,rx)=mpsc::channel();
    let submit=std::thread::spawn(move || tx.send(contender.retain_t8140_job().is_ok()).unwrap());
    assert!(rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "new job entered while deferred unmap was still running");
    v.release_cleanup();
    retire.join().unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(),true);
    submit.join().unwrap();
}

#[test]
fn synchronized_explicit_unbind_is_independent_of_unrelated_live_jobs() {
    let v=Vm::new();
    let job=v.retain_t8140_job().unwrap();
    assert_eq!(v.unmap_user_range(0x1000,0x4000),Ok(()));
    assert_eq!(v.unmaps.load(Ordering::SeqCst),1);
    drop(job);
    assert_eq!(v.unmap_user_range(0x1000,0x4000),Ok(()));
    assert_eq!(v.unmaps.load(Ordering::SeqCst),2);
}

#[test]
fn quarantined_job_defers_explicit_unbind_until_all_vm_jobs_retire() {
    let v=Vm::new();
    let lost=v.retain_t8140_job().unwrap();
    let other=v.retain_t8140_job().unwrap();
    lost.quarantine();
    assert_eq!(v.unmap_user_range(0x5000,0x4000),Ok(()));
    assert_eq!(v.unmap_user_range(0x5000,0x4000),Ok(()));
    assert_eq!(v.unmaps.load(Ordering::SeqCst),0);
    drop(other);
    assert_eq!(v.unmaps.load(Ordering::SeqCst),0);
    drop(lost);
    assert_eq!(v.unmaps.load(Ordering::SeqCst),1,
               "duplicate teardown requests coalesce at processor-stop retirement");
}

#[test]
fn whole_vm_close_supersedes_queued_explicit_unmaps() {
    let v=Vm::new();
    let lost=v.retain_t8140_job().unwrap();
    lost.quarantine();
    assert_eq!(v.unmap_user_range(0x5000,0x4000),Ok(()));
    v.unmap_user_ranges(0..0x10000,0x8000..0x9000).unwrap();
    drop(lost);
    assert_eq!(v.unmaps.load(Ordering::SeqCst),1,
               "whole-VM teardown must not replay an overlapping range unmap");
}

#[test]
fn imported_object_close_is_owned_independently_by_each_vm() {
    let object=gem::Object(11);
    let a=Vm::new();
    let b=Vm::new();
    let ajob=a.retain_t8140_job().unwrap();
    let bjob=b.retain_t8140_job().unwrap();
    a.drop_mappings(&object).unwrap();
    b.drop_mappings(&object).unwrap();
    drop(ajob);
    assert_eq!(a.object_unmaps.load(Ordering::SeqCst),1);
    assert_eq!(b.object_unmaps.load(Ordering::SeqCst),0);
    drop(bjob);
    assert_eq!(b.object_unmaps.load(Ordering::SeqCst),1);
}

#[test]
fn overflow_and_idle_file_close_do_not_create_unowned_job() {
    let v=Vm::new();
    v.job_lifetime.lock().active=usize::MAX;
    assert!(v.retain_t8140_job().is_err());
    v.job_lifetime.lock().active=0;
    v.unmap_user_ranges(0..100,80..90).unwrap();
    assert_eq!(v.unmaps.load(Ordering::SeqCst),1);
    assert!(v.retain_t8140_job().is_err());
}
'''

with tempfile.TemporaryDirectory() as d:
    f = Path(d) / "test.rs"
    f.write_text(source)
    exe = Path(d) / "test"
    subprocess.run(["rustc", "--edition=2021", "--test", str(f), "-o", str(exe)], check=True)
    subprocess.run([str(exe), "--test-threads=1"], check=True)

print("PASS production VM job ownership: healthy unbinds stay live; quarantined unbinds defer")
