#!/usr/bin/env python3
"""Compile production async prepare/publish/release against fallible owner mocks."""
from pathlib import Path
import subprocess
import sys
import tempfile
from g17_config_test_support import with_g17_config

root = Path(__file__).resolve().parents[1]
source = (root / "g17_async_compute.rs").read_text()
limits = (root / "g17_queue_limits.rs").read_text()


def item(text, marker):
    start = text.index(marker)
    end = text.index("{", start) + 1
    depth = 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


types = "\n".join(item(source, "struct " + name + " {") for name in [
    "G17PAsyncComputeActive", "G17PAsyncComputeRetirementTicket",
    "G17PAsyncComputePreviousBinding", "G17PAsyncComputeQueue"])
methods = "\n".join(item(source, "    pub(crate) fn " + name + "(") for name in [
    "prepare_async_compute_queue", "publish_async_compute", "release_async_compute_queue"])
if "--negative-control" in sys.argv:
    methods = methods.replace(
        "let binding = manager.bind_independent_compute_user_vm(",
        "drop(q.binding.take()); drop(q.context.take());\n"
        "        let binding = manager.bind_independent_compute_user_vm(", 1)
predicates = "\n".join(item(limits, "pub(crate) const fn " + name + "(") for name in [
    "reusable_idle_compute_graph", "rebindable_released_compute_graph"])

rust = r'''
#![allow(dead_code, unused_variables)]
use std::{pin::Pin,sync::{Arc,atomic::{AtomicU32,AtomicUsize,Ordering,fence}}};
type Error=i32;type Result<T=()>=std::result::Result<T,Error>;
const EIO:i32=5;const ENODEV:i32=19;const EINVAL:i32=22;const EFAULT:i32=14;const EBUSY:i32=16;const EOVERFLOW:i32=75;const ENOSPC:i32=28;const GFP_KERNEL:u32=0;
macro_rules! dev_info {($($t:tt)*)=>{()}}
mod util {pub fn g17p_verbose()->bool{false}}
mod g17_status {pub struct VmStatus;}
mod g17_queue_limits {''' + predicates + r'''}
struct KVec<T>(Vec<T>);
impl<T> std::ops::Deref for KVec<T>{type Target=Vec<T>;fn deref(&self)->&Vec<T>{&self.0}}
impl<T> std::ops::DerefMut for KVec<T>{fn deref_mut(&mut self)->&mut Vec<T>{&mut self.0}}
impl<T> KVec<T>{fn reserve(&mut self,n:usize,_flags:u32)->Result{self.0.reserve(n);Ok(())}fn push(&mut self,v:T,_flags:u32)->Result{self.0.push(v);Ok(())}}
mod mmu {
 use super::*;
 #[derive(Clone)]pub struct Vm{pub id:u64}
 pub struct T8140ComputeExecutionContext{pub vm:Vm,pub id:u32,pub current:bool,pub drops:Arc<AtomicUsize>,pub channel_control:()}
 impl T8140ComputeExecutionContext{pub fn matches(&self,vm:&Vm)->bool{self.vm.id==vm.id}pub fn is_current(&self)->bool{self.current}pub fn context_id(&self)->u32{self.id}pub fn root(&self)->u64{self.vm.id*0x4000}}
 impl Drop for T8140ComputeExecutionContext{fn drop(&mut self){self.drops.fetch_add(1,Ordering::SeqCst);}}
}
mod g17_drm {pub struct G17PComputeLeaseGuard;}
mod g17_resources {pub use crate::mmu::T8140ComputeExecutionContext as G17PLogicalQueueContext;}
mod g17_uapi {pub struct ComputeTimestampAddresses{pub start:u64,pub end:u64}}
mod g17_rtkit {pub const ENDPOINT_GFX_INTERRUPTS:u32=0;}
mod g17_job {
 use super::*;
 pub struct Completion;impl Completion{pub fn gpu_address(&self)->u64{0x8000}}
 pub struct G17PComputeJobPacket{pub id:u64,pub qid:u8,pub ctx:Arc<mmu::T8140ComputeExecutionContext>,pub c:Completion}
 impl G17PComputeJobPacket{pub fn submission_id(&self)->u64{self.id}pub fn hardware_queue_id(&self)->u8{self.qid}pub fn context(&self)->&Arc<mmu::T8140ComputeExecutionContext>{&self.ctx}pub fn completion(&self)->Result<&Completion>{Ok(&self.c)}pub fn command(&self)->&(){&()}}
}
struct State{submission_phase:AtomicU32}
struct Rtkit{fail:bool}impl Rtkit{fn send_message(self:Pin<&mut Self>,_ep:u32,_msg:u64)->Result{if self.fail{Err(EIO)}else{Ok(())}}}
mod g17_manager {
 use super::*;
 pub const G17P_SUBMIT_PHASE_IDLE:u32=0;
 #[derive(Debug,Copy,Clone,PartialEq,Eq)]pub struct G17PComputeRetirementTicket;
 pub struct G17PSksmQueue{pub id:u8,pub installed:bool,pub scheduler_owner:u32,pub fail_retarget:bool}
 impl G17PSksmQueue{pub fn queue_id(&self)->u8{self.id}pub fn requires_first_install(&self)->bool{!self.installed}
 pub fn bind_logical_channel_control(&mut self,c:&g17_resources::G17PLogicalQueueContext)->Result{if self.fail_retarget{return Err(EIO)}self.scheduler_owner=c.context_id();Ok(())}}
 pub struct G17PComputeUserBinding{pub vm:u64,pub drops:Arc<AtomicUsize>,pub _context:Arc<mmu::T8140ComputeExecutionContext>}
 impl G17PComputeUserBinding{pub fn matches(&self,vm:&mmu::Vm)->bool{self.vm==vm.id}
 pub fn retarget_logical_context(&mut self,c:Arc<g17_resources::G17PLogicalQueueContext>)->Result{if !self.matches(&c.vm)||!c.is_current(){return Err(EFAULT)}self._context=c;Ok(())}}
 impl Drop for G17PComputeUserBinding{fn drop(&mut self){self.drops.fetch_add(1,Ordering::SeqCst);}}
 pub struct Handshake{pub state:u32,pub epoch:u64}
 pub struct Notifications{pub activation:Option<u64>,pub direct_kick:Option<u64>}
 pub struct G17PComputeSubmissionContext<'a>{pub context_id:u32,pub dispatch_identity:u64,pub timestamps:g17_uapi::ComputeTimestampAddresses,pub descriptor_flag_4c:bool,pub converted_command_timestamp:u32,pub barriers:&'a [u8],pub mcache:Option<()>,pub rce_kind:u32,pub auxiliary:u64}
 pub struct Manager{pub fail_allocate:bool,pub fail_retain:bool,pub retains:usize,pub fail_bind:bool,pub fail_submit:u8,pub binds:usize,pub new_drops:Arc<AtomicUsize>,pub old_drops:Arc<AtomicUsize>,pub observed_old_drops:usize,pub epoch:u64,pub state:u32}
 impl Manager{
  pub fn retain_logical_channel_control(&mut self,_:&())->Result{if self.fail_retain{return Err(EIO)}self.retains+=1;Ok(())}
  pub fn cancel_unpublished_compute_queue(&mut self,q:&G17PSksmQueue)->bool{q.requires_first_install()}
  pub fn stage_compute_flist_state(&mut self)->Result{Ok(())}
  pub fn prepare_g17p_compute_queue(&mut self,_dev:&())->Result<G17PSksmQueue>{if self.fail_allocate{return Err(ENOSPC)}Ok(G17PSksmQueue{id:6,installed:false,scheduler_owner:0,fail_retarget:false})}
  pub fn bind_independent_compute_user_vm(&mut self,_q:&mut G17PSksmQueue,vm:&mmu::Vm,context:Arc<mmu::T8140ComputeExecutionContext>)->Result<G17PComputeUserBinding>{
   assert!(self.retains>0,"control must be retained before binding exposes it");
   self.binds+=1;self.observed_old_drops=self.old_drops.load(Ordering::SeqCst);
   let binding=G17PComputeUserBinding{vm:vm.id,drops:self.new_drops.clone(),_context:context};
   if self.fail_bind{Err(EIO)}else{Ok(binding)}
  }
  pub fn firmware_recovery_handshake_snapshot(&mut self)->Result<Handshake>{Ok(Handshake{state:self.state,epoch:self.epoch})}
  pub fn capture_independent_compute_retirement_ticket(&mut self,_q:&mut G17PSksmQueue)->Result<G17PComputeRetirementTicket>{Ok(G17PComputeRetirementTicket)}
  pub fn submit_translated_compute(&mut self,_regs:&(),q:&mut G17PSksmQueue,phase:&AtomicU32,binding:&G17PComputeUserBinding,_command:&(),ctx:G17PComputeSubmissionContext)->Result<Option<Notifications>>{
   assert_eq!(binding._context.context_id(),ctx.context_id);
   if self.fail_submit==1{return Err(EIO)}
   phase.store(1,Ordering::Release);q.installed=true;
   if self.fail_submit==2{return Err(EIO)}
   Ok(Some(Notifications{activation:None,direct_kick:Some(1)}))
  }
 }
}
''' + types + r'''
struct G17PLiveRuntime{
 accepting_submissions:bool,async_compute_queues:KVec<G17PAsyncComputeQueue>,manager:Option<g17_manager::Manager>,
 compute_binding:Option<()>,pending_compute:Option<()>,queue:Option<g17_manager::G17PSksmQueue>,
 queue_setup_pending:bool,drm:Option<()>,pdev:Option<()>,gfx_state:Option<Arc<State>>,registers:Option<()>,gfx_rtkit:Option<Rtkit>,
}
impl G17PLiveRuntime{
 fn is_crashed(&self)->bool{false}
 fn settle_open_recovery_before_publication(&mut self)->Result{Ok(())}
 fn wire_g17p_submit_gpu_power(&mut self)->Result{Ok(())}
 fn quarantine_async_compute(&mut self,owner:u64,_submission:u64,_error:Error)->bool{let q=self.async_compute_queues.iter_mut().find(|q|q.owner==Some(owner)).unwrap();q.quarantined=true;true}
''' + methods + r'''
}
fn counter()->Arc<AtomicUsize>{Arc::new(AtomicUsize::new(0))}
fn context(vm:u64,id:u32,drops:&Arc<AtomicUsize>)->Arc<mmu::T8140ComputeExecutionContext>{Arc::new(mmu::T8140ComputeExecutionContext{vm:mmu::Vm{id:vm},id,current:true,drops:drops.clone(),channel_control:()})}
fn runtime(old:&Arc<AtomicUsize>,new:&Arc<AtomicUsize>,roots:&Arc<AtomicUsize>)->G17PLiveRuntime{
 let ctx=context(1,10,roots);
 G17PLiveRuntime{accepting_submissions:true,async_compute_queues:KVec(vec![G17PAsyncComputeQueue{
 owner:None,queue:g17_manager::G17PSksmQueue{id:5,installed:true,scheduler_owner:10,fail_retarget:false},binding:Some(g17_manager::G17PComputeUserBinding{vm:1,drops:old.clone(),_context:ctx.clone()}),context:Some(ctx),previous_binding:None,active:None,released:true,retirement_ready:true,quarantined:false,retired_by_teardown:false,terminal_fanout:None,
 }]),manager:Some(g17_manager::Manager{fail_allocate:false,fail_retain:false,retains:0,fail_bind:false,fail_submit:0,binds:0,new_drops:new.clone(),old_drops:old.clone(),observed_old_drops:99,epoch:7,state:0}),compute_binding:None,pending_compute:None,queue:None,queue_setup_pending:false,drm:Some(()),pdev:Some(()),gfx_state:Some(Arc::new(State{submission_phase:AtomicU32::new(0)})),registers:Some(()),gfx_rtkit:Some(Rtkit{fail:false})}
}
fn prepare(r:&mut G17PLiveRuntime)->Result<(u8,Arc<mmu::T8140ComputeExecutionContext>)>{let c=context(2,12,&counter());r.prepare_async_compute_queue(20,&mmu::Vm{id:2},c)}
fn packet(r:&G17PLiveRuntime)->Arc<g17_job::G17PComputeJobPacket>{let q=r.async_compute_queues.iter().find(|q|q.owner==Some(20)).unwrap();Arc::new(g17_job::G17PComputeJobPacket{id:721,qid:q.queue.queue_id(),ctx:q.context.as_ref().unwrap().clone(),c:g17_job::Completion})}
#[test]fn released_installed_graph_reuses_qid_without_allocating(){
 let old=counter();let new=counter();let roots=counter();let mut r=runtime(&old,&new,&roots);
 r.manager.as_mut().unwrap().fail_allocate=true;
 for _ in 0..10_000{assert_eq!(prepare(&mut r).unwrap().0,5);}
 assert_eq!(r.manager.as_ref().unwrap().retains,1);
 assert_eq!(r.manager.as_ref().unwrap().binds,1);assert_eq!(r.async_compute_queues.len(),1);
 assert!(r.async_compute_queues[0].previous_binding.is_some());
 assert_eq!(old.load(Ordering::SeqCst),0);assert_eq!(roots.load(Ordering::SeqCst),0);
}
#[test]fn retention_failure_leaves_an_empty_unpublished_graph_for_retry(){
 let old=counter();let new=counter();let roots=counter();let mut r=runtime(&old,&new,&roots);
 r.manager.as_mut().unwrap().fail_retain=true;assert!(matches!(prepare(&mut r),Err(EIO)));
 assert_eq!(r.manager.as_ref().unwrap().retains,0);assert_eq!(r.manager.as_ref().unwrap().binds,0);
 assert_eq!(r.async_compute_queues.len(),1);let q=&r.async_compute_queues[0];
 assert!(q.owner.is_none()&&q.binding.is_some()&&q.context.is_some()&&!q.queue.requires_first_install());
 r.manager.as_mut().unwrap().fail_retain=false;prepare(&mut r).unwrap();
 assert_eq!(r.async_compute_queues.len(),1);assert_eq!(r.manager.as_ref().unwrap().retains,1);
}
#[test]fn failed_prepare_preserves_old_binding_root_and_queue_owner(){
 let old=counter();let new=counter();let roots=counter();let mut r=runtime(&old,&new,&roots);r.manager.as_mut().unwrap().fail_bind=true;
 assert!(prepare(&mut r).is_err());assert_eq!(old.load(Ordering::SeqCst),0);assert_eq!(roots.load(Ordering::SeqCst),0);assert_eq!(new.load(Ordering::SeqCst),1);assert_eq!(r.manager.as_ref().unwrap().observed_old_drops,0);
 let q=&r.async_compute_queues[0];assert_eq!(q.binding.as_ref().unwrap().vm,1);assert!(q.owner.is_none()&&q.released&&q.previous_binding.is_none());
}
#[test]fn closed_owner_rebinds_the_retained_installed_channel(){
 let old=counter();let new=counter();let roots=counter();let mut r=runtime(&old,&new,&roots);
 let(qid,_)=prepare(&mut r).unwrap();assert_eq!(qid,5);assert_eq!(r.async_compute_queues.len(),1);
 assert_eq!(r.async_compute_queues[0].queue.scheduler_owner,12);
 assert_eq!(r.async_compute_queues[0].binding.as_ref().unwrap().vm,2);
 assert!(r.async_compute_queues[0].previous_binding.is_some());
 r.release_async_compute_queue(20);
 assert_eq!(r.async_compute_queues.len(),1);
 assert_eq!(r.async_compute_queues[0].queue.scheduler_owner,10);
 assert!(r.async_compute_queues[0].owner.is_none());
 assert_eq!(old.load(Ordering::SeqCst),0);assert_eq!(new.load(Ordering::SeqCst),1);assert_eq!(roots.load(Ordering::SeqCst),0);
}
#[test]fn same_vm_new_owner_retargets_identity_without_allocating_a_qid(){
 let old=counter();let new=counter();let roots=counter();let mut r=runtime(&old,&new,&roots);
 let(qid,ctx)=r.prepare_async_compute_queue(20,&mmu::Vm{id:1},context(1,12,&counter())).unwrap();
 assert_eq!(qid,5);assert_eq!(ctx.context_id(),12);assert_eq!(r.async_compute_queues.len(),1);
 assert_eq!(r.async_compute_queues[0].queue.scheduler_owner,12);
 assert!(r.async_compute_queues[0].previous_binding.is_some());assert_eq!(old.load(Ordering::SeqCst),0);
 let(qid_again,again)=r.prepare_async_compute_queue(20,&mmu::Vm{id:1},ctx.clone()).unwrap();
 assert_eq!(qid_again,qid);assert!(Arc::ptr_eq(&ctx,&again));assert_eq!(r.manager.as_ref().unwrap().binds,0);
}
#[test]fn unpublished_failure_retries_only_its_own_channel(){
 let old=counter();let new=counter();let roots=counter();let mut r=runtime(&old,&new,&roots);prepare(&mut r).unwrap();
 r.manager.as_mut().unwrap().fail_submit=1;
 assert!(r.publish_async_compute(20,packet(&r),g17_drm::G17PComputeLeaseGuard).is_err());
 assert!(r.async_compute_queues[0].active.is_none());assert!(!r.async_compute_queues[0].queue.requires_first_install());
 r.manager.as_mut().unwrap().fail_submit=0;
 r.publish_async_compute(20,packet(&r),g17_drm::G17PComputeLeaseGuard).unwrap();
 assert!(r.async_compute_queues[0].active.is_some());
 assert_eq!(old.load(Ordering::SeqCst),0);
}
#[test]fn possible_publication_or_notification_failure_retains_all_owners(){
 for notification_failure in [false,true]{
 let old=counter();let new=counter();let roots=counter();let mut r=runtime(&old,&new,&roots);prepare(&mut r).unwrap();
 if notification_failure{r.gfx_rtkit.as_mut().unwrap().fail=true}else{r.manager.as_mut().unwrap().fail_submit=2}
 assert!(r.publish_async_compute(20,packet(&r),g17_drm::G17PComputeLeaseGuard).is_err());r.release_async_compute_queue(20);
 let q=&r.async_compute_queues[0];assert!(q.active.is_some()&&q.quarantined&&q.previous_binding.is_some());
 assert_eq!(old.load(Ordering::SeqCst),0);assert_eq!(new.load(Ordering::SeqCst),0);assert_eq!(roots.load(Ordering::SeqCst),0);
 }
}
#[test]fn failed_binding_leaves_an_unpublished_empty_graph_reusable(){
 let old=counter();let new=counter();let roots=counter();let mut r=runtime(&old,&new,&roots);r.manager.as_mut().unwrap().fail_bind=true;
 assert!(prepare(&mut r).is_err());assert_eq!(r.async_compute_queues.len(),1);
 assert!(r.async_compute_queues[0].binding.is_some());assert!(!r.async_compute_queues[0].queue.requires_first_install());
 r.manager.as_mut().unwrap().fail_bind=false;prepare(&mut r).unwrap();assert_eq!(r.async_compute_queues.len(),1);
 assert_eq!(r.async_compute_queues[0].queue.scheduler_owner,12);
}

'''

with tempfile.TemporaryDirectory() as directory:
    path = Path(directory)
    test = path / "rebind.rs"
    test.write_text(with_g17_config(rust))
    binary = path / "rebind"
    subprocess.run(["rustc", "--edition=2021", "--test", str(test), "-o", str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print("PASS production stable compute channels and prepare/publication ownership; no hardware claim")
