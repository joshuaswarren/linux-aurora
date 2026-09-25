#!/usr/bin/env python3
"""Run the actual publication-frontier/token/error helpers with mock fences."""
from pathlib import Path
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
drm = (root / 'g17_drm.rs').read_text()
job = (root / 'g17_job.rs').read_text()
live = (root / 'g17_live_boot.rs').read_text()
comp = (root / 'g17_async_compute.rs').read_text()
manager = (root / 'g17_manager.rs').read_text()
codec = (root / 'g17_submission.rs').read_text()

def block(text, marker):
    a = text.index(marker)
    b = text.index('{', a) + 1
    depth = 1
    while depth:
        depth += (text[b] == '{') - (text[b] == '}')
        b += 1
    return text[a:b]

def item(name):
    match = re.search(r'^(?:pub\(crate\) )?(?:(?:const )?fn|enum|struct) ' + name + r'\b', codec, re.M)
    assert match, name
    return block(codec, match.group())

constants = []
for name in ['KICK_TS_BITS', 'KICK_TS_MASK', 'KICK_QID_BITS', 'KICK_QID_MASK',
             'KICK_QID_SHIFT', 'KICK_STAMP_SHIFT', 'KICK_STAMP_INDEX_MASK',
             'KICK_RESERVED_MASK', 'KICK_SELECTOR_BIT']:
    constants.append(re.search(r'^(?:pub\(crate\) )?const ' + name + r': .*?;', codec, re.M | re.S).group())

program = r'''
#![allow(dead_code,unused_imports)]
use std::sync::{Arc,atomic::{AtomicI32,AtomicU64,Ordering}};
#[derive(Debug,Copy,Clone,PartialEq)]struct Error(i32);
impl Error {fn from_errno(e:i32)->Self{Self(e)}}
type Result<T=()> = std::result::Result<T,Error>;
const EIO:Error=Error(-5);const ENODEV:Error=Error(-19);const GFP_KERNEL:()=();
#[derive(Clone)]struct Fence {id:u64,status:Arc<AtomicI32>}
impl Fence {fn raw(&self)->*const Self{self}}
mod bindings {pub unsafe fn dma_fence_get_status(f:*const super::Fence)->i32{(*f).status.load(super::Ordering::Acquire)}}
mod kernel {pub(crate) use crate::bindings;pub mod dma_fence {
 pub trait RawDmaFence{fn raw(&self)->*const crate::Fence;}
 impl RawDmaFence for crate::Fence{fn raw(&self)->*const crate::Fence{self}}
}}
mod g17_status {pub struct VmStatus;}
#[derive(Default)]struct KVec<T>(Vec<T>);
impl<T>KVec<T>{fn push(&mut self,x:T,_f:())->Result{self.0.push(x);Ok(())}}
impl<T>std::ops::Deref for KVec<T>{type Target=[T];fn deref(&self)->&[T]{&self.0}}
mod g17_job {#[derive(Clone)]pub struct G17PCommandFences {pub published:crate::Fence,pub completed:crate::Fence}}
'''
program += '\nmod g17_submission {\n' + '\n'.join(constants)
for name in ['SubmissionError', 'KickTimestamp', 'SksmKickWord', 'G17PClBarrierDependency', 'G17PRenderOrder']:
    program += '\n#[derive(Debug,Copy,Clone,PartialEq,Eq)]\n' + item(name)
program += '''
#[derive(Copy,Clone)]pub struct Geometry;impl Geometry{fn stamp_count(self)->u16{256}}
#[derive(Copy,Clone)]pub struct G17PClKickProducerState{pub base_timestamp:u64,pub geometry:Geometry}
'''
for name in ['encode_kick_word', 'decode_kick_word', 'encode_kick_timestamp',
             'g17p_render_barrier_from_timestamp', 'g17p_render_prefix_contains',
             'g17p_live_render_dependency', 'encode_g17p_cl_dependency',
             'encode_g17p_cl_barrier_dependency']:
    program += '\n' + item(name)
program += '''
pub fn native_word(d:G17PClBarrierDependency)->u64{
 encode_g17p_cl_barrier_dependency(G17PClKickProducerState{base_timestamp:0xabc00,geometry:Geometry},d).unwrap()
}
}\n'''
program += block(drm, 'fn append_render_prefix_dependencies(')
program += '\nstruct G17PQueue;impl G17PQueue {' + block(drm, '    fn check_firmware_fence(') + '}\n'
program += r'''
struct Render {owner:u64,status:Arc<g17_status::VmStatus>,render_order:g17_submission::G17PRenderOrder,fragment_dependency:AtomicU64,fence:Fence}
impl Render {fn fence(&self)->Fence{self.fence.clone()}
'''+block(drm, '    pub(crate) fn compute_predecessor_dependency(')+'}\n'
program += r'''
struct Producer{submitted:u64,previous_valid:bool,last_add_kicks_timestamp:u64}
struct HardwareQueue{queue_id:u8,producer:Producer}
impl HardwareQueue {
'''+block(manager, '    pub(crate) fn published_compute_dependency(')+'}\n'
program += r'''
struct ComputeCompletion{status:Arc<g17_status::VmStatus>,fence:Fence}
impl ComputeCompletion{fn fence(&self)->Fence{self.fence.clone()}}
struct Packet{completion:ComputeCompletion}
impl Packet{fn completion(&self)->Result<&ComputeCompletion>{Ok(&self.completion)}}
struct Ticket{recovery_epoch:u64}
struct Active{packet:Packet,retirement:Option<Ticket>}
struct Queue{owner:Option<u64>,active:Option<Active>,quarantined:bool,queue:HardwareQueue}
#[derive(Copy,Clone)]struct Handshake{state:u32,epoch:u64}
struct Manager{h:Handshake}
impl Manager{fn firmware_recovery_handshake_snapshot(&mut self)->Result<Handshake>{Ok(self.h)}}
struct Runtime{manager:Option<Manager>,async_compute_queues:Vec<Queue>}
impl Runtime {
'''+block(comp, '    pub(crate) fn async_compute_native_dependency(')+'}\n'
program += r'''
fn fence(id:u64,status:i32)->Fence{Fence{id,status:Arc::new(AtomicI32::new(status))}}
fn pair(id:u64)->g17_job::G17PCommandFences{g17_job::G17PCommandFences{published:fence(id+100,1),completed:fence(id,0)}}
fn render(vm:&Arc<g17_status::VmStatus>,seq:u64,qid:u8,stamp:u64)->Render{
 Render{owner:7,status:vm.clone(),render_order:g17_submission::G17PRenderOrder{sequence:seq,wait_through:None},
 fragment_dependency:AtomicU64::new(g17_submission::encode_kick_word(stamp,qid).unwrap()),fence:fence(seq,0)}
}
fn runtime(vm:&Arc<g17_status::VmStatus>)->Runtime{
 Runtime{manager:Some(Manager{h:Handshake{state:0,epoch:4}}),async_compute_queues:vec![Queue{
 owner:Some(7),quarantined:false,queue:HardwareQueue{queue_id:11,producer:Producer{submitted:1,previous_valid:true,last_add_kicks_timestamp:0x1000005}},
 active:Some(Active{packet:Packet{completion:ComputeCompletion{status:vm.clone(),fence:fence(1,0)}},retirement:Some(Ticket{recovery_epoch:4})})}]}
}
#[test]fn prefix_waits_for_publication_without_waiting_for_hardware(){
 let f=[(1,pair(1)),(2,pair(2)),(3,pair(3))];let mut d=KVec(Vec::new());let mut c=KVec(Vec::new());
 append_render_prefix_dependencies(&f,Some(2),&mut d,&mut c).unwrap();
 assert_eq!(d.iter().map(|f|f.id).collect::<Vec<_>>(),vec![101,102]);
 assert_eq!(c.iter().map(|f|f.id).collect::<Vec<_>>(),vec![1,2]);
 for f in d.iter(){assert_eq!(f.status.load(Ordering::Acquire),1)}
 for f in c.iter(){assert_eq!(f.status.load(Ordering::Acquire),0);assert!(G17PQueue::check_firmware_fence(f).is_ok())}
 c[0].status.store(-5,Ordering::Release);assert_eq!(G17PQueue::check_firmware_fence(&c[0]),Err(EIO));
}
#[test]fn complete_live_render_prefix_encodes_both_physical_pairs(){
 let vm=Arc::new(g17_status::VmStatus);let a=render(&vm,41,7,0x1ffffff);let b=render(&vm,42,9,0x2000000);
 let da=a.compute_predecessor_dependency(7,&vm,Some(42),None).unwrap().unwrap();
 let db=b.compute_predecessor_dependency(7,&vm,Some(42),None).unwrap().unwrap();
 assert_eq!(g17_submission::native_word(da),g17_submission::encode_kick_word(0x1ffffff,7).unwrap());
 assert_eq!(g17_submission::native_word(db),g17_submission::encode_kick_word(0x2000000,9).unwrap());
 assert_eq!(b.compute_predecessor_dependency(7,&vm,Some(41),None),Ok(None));
 assert!(b.compute_predecessor_dependency(7,&vm,Some(41),Some(42)).unwrap().is_some());
}
#[test]fn render_owner_vm_failure_and_missing_token_are_checked(){
 let vm=Arc::new(g17_status::VmStatus);let other=Arc::new(g17_status::VmStatus);let a=render(&vm,1,7,3);
 assert_eq!(a.compute_predecessor_dependency(8,&vm,Some(1),None),Ok(None));
 assert_eq!(a.compute_predecessor_dependency(7,&other,Some(1),None),Ok(None));
 a.fragment_dependency.store(u64::MAX,Ordering::Release);assert!(a.compute_predecessor_dependency(7,&vm,Some(1),None).is_err());
 a.fence.status.store(1,Ordering::Release);assert_eq!(a.compute_predecessor_dependency(7,&vm,Some(1),None),Ok(None));
 a.fence.status.store(-125,Ordering::Release);assert!(a.compute_predecessor_dependency(7,&vm,Some(1),None).is_err());
}
#[test]fn compute_to_render_uses_published_stamp_not_consumer_seed(){
 let vm=Arc::new(g17_status::VmStatus);let mut rt=runtime(&vm);
 let d=rt.async_compute_native_dependency(7,&vm).unwrap().unwrap();
 assert_eq!(g17_submission::native_word(d),g17_submission::encode_kick_word(0x1000005,11).unwrap());
 // Both physical render pairs see the same live compute predecessor.
 assert_eq!(rt.async_compute_native_dependency(7,&vm).unwrap(),Some(d));
 rt.async_compute_queues[0].active=None;assert_eq!(rt.async_compute_native_dependency(7,&vm),Ok(None));
}
#[test]fn compute_token_refuses_recovery_epoch_quarantine_and_cancellation(){
 let vm=Arc::new(g17_status::VmStatus);let mut rt=runtime(&vm);
 rt.manager.as_mut().unwrap().h.epoch=5;assert!(rt.async_compute_native_dependency(7,&vm).is_err());
 rt.manager.as_mut().unwrap().h.epoch=4;rt.manager.as_mut().unwrap().h.state=1;assert!(rt.async_compute_native_dependency(7,&vm).is_err());
 rt.manager.as_mut().unwrap().h.state=0;rt.async_compute_queues[0].quarantined=true;assert!(rt.async_compute_native_dependency(7,&vm).is_err());
 rt.async_compute_queues[0].quarantined=false;rt.async_compute_queues[0].active.as_ref().unwrap().packet.completion.fence.status.store(-125,Ordering::Release);
 assert!(rt.async_compute_native_dependency(7,&vm).is_err());
}
#[test]fn compute_token_requires_exact_publication_and_vm(){
 let vm=Arc::new(g17_status::VmStatus);let mut rt=runtime(&vm);
 assert_eq!(rt.async_compute_native_dependency(8,&vm),Ok(None));
 assert!(rt.async_compute_native_dependency(7,&Arc::new(g17_status::VmStatus)).is_err());
 rt.async_compute_queues[0].queue.producer.previous_valid=false;assert!(rt.async_compute_native_dependency(7,&vm).is_err());
 rt.async_compute_queues[0].queue.producer.previous_valid=true;rt.async_compute_queues[0].active.as_mut().unwrap().retirement=None;
 assert!(rt.async_compute_native_dependency(7,&vm).is_err());
}
'''

# The kernel scheduled fence is signalled only after native publish returns.
sched = (root.parent / 'scheduler/sched_main.c').read_text()
assert sched.index('fence = sched->ops->run_job(sched_job);') < sched.index('drm_sched_fence_scheduled(s_fence, fence);')
assert job.count('let published = job.fences().scheduled();') == 2
assert job.count('Ok(G17PCommandFences { published, completed: hardware })') == 2
assert 'barriers: &dependencies[..dependency_count]' in comp
assert 'packet.command().render_wait_through, packet.command().render_wait_last' in comp
assert 'completion.owner(), &completion.vm_status(),' in live
assert 'get_mut(dependency_count).ok_or(EOVERFLOW)?' in live
for name in ['submit_compute_async', 'submit_render_async']:
    body = block(drm, '    fn '+name+'(')
    assert 'dependencies.push(self.last_publication.as_ref().ok_or(EIO)?.clone()' in body
    assert 'checked_dependencies.push(prior.clone()' not in body
assert 'dma_fence_wait_timeout' not in block(drm, '    fn check_firmware_fence(')
assert 'MAX_IN_FLIGHT_PER_QUEUE: u32 = 2' in (root / 'g17_queue_limits.rs').read_text()

with tempfile.TemporaryDirectory() as directory:
    src = Path(directory) / 'cross.rs'
    exe = Path(directory) / 'cross'
    src.write_text(program)
    subprocess.run(['rustc', '--edition=2021', '--test', str(src), '-o', str(exe)], check=True)
    subprocess.run([str(exe), '--test-threads=1'], check=True, timeout=60)
print('PASS actual cross-engine publication, native token, owner/epoch/error helpers; no hardware claim')
