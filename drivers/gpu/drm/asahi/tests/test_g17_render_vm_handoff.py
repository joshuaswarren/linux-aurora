#!/usr/bin/env python3
"""Compile actual idle render handoff with root/lease/cache failure injection."""
from pathlib import Path
import subprocess, tempfile
p=Path(__file__).resolve().parents[1]
s=(p/'g17_manager.rs').read_text()
def method(marker):
    a=s.index(marker); b=s.index('{',a); d=1; e=b+1
    while d:
        d+=(s[e]=='{')-(s[e]=='}');e+=1
    return s[a:e]
methods='\n'.join(method(m) for m in ['pub(crate) fn retained_render_has_other_owner(', 'pub(crate) fn handoff_retained_render_vm(', 'fn require_retired_render_owner(', 'pub(crate) fn release_retired_render_context('])
source=r'''
#![allow(dead_code,unused_variables)]
use std::cell::RefCell;
use std::rc::Rc;
use std::collections::BTreeSet;
type Result<T=()> = core::result::Result<T,i32>;
const EBUSY:i32=16; const EIO:i32=5; const ENODEV:i32=19;
macro_rules! dev_info { ($($tokens:tt)*) => {}; }
#[derive(Clone)] struct Vm { id:u64 }
mod mmu { pub(super) type Vm=super::Vm; }
#[derive(Clone,Copy,PartialEq,Debug)] enum Op { Cache,Aliases,Bind,App }
#[derive(Default)] struct State { root:u64, refs:u32, cache:BTreeSet<u64>, events:Vec<String>, failures:Vec<(Op,u64)> }
#[derive(Clone)] struct Resources(Rc<RefCell<State>>);
struct Binding { vm:Vm, shared:Rc<RefCell<State>> }
impl Binding { fn vm(&self)->&Vm{&self.vm} fn root(&self)->u64{self.vm.id*0x10000}
 fn vm_id(&self)->u64{self.vm.id} fn matches(&self,v:&Vm)->bool{self.vm.id==v.id} }
impl Clone for Binding {fn clone(&self)->Self {self.shared.borrow_mut().refs+=1;Self{vm:self.vm.clone(),shared:self.shared.clone()}}}
impl Drop for Binding {fn drop(&mut self) {let mut s=self.shared.borrow_mut();s.refs-=1;s.events.push(format!("release{}",self.vm.id));}}
struct Context { _bind:Binding }
struct Aliases { id:u64 }
impl Aliases {fn matches(&self,vm:&Vm)->bool{self.id==vm.id}}
#[derive(Clone,Copy)]struct Queue { idle:bool }
impl Queue {fn idle(&self)->bool{self.idle}}
#[derive(Clone,Copy)]struct Snapshot {job_list_empty:bool,tiling:Queue,fragment:Queue}
struct Storage {_command_context:Option<Context>,snapshot:Snapshot}
impl Storage {fn snapshot(&mut self)->Result<Snapshot>{Ok(self.snapshot)}
 fn release_client_context(&mut self){self._command_context=None;}
 fn install_client_context(&mut self,c:Context){self._command_context=Some(c);}}
#[derive(PartialEq)]enum G17PBootstrapUmaRequestState {Completed,Ready}
struct Uma {state:G17PBootstrapUmaRequestState,hardware_buffer_owned:bool,hardware_buffer_references:u32,pending_release_count:u32}
impl Resources {
 fn operation(&self,op:Op,id:u64)->Result {
  let mut s=self.0.borrow_mut();s.events.push(format!("{:?}{}",op,id));
  if let Some(i)=s.failures.iter().position(|x|*x==(op,id)){s.failures.remove(i);return Err(12)} Ok(())
 }
 fn uat(&self)->&Self{self}
 fn partial_opening_outer_counters(&self)->Result<[[u32;3];2]>{Ok([[1,1,1];2])}
 fn cache_render_vm_mappings(&mut self,_storage:&mut Storage,vm:&Vm)->Result {
  if self.0.borrow().cache.contains(&vm.id){return Ok(())}
  self.operation(Op::Cache,vm.id)?;self.0.borrow_mut().cache.insert(vm.id);Ok(())
 }
 fn install_t8140_native_context_aliases(&self,vm:&Vm)->Result<Aliases>{
  self.operation(Op::Aliases,vm.id)?;Ok(Aliases{id:vm.id})
 }
 fn bind(&self,vm:&Vm)->Result<Binding>{
  self.operation(Op::Bind,vm.id)?;
  let mut s=self.0.borrow_mut();if s.refs!=0{return Err(EBUSY)}
  s.root=vm.id*0x10000;s.refs=1;s.events.push(format!("tlbi1-root{}",vm.id));
  Ok(Binding{vm:vm.clone(),shared:self.0.clone()})
 }
 fn allocate_t8140_app_context(&self,b:&Binding)->Result<Context>{
  self.operation(Op::App,b.vm.id)?;Ok(Context{_bind:b.clone()})
 }
}
struct Manager {
 resources:Resources,render_storage:Option<Storage>,render_bind:Option<Binding>,
 render_context_aliases:Option<Aliases>,render_tracker:Option<u32>,
 render_hardware_buffer_id:Option<u32>,render_hardware_buffer_published:bool,
 prepared_render_flist:Option<u32>,render_submission_ordinal:u32,bootstrap_uma:Option<Uma>,
}
impl Manager {fn new()->Self{
 let resources=Resources(Rc::new(RefCell::new(State::default())));let vm=Vm{id:1};
 let binding=resources.bind(&vm).unwrap();let context=resources.allocate_t8140_app_context(&binding).unwrap();
 resources.0.borrow_mut().cache.insert(1);resources.0.borrow_mut().events.clear();
 Self{resources,render_storage:Some(Storage{_command_context:Some(context),snapshot:Snapshot{job_list_empty:true,tiling:Queue{idle:true},fragment:Queue{idle:true}}}),
 render_bind:Some(binding),render_context_aliases:Some(Aliases{id:1}),render_tracker:None,
 render_hardware_buffer_id:None,render_hardware_buffer_published:false,prepared_render_flist:None,
 render_submission_ordinal:3,bootstrap_uma:Some(Uma{state:G17PBootstrapUmaRequestState::Completed,
 hardware_buffer_owned:false,hardware_buffer_references:0,pending_release_count:0})}}
 fn expect_owner(&self,id:u64) {assert!(self.render_bind.as_ref().unwrap().matches(&Vm{id}));
 assert!(self.render_context_aliases.as_ref().unwrap().matches(&Vm{id}));
 assert_eq!(self.resources.0.borrow().root,id*0x10000);assert_eq!(self.resources.0.borrow().refs,2);
 assert_eq!(self.render_submission_ordinal,3);assert!(self.render_tracker.is_none());}
}
'''
source+='impl Manager {\n'+methods+'\n}\n'
source+=r'''
#[test]fn successful_a_b_a_keeps_cached_aliases_and_distinct_roots(){
 let mut m=Manager::new();let a=Vm{id:1};let b=Vm{id:2};
 m.handoff_retained_render_vm(&a).unwrap();assert!(m.resources.0.borrow().events.is_empty());
 m.handoff_retained_render_vm(&b).unwrap();m.expect_owner(2);
 m.handoff_retained_render_vm(&a).unwrap();m.expect_owner(1);
 m.handoff_retained_render_vm(&b).unwrap();m.expect_owner(2);
 let s=m.resources.0.borrow();assert_eq!(s.events.iter().filter(|x|x.as_str()=="Cache2").count(),1);
 assert_eq!(s.events.iter().filter(|x|x.as_str()=="tlbi1-root2").count(),2);
}
#[test]fn preparation_and_each_root_switch_failure_restore_a(){
 for op in [Op::Cache,Op::Aliases,Op::Bind,Op::App] {
  let mut m=Manager::new();m.resources.0.borrow_mut().failures.push((op,2));
  assert_eq!(m.handoff_retained_render_vm(&Vm{id:2}),Err(12));m.expect_owner(1);
  m.handoff_retained_render_vm(&Vm{id:2}).unwrap();m.expect_owner(2);
 }
}
#[test]fn rollback_failure_leaves_retired_graph_attachable(){
 for rollback in [Op::Aliases,Op::Bind,Op::App] {
  let mut m=Manager::new();m.resources.0.borrow_mut().failures=vec![(Op::App,2),(rollback,1)];
  assert_eq!(m.handoff_retained_render_vm(&Vm{id:2}),Err(12));
  assert!(m.render_bind.is_none());assert_eq!(m.resources.0.borrow().refs,0);
  m.handoff_retained_render_vm(&Vm{id:3}).unwrap();m.expect_owner(3);
 }
}
#[test]fn incomplete_retirement_blocks_every_mapping_and_root_change(){
 for cause in 0..9 {
  let mut m=Manager::new();match cause {
   0=>m.render_tracker=Some(1),1=>m.render_hardware_buffer_id=Some(1),
   2=>m.render_hardware_buffer_published=true,3=>m.prepared_render_flist=Some(1),
   4=>m.bootstrap_uma.as_mut().unwrap().state=G17PBootstrapUmaRequestState::Ready,
   5=>m.bootstrap_uma.as_mut().unwrap().hardware_buffer_references=1,
   6=>m.render_storage.as_mut().unwrap().snapshot.tiling.idle=false,
   7=>m.render_storage.as_mut().unwrap().snapshot.fragment.idle=false,
   _=>m.render_storage.as_mut().unwrap().snapshot.job_list_empty=false,
  }
  assert_eq!(m.handoff_retained_render_vm(&Vm{id:2}),Err(EBUSY));
  assert!(m.resources.0.borrow().events.is_empty());assert_eq!(m.resources.0.borrow().root,0x10000);
 }
}
#[test]fn render_compute_render_retains_cache_and_queue_ordinal(){
 let mut m=Manager::new();let ordinal=m.render_submission_ordinal;
 m.release_retired_render_context().unwrap();
 assert!(m.render_bind.is_none());assert!(m.render_context_aliases.is_none());
 assert_eq!(m.resources.0.borrow().refs,0);
 assert!(m.resources.0.borrow().cache.contains(&1));
 let compute=m.resources.bind(&Vm{id:2}).unwrap();
 assert_eq!(compute.root(),0x20000);drop(compute);
 m.handoff_retained_render_vm(&Vm{id:1}).unwrap();m.expect_owner(1);
 assert_eq!(m.render_submission_ordinal,ordinal);
 assert_eq!(m.resources.0.borrow().events.iter().filter(|x|x.as_str()=="Cache1").count(),0);
}
#[test]fn unfinished_render_cannot_release_context_to_compute(){
 let mut m=Manager::new();m.render_storage.as_mut().unwrap().snapshot.fragment.idle=false;
 assert_eq!(m.release_retired_render_context(),Err(EBUSY));m.expect_owner(1);
 assert!(m.resources.0.borrow().events.is_empty());
}
'''
with tempfile.TemporaryDirectory() as d:
    f=Path(d)/'test.rs'; f.write_text(source);exe=Path(d)/'test'
    subprocess.run(['rustc','--edition=2021','--test',str(f),'-o',str(exe)],check=True)
    subprocess.run([str(exe),'--nocapture'],check=True)
print('PASS actual handoff: A/B/A cached roots, all allocation/root/lease failures, failed rollback retry, and mandatory retirement gates')
