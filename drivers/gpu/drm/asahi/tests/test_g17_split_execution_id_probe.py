#!/usr/bin/env python3
"""Execute the production split-ID allocator, with shared-owner/drop checks."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]


def item(source, marker):
    start = source.index(marker)
    end = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


resources = (root / "g17_resources.rs").read_text()
manager = (root / "g17_manager.rs").read_text()
frontend = (root / "g17_drm.rs").read_text()
runtime = (root / "g17_async_compute.rs").read_text()
params = (root / "asahi.rs").read_text()
assert "default: 0" in item(params, "g17p_split_execution_id_probe: u32")
ensure = item(frontend, "    fn ensure_async_compute_queue(")
assert ensure.index("return Ok(())") < ensure.index(".prepare_compute_execution_context(")
assert "backend.context.clone()" in item(frontend, "    fn submit_compute_async(")
assert "g17p_split_execution_id_probe.value() != 0" in item(runtime, "    pub(crate) fn prepare_compute_execution_context(")

context = "\n".join(item(resources, marker) for marker in [
    "pub(crate) struct G17PLogicalQueueContext {",
    "impl G17PLogicalQueueContext {",
    "impl core::ops::Deref for G17PLogicalQueueContext {",
])
prepare = item(manager, "    pub(crate) fn prepare_compute_execution_context(")
rust = r'''
#![allow(dead_code)]
use std::{cell::Cell,ops::Deref};
type Result<T=()>=std::result::Result<T,i32>;
const EFAULT:i32=14;const EIO:i32=5;const GFP_KERNEL:u32=0;
macro_rules! dev_info {($($t:tt)*)=>{()}}
thread_local!{static FAIL_ARC:Cell<bool>=const{Cell::new(false)};}
struct Arc<T>(std::sync::Arc<T>);
impl<T> Clone for Arc<T>{fn clone(&self)->Self{Self(self.0.clone())}}
impl<T> Deref for Arc<T>{type Target=T;fn deref(&self)->&T{&self.0}}
impl<T> Arc<T>{fn new(v:T,_flags:u32)->Result<Self>{if FAIL_ARC.with(|f|f.replace(false)){Err(EIO)}else{Ok(Self(std::sync::Arc::new(v)))}}}
mod mmu{
 use super::*;
 #[derive(Clone)]pub struct Vm{pub id:u64,pub root:u64}
 pub struct T8140ComputeExecutionContext{pub vm:Vm,pub id:u32,pub current:bool,pub drops:std::rc::Rc<Cell<u32>>}
 impl T8140ComputeExecutionContext{
  pub fn context_id(&self)->u32{self.id} pub fn vm(&self)->&Vm{&self.vm}
  pub fn matches(&self,v:&Vm)->bool{self.vm.id==v.id&&self.vm.root==v.root}
  pub fn is_current(&self)->bool{self.current}
 }
 impl Drop for T8140ComputeExecutionContext{fn drop(&mut self){self.drops.set(self.drops.get()+1)}}
}
struct G17PClChannelControl{address:u64}impl G17PClChannelControl{fn gpu_va(&self)->u64{self.address}}
struct G17PLogicalQosLease{id:u8}
''' + context + r'''
mod g17_resources{pub(super)use super::G17PLogicalQueueContext;}
struct Uat{calls:Cell<u32>,fail:bool,id:u32,wrong_vm:bool,drops:std::rc::Rc<Cell<u32>>}
impl Uat{fn allocate_t8140_compute_execution_context(&self,v:&mmu::Vm)->Result<Arc<mmu::T8140ComputeExecutionContext>>{
 self.calls.set(self.calls.get()+1);if self.fail{return Err(EIO)}
 let mut vm=v.clone();if self.wrong_vm{vm.id+=1}
 // Root allocation success is separate from logical wrapper allocation.
 Ok(Arc(std::sync::Arc::new(mmu::T8140ComputeExecutionContext{vm,id:self.id,current:true,drops:self.drops.clone()})))
}}
struct Manager{uat:Uat}impl Manager{fn uat(&self)->&Uat{&self.uat}
''' + prepare + r'''
}
fn fixture()->(Manager,Arc<G17PLogicalQueueContext>){
 let drops=std::rc::Rc::new(Cell::new(0));
 let m=Manager{uat:Uat{calls:Cell::new(0),fail:false,id:6,wrong_vm:false,drops:drops.clone()}};
 let c=Arc::new(G17PLogicalQueueContext::new(
  Arc::new(mmu::T8140ComputeExecutionContext{vm:mmu::Vm{id:8,root:0x1001234000},id:5,current:true,drops},0).unwrap(),
  Arc::new(G17PClChannelControl{address:0xfffffc2180048000},0).unwrap(),
  Arc::new(G17PLogicalQosLease{id:3},0).unwrap(),0x12345678),0).unwrap();(m,c)
}
#[test]fn default_preserves_exact_arc_without_allocating(){let(mut m,c)=fixture();let d=m.prepare_compute_execution_context(&c,false).unwrap();assert!(std::sync::Arc::ptr_eq(&c.0,&d.0));assert_eq!(m.uat.calls.get(),0);}
#[test]fn split_changes_only_execution_slot_and_retains_shared_owners(){
 let(mut m,c)=fixture();let d=m.prepare_compute_execution_context(&c,true).unwrap();
 assert_eq!((c.context_id(),d.context_id()),(5,6));assert!(d.matches(c.vm()));
 assert!(std::sync::Arc::ptr_eq(&c.channel_control.0,&d.channel_control.0));
 assert!(std::sync::Arc::ptr_eq(&c.qos.0,&d.qos.0));assert_eq!(c.owner_pid,d.owner_pid);
 let page=std::sync::Arc::downgrade(&c.channel_control.0);let qos=std::sync::Arc::downgrade(&c.qos.0);
 drop(c);assert_eq!(m.uat.drops.get(),1);assert!(page.upgrade().is_some());assert!(qos.upgrade().is_some());
 drop(d);assert_eq!(m.uat.drops.get(),2);assert!(page.upgrade().is_none());assert!(qos.upgrade().is_none());
}
#[test]fn root_allocation_failure_keeps_original_owner(){let(mut m,c)=fixture();m.uat.fail=true;assert!(m.prepare_compute_execution_context(&c,true).is_err());assert_eq!(m.uat.drops.get(),0);assert_eq!(c.context_id(),5);}
#[test]fn wrapper_allocation_failure_returns_extra_root(){let(mut m,c)=fixture();FAIL_ARC.with(|f|f.set(true));assert!(m.prepare_compute_execution_context(&c,true).is_err());assert_eq!(m.uat.drops.get(),1);assert_eq!(c.context_id(),5);}
#[test]fn duplicate_id_or_wrong_vm_rejected(){for duplicate in [true,false]{let(mut m,c)=fixture();if duplicate{m.uat.id=5}else{m.uat.wrong_vm=true}assert!(m.prepare_compute_execution_context(&c,true).is_err());assert_eq!(m.uat.drops.get(),1);}}
#[test]fn stale_original_rejected_before_allocation(){let(mut m,mut c)=fixture();std::sync::Arc::get_mut(&mut std::sync::Arc::get_mut(&mut c.0).unwrap().execution.0).unwrap().current=false;assert!(m.prepare_compute_execution_context(&c,true).is_err());assert_eq!(m.uat.calls.get(),0);}
'''
with tempfile.TemporaryDirectory() as directory:
    path = Path(directory)
    source = path / "probe.rs"
    source.write_text(rust)
    binary = path / "probe"
    built = subprocess.run(["rustc", "--edition=2021", "--test", str(source), "-o", str(binary)], capture_output=True, text=True)
    if built.returncode:
        raise SystemExit(built.stderr[:6000])
    subprocess.run([str(binary)], check=True)
print("PASS split execution-ID diagnostic; hardware and performance effects unqualified")
