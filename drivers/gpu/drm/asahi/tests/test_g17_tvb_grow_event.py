#!/usr/bin/env python3
"""Run the production event codec, owner dispatch, and TVB commit with host storage."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
LIVE = (ROOT / 'g17_live_boot.rs').read_text()
DRIVER = (ROOT / 'driver.rs').read_text()
MANAGER = (ROOT / 'g17_manager.rs').read_text()

drain = LIVE[LIVE.index('fn drain_primary_firmware_event_batch('):]
drain = drain[:drain.index('fn complete_primary_real_recovery(')]
assert drain.index('G17PTvbGrowDisposition::Deferred') < drain.index('break;', drain.index('G17PTvbGrowDisposition::Deferred'))
service = LIVE[LIVE.index('    pub(crate) fn service_primary_firmware_events('):]
service = service[:service.index('    fn service_firmware_events_inline(')]
assert service.index('flush_pending_primary_device_control') < service.index('notifications_pending')
assert 'note_primary_device_control_backpressure' in service
assert 'if !self.primary_device_control_backpressured()' in service
inline = LIVE[LIVE.index('    fn service_firmware_events_inline('):]
inline = inline[:inline.index('    pub(crate) fn service_primary_recovery(')]
assert inline.index('primary_tvb_reply_pending') < inline.index('firmware_event_notifications')
assert 'pending_control && !self.flush_pending_primary_device_control()?' in inline
assert 'pending_release && !self.flush_pending_render_usc_freelist_release()?' in inline
worker = DRIVER[DRIVER.index('impl WorkItem<1>'):DRIVER.index('impl WorkItem<2>')]
assert worker.index('service_primary_firmware_events') < worker.index('fsleep(Delta::from_millis(1))')
requeue = DRIVER[DRIVER.index('fn requeue_g17p_firmware_event_worker_if_pending('):]
assert requeue.index('runtime.mutex().lock()') < requeue.index('event_worker_armed()') < requeue.index('enqueue')
for marker in ('publish_primary_device_control_record(',
               'publish_render_usc_freelist_request(',
               'publish_render_usc_freelist_release('):
    method = MANAGER[MANAGER.index('    pub(crate) fn ' + marker):]
    method = method[:method.index('\n    }')]
    assert ('pending_primary_control.is_some()' in method
            or 'primary_device_control_backpressured()' in method)

def item(file, marker):
    source = (ROOT / file).read_text()
    start = source.index(marker)
    end = source.index('{', start) + 1
    depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    prefix = '#[derive(Debug, Copy, Clone, PartialEq, Eq)]\n' if marker.startswith('struct G17PPm') else ''
    return prefix + source[start:end]

code = r'''
#![allow(dead_code, unused_variables)]
use std::cell::Cell;
use std::sync::atomic::Ordering;
type Result<T=()> = std::result::Result<T, i32>;
const EINVAL:i32=22; const ENOENT:i32=2; const EIO:i32=5;
const EFAULT:i32=14; const EOVERFLOW:i32=75; const ERANGE:i32=34;
const EAGAIN:i32=11; const ENOMEM:i32=12; const EBUSY:i32=16;
const ETIMEDOUT:i32=110; const GFP_KERNEL:()=();
macro_rules! dev_info { ($($t:tt)*) => {()} }
macro_rules! dev_warn { ($($t:tt)*) => {()} }
struct KVec<T>(Vec<T>);
impl<T> KVec<T> {
 fn with_capacity(n:usize,_:())->Result<Self>{Ok(Self(Vec::with_capacity(n)))}
 fn push(&mut self,t:T,_:())->Result{self.0.push(t);Ok(())}
}
impl<T> std::ops::Deref for KVec<T>{type Target=Vec<T>;fn deref(&self)->&Vec<T>{&self.0}}
impl<T> std::ops::DerefMut for KVec<T>{fn deref_mut(&mut self)->&mut Vec<T>{&mut self.0}}
impl<T> IntoIterator for KVec<T>{type Item=T;type IntoIter=std::vec::IntoIter<T>;fn into_iter(self)->Self::IntoIter{self.0.into_iter()}}
mod mmu {
 use super::*;
 pub struct Vm {pub blocks:Cell<usize>,pub fail:Cell<bool>}
 impl Vm {
  pub fn render_pool_tvb_blocks(&self,_:u8,_:u64)->Option<usize>{Some(self.blocks.get())}
  pub fn append_render_pool_mappings(&self,_:u8,_:u64,before:usize,after:usize,aliases:usize)->Result{
   if self.fail.get(){return Err(ENOMEM)}
   assert_eq!(before,self.blocks.get());assert_eq!(aliases,after-before);
   self.blocks.set(after);Ok(())
  }
 }
}
mod g17_submission {
 pub const G17P_PBDESC_ENTRY_SIZE:usize=16;
 pub fn g17p_pbdesc_offset(id:u32)->usize{id as usize*16}
 #[derive(Default)]pub struct G17PPbDescRingState;
 pub fn encode_g17p_pbdesc(_:[u32;4],address:u64,pages:u32,_:G17PPbDescRingState)->[u32;4]{[address as u32,pages,0,0]}
}
''' + f'#[path="{ROOT}/g17_completion.rs"] mod g17_completion;\n' + r'''
mod g17_resources {
 use super::*;
 const G17P_PM_MAX_BLOCKS:usize=0xd1a;
 const G17P_PM_MAX_OWNED_BLOCKS:usize=G17P_PM_MAX_BLOCKS-1;
 const G17P_PM_MAX_BLOCKS_NOMEMLESS:usize=0x45e;
 const G17P_PM_PAGES_PER_BLOCK:usize=4;
 const G17P_RENDER_TVB_BLOCK_SIZE:usize=0x20000;
 const G17P_RENDER_TVB_BLOCK_STRIDE:usize=0x28000;
 const G17P_RENDER_USER_BASE:u64=0x10_0000_0000;
 const G17P_RENDER_USER_END:u64=G17P_RENDER_USER_BASE+0x1_0000_0000;
 const G17P_PM_PAGE_SIZE:usize=0x8000;
 const G17P_PM_SHARED_TOTAL_CURSOR:usize=0;
 const G17P_PM_SHARED_COMMITTED_CURSOR:usize=4;
 const G17P_PM_SHARED_READ_CURSOR:usize=8;
 fn put_u32(raw:&mut[u8],offset:usize,value:u32){raw[offset..offset+4].copy_from_slice(&value.to_le_bytes())}
 fn g17p_render_tvb_page_id(_:Pair,index:usize)->Option<u32>{Some(0x1000+index as u32*8)}
 #[derive(Copy,Clone)]struct Pair(u8);impl Pair{fn slot(self)->u8{self.0}}
 pub struct Object{pub raw:Vec<u8>}
 impl Object{fn with_bytes_mut<T>(&mut self,f:impl FnOnce(&mut[u8])->Result<T>)->Result<T>{if self.raw.is_empty(){return Err(EFAULT)}f(&mut self.raw)}}
 #[derive(Copy,Clone)]struct Layout{total_size:usize,shared_control:usize,block_page_bases:usize,page_list:usize,hwpb_state:usize}
 struct Pm{backing:Object,layout:Layout,hardware_buffer_id:u32,page_count:u32}
 impl Pm{fn page_list_low_va(&self)->u64{0x98760000}}
 fn block_address(index:usize)->u64{G17P_RENDER_USER_BASE+(0x1000+index as u64*8)*G17P_PM_PAGE_SIZE as u64}
 struct Growth{extension:usize,mappings:usize,addresses:KVec<u64>}
 struct Tvb{block_count:usize,logical_size:usize,extensions:KVec<usize>,block_addresses:KVec<u64>,fail:bool}
 impl Tvb {
  fn all_reachable_from(&self,_:&mmu::Vm)->bool{true}
  fn prepare_growth(&mut self,_:(),_:&mmu::Vm,blocks:usize)->Result<Option<Growth>>{
   if self.fail{return Err(ENOMEM)}
   self.extensions.0.reserve(1);
   self.block_addresses.0.reserve(blocks-self.block_count);
   Ok(Some(Growth{extension:blocks-self.block_count,mappings:blocks-self.block_count,
    addresses:KVec((self.block_count..blocks).map(block_address).collect())}))
  }
 }
 pub struct G17PUserRenderStorage{render_tvb:Option<Tvb>,parameter_management:Option<Pm>,queue_pair:Pair}
 impl G17PUserRenderStorage {
  fn render_pool_id(&self)->u64{1}
  pub fn tvb_capacity(&self)->usize{self.render_tvb.as_ref().unwrap().block_count}
  pub fn retained_hardware_buffer_id(&self)->Option<u32>{self.parameter_management.as_ref().map(|pm|pm.hardware_buffer_id)}
  pub fn new(id:u32,blocks:usize,slot:u8)->Self {
   let layout=Layout{total_size:0x30000,shared_control:0x100,block_page_bases:0x200,page_list:0x10000,hwpb_state:0};
   let mut raw=vec![0xa5;layout.total_size];
   for (offset,v) in [(0,blocks as u32),(4,blocks as u32),(8,7)]{put_u32(&mut raw,layout.shared_control+offset,v)}
   for i in 0..blocks{put_u32(&mut raw,layout.block_page_bases+i*8,0x1000+i as u32*8)}
   Self{render_tvb:Some(Tvb{block_count:blocks,logical_size:1,extensions:KVec(Vec::new()),block_addresses:KVec((0..blocks).map(block_address).collect()),fail:false}),
    parameter_management:Some(Pm{backing:Object{raw},layout,hardware_buffer_id:id,page_count:blocks as u32*4}),queue_pair:Pair(slot)}
  }
 }
 struct Resources{pb_descriptor_table:Object}
 #[derive(Debug,Copy,Clone,PartialEq,Eq)]pub struct G17PDeviceControlPublication;
 pub struct G17ResourceOwner{resources:Resources,pub replies:Vec<[u8;64]>,pub credit:bool}
 impl G17ResourceOwner {
  pub fn new()->Self{Self{resources:Resources{pb_descriptor_table:Object{raw:vec![0x5a;0x1000]}},replies:vec![],credit:true}}
  pub fn dev(&self){}
  pub fn publish_primary_device_control_raw(&mut self,record:&[u8;64])->Result<G17PDeviceControlPublication>{
   if !self.credit{return Err(EAGAIN)} self.credit=false;
   self.replies.push(*record);Ok(G17PDeviceControlPublication)
  }
''' + '\n'.join(item('g17_resources.rs', m) for m in [
    '    pub(crate) fn grow_render_tvb_synchronously(',
    '    fn grow_render_tvb_inner(',
]) + '\n}\n' + '\n'.join(item('g17_resources.rs', m) for m in [
    'fn g17p_compact_tvb_page_id(', 'fn g17p_tvb_grow_target(',
    'fn g17p_tvb_auto_grow_target(', 'struct G17PPmControlCursors {',
    'fn g17p_pm_control_cursors(', 'struct G17PPmGrowthPlan {',
    'impl G17PPmGrowthPlan {', 'fn g17p_pm_growth_plan(',
]) + r'''
 #[test]fn tvb_auto_growth_uses_measured_headroom_and_native_cap(){
  assert_eq!(g17p_tvb_auto_grow_target(238,60,G17P_PM_MAX_BLOCKS_NOMEMLESS),Some(179));
  assert_eq!(g17p_tvb_auto_grow_target(120,60,G17P_PM_MAX_BLOCKS_NOMEMLESS),None);
  assert_eq!(g17p_tvb_auto_grow_target(121,60,G17P_PM_MAX_BLOCKS_NOMEMLESS),Some(91));
  assert_eq!(g17p_tvb_auto_grow_target(usize::MAX,60,G17P_PM_MAX_BLOCKS_NOMEMLESS),Some(G17P_PM_MAX_BLOCKS_NOMEMLESS));
 }
 #[test]fn tvb_live_growth_preserves_all_firmware_state_except_new_blocks_and_host_cursors(){
  let mut owner=G17ResourceOwner::new();let mut storage=G17PUserRenderStorage::new(2,32,0);
  let vm=mmu::Vm{blocks:Cell::new(32),fail:Cell::new(false)};
  let before=storage.parameter_management.as_ref().unwrap().backing.raw.clone();
  let desc=owner.resources.pb_descriptor_table.raw.clone();
  assert!(owner.grow_render_tvb_synchronously(&mut storage,&vm).unwrap());
  assert_eq!(storage.tvb_capacity(),42);assert_eq!(vm.blocks.get(),42);
  assert_eq!(storage.render_tvb.as_ref().unwrap().extensions.0,vec![10]);
  let after=&storage.parameter_management.as_ref().unwrap().backing.raw;
  for i in 0..before.len(){
   if !(0x100..0x108).contains(&i)&&!(0x200+32*8..0x200+42*8).contains(&i){assert_eq!(after[i],before[i],"changed byte {i:#x}")}
  }
  assert_eq!(u32::from_le_bytes(after[0x100..0x104].try_into().unwrap()),42);
  assert_eq!(u32::from_le_bytes(after[0x104..0x108].try_into().unwrap()),42);
  for i in 32..42{assert_eq!(u32::from_le_bytes(after[0x200+i*8..0x204+i*8].try_into().unwrap()),0x1000+i as u32*8)}
  assert_eq!(owner.resources.pb_descriptor_table.raw,desc);
 }
 #[test]fn tvb_live_growth_does_not_acquire_a_descriptor_cpu_view(){
  let mut owner=G17ResourceOwner::new();let mut storage=G17PUserRenderStorage::new(2,32,0);
  owner.resources.pb_descriptor_table.raw.clear();
  let vm=mmu::Vm{blocks:Cell::new(32),fail:Cell::new(false)};
  assert_eq!(owner.grow_render_tvb_synchronously(&mut storage,&vm),Ok(true));
  assert_eq!(storage.tvb_capacity(),42);
 }
 #[test]fn tvb_retired_growth_still_rebuilds_page_list_state_and_descriptor(){
  let mut owner=G17ResourceOwner::new();let mut storage=G17PUserRenderStorage::new(2,32,0);
  let vm=mmu::Vm{blocks:Cell::new(32),fail:Cell::new(false)};
  owner.grow_render_tvb_inner(&mut storage,&vm,42,false).unwrap();
  let pm=storage.parameter_management.as_ref().unwrap();
  let word=|offset:usize|u32::from_le_bytes(pm.backing.raw[offset..offset+4].try_into().unwrap());
  assert_eq!(word(0x34),168);assert_eq!(word(0x3c),42);assert_eq!(word(0x40),7);
  assert_eq!(word(pm.layout.page_list),0x1000+7*8);
  let desc=&owner.resources.pb_descriptor_table.raw[32..48];
  assert_eq!(desc.chunks_exact(4).map(|b|u32::from_le_bytes(b.try_into().unwrap())).collect::<Vec<_>>(),vec![0x98760000,168,0,0]);
 }
 #[test]fn tvb_growth_allocation_and_alias_failure_publish_nothing(){
  for fail_backing in [true,false]{
   let mut owner=G17ResourceOwner::new();let mut storage=G17PUserRenderStorage::new(2,32,0);
   storage.render_tvb.as_mut().unwrap().fail=fail_backing;
   let vm=mmu::Vm{blocks:Cell::new(32),fail:Cell::new(!fail_backing)};
   let before=storage.parameter_management.as_ref().unwrap().backing.raw.clone();
   assert_eq!(owner.grow_render_tvb_synchronously(&mut storage,&vm),Err(ENOMEM));
   assert_eq!(storage.tvb_capacity(),32);assert_eq!(vm.blocks.get(),32);
   assert_eq!(storage.parameter_management.as_ref().unwrap().backing.raw,before);
   assert!(storage.render_tvb.as_ref().unwrap().extensions.is_empty());
  }
 }
 #[test]fn tvb_growth_caps_final_increment_and_reports_no_growth_at_limit(){
  for remaining in 0..=12{
   let before=G17P_PM_MAX_OWNED_BLOCKS-remaining;
   assert_eq!(g17p_tvb_grow_target(before,G17P_PM_MAX_OWNED_BLOCKS),before+remaining.min(10));
  }
  assert_eq!(g17p_tvb_grow_target(usize::MAX,usize::MAX),usize::MAX);
  let max=G17P_PM_MAX_OWNED_BLOCKS;
  let mut owner=G17ResourceOwner::new();let mut storage=G17PUserRenderStorage::new(2,max-3,0);
  let vm=mmu::Vm{blocks:Cell::new(max-3),fail:Cell::new(false)};
  assert_eq!(owner.grow_render_tvb_synchronously(&mut storage,&vm),Ok(true));
  assert_eq!(storage.tvb_capacity(),max);
  assert_eq!(owner.grow_render_tvb_synchronously(&mut storage,&vm),Ok(false));
 }
}
struct G17PRenderRootOwner{context:u8,vm:mmu::Vm}
impl G17PRenderRootOwner{fn root_slot(&self)->Result<u8>{Ok(self.context)}fn vm(&self)->&mmu::Vm{&self.vm}}
struct Tracker{hardware_buffer_id:u32}
struct Parked{tracker:Option<Tracker>,queued_tracker:Option<Tracker>,hardware_buffer_id:Option<u32>,storage:Option<g17_resources::G17PUserRenderStorage>,bind:Option<G17PRenderRootOwner>,hardware_buffer_published:bool}
mod g17_initdata{pub const CONTROL_RECORD_SIZE:usize=64;}
#[derive(Debug,Copy,Clone,PartialEq,Eq)]enum G17PTvbGrowDisposition{IgnoredStale,Published,Deferred}
enum G17PPendingPrimaryControlFlush{Empty,Blocked,Published(g17_resources::G17PDeviceControlPublication)}
struct G17PPendingPrimaryControl{record:[u8;64],recovery_generation:u64,worker_attempts:u16}
const G17P_PRIMARY_CONTROL_RETRY_LIMIT:u16=1000;
struct Manager{resources:g17_resources::G17ResourceOwner,accepted_firmware_recovery_generation:u64,pending_primary_control:Option<G17PPendingPrimaryControl>,render_tracker:Option<Tracker>,render_queued_tracker:Option<Tracker>,render_hardware_buffer_id:Option<u32>,render_storage:Option<g17_resources::G17PUserRenderStorage>,render_bind:Option<G17PRenderRootOwner>,render_hardware_buffer_published:bool,parked_render_slots:[Option<Box<Parked>>;2]}
impl Manager{
 fn new()->Self{Self{resources:g17_resources::G17ResourceOwner::new(),accepted_firmware_recovery_generation:9,
 pending_primary_control:None,render_tracker:Some(Tracker{hardware_buffer_id:2}),render_queued_tracker:None,render_hardware_buffer_id:Some(2),render_storage:Some(g17_resources::G17PUserRenderStorage::new(2,32,0)),render_bind:Some(G17PRenderRootOwner{context:10,vm:mmu::Vm{blocks:Cell::new(32),fail:Cell::new(false)}}),render_hardware_buffer_published:true,parked_render_slots:[None,Some(Box::new(Parked{tracker:Some(Tracker{hardware_buffer_id:3}),queued_tracker:None,hardware_buffer_id:Some(3),storage:Some(g17_resources::G17PUserRenderStorage::new(3,32,1)),bind:Some(G17PRenderRootOwner{context:11,vm:mmu::Vm{blocks:Cell::new(32),fail:Cell::new(false)}}),hardware_buffer_published:true}))]}}
 fn pending_render_usc_freelist_release(&self)->Option<()>{None}
''' + item('g17_manager.rs', '    pub(crate) fn primary_device_control_backpressured(') + item('g17_manager.rs', '    pub(crate) fn flush_pending_primary_device_control(') + item('g17_manager.rs', '    pub(crate) fn note_primary_device_control_backpressure(') + item('g17_manager.rs', '    pub(crate) fn service_render_tvb_grow(') + r'''
}
fn request()->g17_completion::G17PFirmwareTvbGrowEvent{g17_completion::G17PFirmwareTvbGrowEvent{vm_slot:10,buffer_slot:2,counter:0x12345678,subpipe:63,halt_count:9}}
#[test]fn tvb_raw_failure_event_decodes_and_replies_with_exact_native_bytes(){
 let mut raw=[0u8;72];for (i,w) in [0xa00000006u64,2,0,0,0,0,0,1,1].into_iter().enumerate(){raw[i*8..i*8+8].copy_from_slice(&w.to_le_bytes())}
 let r=g17_completion::G17PFirmwareTvbGrowEvent::decode(&raw).unwrap();
 assert_eq!((r.vm_slot,r.buffer_slot,r.counter,r.subpipe,r.halt_count),(10,2,0,0,0));
 for grew in [false,true]{let reply=r.reply(grew);let words:Vec<u64>=reply.chunks_exact(8).map(|b|u64::from_le_bytes(b.try_into().unwrap())).collect();assert_eq!(words,vec![8|((grew as u64)<<32),0xa00000002,0,0,0,0,0,0])}
}
#[test]fn tvb_packed_generation_and_nonzero_fields_survive_poisoned_tail(){
 let r=request();let mut raw=[0xa5;72];raw[..4].copy_from_slice(&6u32.to_le_bytes());
 for (off,v) in [(4,u32::MAX),(8,2),(12,r.counter),(16,r.subpipe)]{raw[off..off+4].copy_from_slice(&v.to_le_bytes())}
 let generation=0x8877665544332211u64;raw[20..28].copy_from_slice(&generation.to_le_bytes());
 let decoded=g17_completion::G17PFirmwareTvbGrowEvent::decode(&raw).unwrap();let reply=decoded.reply(true);
 assert_eq!(u64::from_le_bytes(reply[24..32].try_into().unwrap()),generation);
 assert_eq!(&reply[12..24],&[255,255,255,255,0x78,0x56,0x34,0x12,63,0,0,0]);
 assert!(reply[32..].iter().all(|v|*v==0));assert!(decoded.matches_owner(generation,2,55));
 for (off,value) in [(0,7u32),(4,0xfffffffe),(8,127),(16,64)]{let mut bad=raw;bad[off..off+4].copy_from_slice(&value.to_le_bytes());assert!(g17_completion::G17PFirmwareTvbGrowEvent::decode(&bad).is_err())}
 for size in [0,20,28,64,71]{assert!(g17_completion::G17PFirmwareTvbGrowEvent::decode(&raw[..size]).is_err())}
}
#[test]fn tvb_manager_routes_both_slots_and_preserves_request_identity(){
 for (id,context) in [(2,10),(3,11)]{let mut m=Manager::new();let r=g17_completion::G17PFirmwareTvbGrowEvent{buffer_slot:id,vm_slot:context,..request()};assert_eq!(m.service_render_tvb_grow(r),Ok(G17PTvbGrowDisposition::Published));assert_eq!(m.resources.replies,vec![r.reply(true)]);assert_eq!(m.render_hardware_buffer_id,Some(2));assert_eq!(m.render_storage.as_ref().unwrap().tvb_capacity(),if id==2{42}else{32});assert_eq!(m.parked_render_slots[1].as_ref().unwrap().storage.as_ref().unwrap().tvb_capacity(),if id==3{42}else{32})}
}
#[test]fn tvb_stale_generation_never_grows_or_publishes(){
 for halt in [0,8,10,u64::MAX]{let mut m=Manager::new();assert_eq!(m.service_render_tvb_grow(g17_completion::G17PFirmwareTvbGrowEvent{halt_count:halt,..request()}),Ok(G17PTvbGrowDisposition::IgnoredStale));assert!(m.resources.replies.is_empty());assert_eq!(m.render_storage.as_ref().unwrap().tvb_capacity(),32)}
}
#[test]fn tvb_wrong_owner_and_allocation_failure_reply_false(){
 for failure in 0..4{let mut m=Manager::new();let mut r=request();match failure{0=>r.buffer_slot=126,1=>r.vm_slot=11,2=>{m.render_tracker=None;m.render_hardware_buffer_published=false},_=>m.render_bind.as_ref().unwrap().vm.fail.set(true)};assert_eq!(m.service_render_tvb_grow(r),Ok(G17PTvbGrowDisposition::Published));assert_eq!(m.resources.replies,vec![r.reply(false)]);assert_eq!(m.render_storage.as_ref().unwrap().tvb_capacity(),32)}
}
#[test]fn tvb_full_control_ring_defers_exact_encoded_reply_without_replaying_growth(){
 let mut m=Manager::new();let r=request();m.resources.credit=false;
 assert_eq!(m.service_render_tvb_grow(r),Ok(G17PTvbGrowDisposition::Deferred));
 assert!(m.primary_device_control_backpressured());assert!(m.resources.replies.is_empty());
 assert_eq!(m.render_storage.as_ref().unwrap().tvb_capacity(),42);
 let next=g17_completion::G17PFirmwareTvbGrowEvent{counter:r.counter+1,..r};
 assert_eq!(m.service_render_tvb_grow(next),Err(EBUSY));
 assert_eq!(m.render_storage.as_ref().unwrap().tvb_capacity(),42);
 for _ in 0..4{assert!(matches!(m.flush_pending_primary_device_control(),Ok(G17PPendingPrimaryControlFlush::Blocked)));m.note_primary_device_control_backpressure().unwrap();}
 assert_eq!(m.render_storage.as_ref().unwrap().tvb_capacity(),42);
 m.resources.credit=true;
 assert!(matches!(m.flush_pending_primary_device_control(),Ok(G17PPendingPrimaryControlFlush::Published(_))));
 assert!(!m.primary_device_control_backpressured());assert_eq!(m.resources.replies,vec![r.reply(true)]);
 assert!(matches!(m.flush_pending_primary_device_control(),Ok(G17PPendingPrimaryControlFlush::Empty)));
 assert_eq!(m.resources.replies.len(),1);
}
#[test]fn tvb_deferred_reply_rejects_cross_recovery_replay(){
 let mut m=Manager::new();m.resources.credit=false;
 assert_eq!(m.service_render_tvb_grow(request()),Ok(G17PTvbGrowDisposition::Deferred));
 m.accepted_firmware_recovery_generation+=1;m.resources.credit=true;
 assert!(matches!(m.flush_pending_primary_device_control(),Err(EIO)));
 assert!(m.primary_device_control_backpressured());assert!(m.resources.replies.is_empty());
}
#[test]fn two_tvb_events_remain_ordered_across_one_deferred_reply(){
 let mut m=Manager::new();let first=request();m.resources.credit=false;
 assert_eq!(m.service_render_tvb_grow(first),Ok(G17PTvbGrowDisposition::Deferred));
 m.resources.credit=true;assert!(matches!(m.flush_pending_primary_device_control(),Ok(G17PPendingPrimaryControlFlush::Published(_))));
 let second=g17_completion::G17PFirmwareTvbGrowEvent{counter:first.counter+1,..first};m.resources.credit=true;
 assert_eq!(m.service_render_tvb_grow(second),Ok(G17PTvbGrowDisposition::Published));
 assert_eq!(m.resources.replies,vec![first.reply(true),second.reply(true)]);
 assert_eq!(m.render_storage.as_ref().unwrap().tvb_capacity(),52);
}
#[test]fn deferred_reply_worker_budget_is_bounded(){
 let mut m=Manager::new();m.resources.credit=false;
 assert_eq!(m.service_render_tvb_grow(request()),Ok(G17PTvbGrowDisposition::Deferred));
 for _ in 0..G17P_PRIMARY_CONTROL_RETRY_LIMIT{assert_eq!(m.note_primary_device_control_backpressure(),Ok(()));}
 assert_eq!(m.note_primary_device_control_backpressure(),Err(ETIMEDOUT));
 assert!(m.primary_device_control_backpressured());
}
'''
with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / 'test.rs'
    binary = Path(directory) / 'test'
    source.write_text(code)
    subprocess.run(['rustc', '--edition=2021', '--test', str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary), '--test-threads=1'], check=True)
print('PASS production event-6 codec, owner dispatch, live TVB publication, and failure semantics')
