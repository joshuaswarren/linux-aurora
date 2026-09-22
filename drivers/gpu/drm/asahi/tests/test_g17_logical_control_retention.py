#!/usr/bin/env python3
"""Production logical-context allocation, retention and stopped-session release."""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
manager = (ROOT / 'g17_manager.rs').read_text()
resources = (ROOT / 'g17_resources.rs').read_text()
async_compute = (ROOT / 'g17_async_compute.rs').read_text()


def item(source, marker):
    start = source.index(marker)
    end = source.index('{', start) + 1
    depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


control = resources[resources.index('impl G17PClChannelControl {'):]
methods = '\n'.join(item(manager, marker) for marker in [
    'pub(crate) fn prepare_logical_queue_context(',
    'pub(crate) fn retain_logical_channel_control(',
    'fn release_logical_channel_controls_after_processor_stop(',
])
code = r'''
#![allow(dead_code)]
extern crate self as kernel;
use std::sync::atomic::{AtomicBool,AtomicU32,Ordering};
type Result<T=()> = std::result::Result<T,i32>;
const EOVERFLOW:i32=75;const EIO:i32=5;const EINVAL:i32=22;const ENOMEM:i32=12;const GFP_KERNEL:()=();
struct Arc<T>(std::sync::Arc<T>);
impl<T> Arc<T>{fn new(value:T,_:())->Result<Self>{Ok(Self(std::sync::Arc::new(value)))}
 fn pin_init(value:T,_:())->Result<Self>{Self::new(value,())}}
impl<T> Clone for Arc<T>{fn clone(&self)->Self{Self(self.0.clone())}}
impl<T> std::ops::Deref for Arc<T>{type Target=T;fn deref(&self)->&T{&self.0}}
pub mod sync{pub type Mutex<T> = std::sync::Mutex<T>;}
struct Current;impl Current{fn group_leader(&self)->&Self{self}fn pid(&self)->i32{42}}
#[macro_export]macro_rules! current{()=>{crate::Current}}
#[macro_export]macro_rules! new_mutex{($value:expr,$name:expr)=>{std::sync::Mutex::new($value)}}
struct AsahiDevice;
mod mmu{
 use super::*;pub const UAT_PGSZ:usize=0x4000;pub const PROT_GPU_FW_SHARED_RW:()=();
 pub struct Vm;pub struct Uat;pub struct T8140ComputeExecutionContext;
 impl Uat{pub fn allocate_t8140_compute_execution_context(&self,_:&Vm)->Result<Arc<T8140ComputeExecutionContext>>{Arc::new(T8140ComputeExecutionContext,())}}
}
static LIVE_PAGES:AtomicU32=AtomicU32::new(0);
static NEXT_OBJECT_ID:AtomicU32=AtomicU32::new(1);
fn g17p_allocate_object_id()->Result<u32>{Ok(NEXT_OBJECT_ID.fetch_add(1,Ordering::SeqCst))}
struct MappedObject;
impl MappedObject{
 fn new_wc(_:&AsahiDevice,_:&mmu::Uat,size:usize,_:u64,_:())->Result<Self>{assert_eq!(size,0x4000);LIVE_PAGES.fetch_add(1,Ordering::SeqCst);Ok(Self)}
 fn with_bytes_mut<T>(&mut self,f:impl FnOnce(&mut[u8])->Result<T>)->Result<T>{f(&mut [0;0x4000])}
}
impl Drop for MappedObject{fn drop(&mut self){LIVE_PAGES.fetch_sub(1,Ordering::SeqCst);}}
mod g17_submission{pub fn apply_g17p_cl_channel_control_static_state(_:&mut[u8])->std::result::Result<(),i32>{Ok(())}}
mod g17_resources {
 use super::*;
 const G17P_CL_CHANNEL_CONTROL_SIZE:usize=0x4000;
''' + item(resources, 'pub(crate) struct G17PClChannelControl {') + r'''
 impl G17PClChannelControl {
''' + '\n'.join(item(control, marker) for marker in [
    'pub(crate) fn new(', 'pub(crate) fn is_session_retained(',
    'pub(crate) fn set_session_retained(',
]) + r'''
 }
 pub struct G17PLogicalQosLease;
 impl G17PLogicalQosLease{pub fn new(_:Arc<kernel::sync::Mutex<u128>>)->Result<Arc<Self>>{Arc::new(Self,())}}
''' + item(resources, 'pub(crate) struct G17PLogicalQueueContext {') + r'''
 impl G17PLogicalQueueContext {
''' + item(resources[resources.index('impl G17PLogicalQueueContext {'):], 'pub(crate) fn new(') + r'''
 }
}
struct KVec<T>{items:Vec<T>,fail_reserve:bool,fail_push:bool,reserves:usize}
impl<T>KVec<T>{fn new()->Self{Self{items:vec![],fail_reserve:false,fail_push:false,reserves:0}}
 fn reserve(&mut self,n:usize,_:())->Result{self.reserves+=1;if self.fail_reserve{return Err(ENOMEM)}self.items.reserve(n);Ok(())}
 fn push(&mut self,t:T,_:())->Result{if self.fail_push{return Err(ENOMEM)}self.items.push(t);Ok(())}}
impl<T>std::ops::Deref for KVec<T>{type Target=Vec<T>;fn deref(&self)->&Vec<T>{&self.items}}
impl<T>std::ops::DerefMut for KVec<T>{fn deref_mut(&mut self)->&mut Vec<T>{&mut self.items}}
impl<'a,T>IntoIterator for &'a KVec<T>{type Item=&'a T;type IntoIter=std::slice::Iter<'a,T>;fn into_iter(self)->Self::IntoIter{self.items.iter()}}
struct Manager{logical_channel_controls:KVec<Arc<g17_resources::G17PClChannelControl>>,logical_qos_ids:Option<Arc<kernel::sync::Mutex<u128>>>}
impl Manager{fn new()->Self{Self{logical_channel_controls:KVec::new(),logical_qos_ids:None}}fn uat(&self)->&mmu::Uat{&mmu::Uat}
''' + methods + r'''
}
#[test]fn ten_thousand_create_close_cycles_retain_no_control_pages(){
 assert_eq!(LIVE_PAGES.load(Ordering::SeqCst),0);let mut m=Manager::new();
 for _ in 0..10_000{let c=m.prepare_logical_queue_context(&AsahiDevice,&mmu::Vm).unwrap();
  assert!(!c.channel_control.is_session_retained());assert_eq!(LIVE_PAGES.load(Ordering::SeqCst),1);
  drop(c);assert_eq!(LIVE_PAGES.load(Ordering::SeqCst),0);assert!(m.logical_channel_controls.is_empty());}
 assert_eq!(m.logical_channel_controls.reserves,0);
}
#[test]fn shared_render_compute_retention_is_exact_and_constant_time_after_first_bind(){
 let mut m=Manager::new();let c=m.prepare_logical_queue_context(&AsahiDevice,&mmu::Vm).unwrap();
 for _ in 0..10_000{m.retain_logical_channel_control(&c.channel_control).unwrap();}
 assert_eq!(m.logical_channel_controls.len(),1);assert_eq!(m.logical_channel_controls.reserves,1);
 let sibling=m.prepare_logical_queue_context(&AsahiDevice,&mmu::Vm).unwrap();
 m.retain_logical_channel_control(&sibling.channel_control).unwrap();assert_eq!(m.logical_channel_controls.len(),2);
 // Binding alone is sufficient to conservatively retain; no publication is needed.
 drop(c);drop(sibling);assert_eq!(LIVE_PAGES.load(Ordering::SeqCst),2);
 m.release_logical_channel_controls_after_processor_stop();assert_eq!(LIVE_PAGES.load(Ordering::SeqCst),0);
}
#[test]fn reserve_and_push_failures_do_not_seal_or_retain_the_page(){
 for push in [false,true]{let mut m=Manager::new();let c=m.prepare_logical_queue_context(&AsahiDevice,&mmu::Vm).unwrap();
  m.logical_channel_controls.fail_reserve=!push;m.logical_channel_controls.fail_push=push;
  assert_eq!(m.retain_logical_channel_control(&c.channel_control),Err(ENOMEM));
  assert!(!c.channel_control.is_session_retained());assert!(m.logical_channel_controls.is_empty());
  m.logical_channel_controls.fail_reserve=false;m.logical_channel_controls.fail_push=false;
  m.retain_logical_channel_control(&c.channel_control).unwrap();assert!(c.channel_control.is_session_retained());
  drop(c);m.release_logical_channel_controls_after_processor_stop();assert_eq!(LIVE_PAGES.load(Ordering::SeqCst),0);}
}
#[test]fn stopped_session_clears_marker_before_a_surviving_context_can_retain_again(){
 let mut m=Manager::new();let c=m.prepare_logical_queue_context(&AsahiDevice,&mmu::Vm).unwrap();
 m.retain_logical_channel_control(&c.channel_control).unwrap();m.release_logical_channel_controls_after_processor_stop();
 assert!(!c.channel_control.is_session_retained());assert_eq!(LIVE_PAGES.load(Ordering::SeqCst),1);
 m.retain_logical_channel_control(&c.channel_control).unwrap();assert_eq!(m.logical_channel_controls.len(),1);
 drop(c);m.release_logical_channel_controls_after_processor_stop();assert_eq!(LIVE_PAGES.load(Ordering::SeqCst),0);
}
'''

# The methods above establish the lifetime contract; these checks connect it
# to every current logical pointer-exposure route, including retained retargets.
handoff = item(manager, 'pub(crate) fn handoff_retained_render_vm(')
assert handoff.index('self.retain_logical_channel_control(') < handoff.index('self.retarget_retained_render_context(')
begin = item(manager, 'pub(crate) fn begin_translated_render(')
assert begin.index('self.retain_logical_channel_control(') < begin.index('self.begin_translated_render_with_hardware_buffer(')
prepare = item(async_compute, 'pub(crate) fn prepare_async_compute_queue(')
assert prepare.index('manager.retain_logical_channel_control(') < prepare.index('q.queue.bind_logical_channel_control(')
assert prepare.index('manager.retain_logical_channel_control(') < prepare.index('manager.bind_independent_compute_user_vm(')
# Reused/rollback bindings already belong to retained contexts; fresh bindings
# enter through prepare_async_compute_queue. Bootstrap construction uses None.
assert manager.count('.new_user_render_storage(') == 2
assert manager.count('self.logical_channel_controls.clear()') == 1

with tempfile.TemporaryDirectory(prefix='g17-control-retention-') as temp:
    path = Path(temp)
    (path / 'test.rs').write_text(code)
    subprocess.run(['rustc', '--edition=2021', '--test', str(path / 'test.rs'),
                    '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test'), '--test-threads=1'], check=True)
print('PASS production control creation/retention/reset and publication boundary wiring')
