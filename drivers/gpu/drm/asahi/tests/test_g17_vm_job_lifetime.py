#!/usr/bin/env python3
"""Exercise unchanged job/close ownership; model only GPUVM map actuators."""
from pathlib import Path
import subprocess,tempfile
s=(Path(__file__).resolve().parents[1]/'mmu.rs').read_text()
def item(needle):
 a=s.index(needle);start=s.index('{',a);depth=1;i=start+1
 while depth:depth+=(s[i]=='{')-(s[i]=='}');i+=1
 return s[a:i]
state=item('struct T8140VmJobLifetime {')
guard=item('pub(crate) struct T8140VmJobGuard {')+'\n'+item('impl Drop for T8140VmJobGuard {')
retain=item('    pub(crate) fn retain_t8140_job(')
unmap=item('    pub(crate) fn unmap_user_ranges(');unmap=unmap[:unmap.index('        let retained = self.driver_mappings.lock();')]+'''        self.unmaps.fetch_add(1, Ordering::SeqCst); Ok(())\n    }'''
drop=item('    pub(crate) fn drop_mappings(');drop=drop[:drop.index('        // Removing')]+'''        self.object_unmaps.fetch_add(1, Ordering::SeqCst); Ok(())\n    }'''
source=r'''
#![allow(dead_code,unused_variables)]
use std::ops::{Deref,Range};use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
type Error=i32;type Result<T=()>=std::result::Result<T,Error>;const ENOENT:i32=2;const EOVERFLOW:i32=75;const GFP_KERNEL:u32=0;
macro_rules! pr_err{($($t:tt)*)=>{()}}
struct KVec<T>(Vec<T>);impl<T>KVec<T>{fn new()->Self{Self(vec![])}fn push(&mut self,v:T,_f:u32)->Result{self.0.push(v);Ok(())}}
impl<T>Deref for KVec<T>{type Target=Vec<T>;fn deref(&self)->&Vec<T>{&self.0}}
impl<T>IntoIterator for KVec<T>{type Item=T;type IntoIter=std::vec::IntoIter<T>;fn into_iter(self)->Self::IntoIter{self.0.into_iter()}}
struct Mutex<T>(std::sync::Mutex<T>);impl<T>Mutex<T>{fn lock(&self)->std::sync::MutexGuard<'_,T>{self.0.lock().unwrap()}}
mod gem{#[derive(Clone)]pub struct Object(pub u64);}
struct ARef<T>(Arc<T>);impl<T>Deref for ARef<T>{type Target=T;fn deref(&self)->&T{&self.0}}
impl From<&gem::Object> for ARef<gem::Object>{fn from(v:&gem::Object)->Self{Self(Arc::new(v.clone()))}}
#[derive(Clone)]struct Vm{job_lifetime:Arc<Mutex<T8140VmJobLifetime>>,unmaps:Arc<AtomicUsize>,object_unmaps:Arc<AtomicUsize>}
'''+state+'\n'+guard+'\nimpl Vm {\n'+retain+'\n'+unmap+'\n'+drop+r'''
 fn bo_deferred_cleanup(&self){}
 fn new()->Self{Self{job_lifetime:Arc::new(Mutex(std::sync::Mutex::new(T8140VmJobLifetime{active:0,closed:false,close_ranges:None,closed_objects:KVec::new()}))),unmaps:Arc::new(AtomicUsize::new(0)),object_unmaps:Arc::new(AtomicUsize::new(0))}}
}
#[test]fn file_and_object_close_preserve_two_outstanding_jobs(){
 let v=Vm::new();let a=v.retain_t8140_job().unwrap();let b=v.retain_t8140_job().unwrap();v.drop_mappings(&gem::Object(7)).unwrap();v.unmap_user_ranges(0..100,80..90).unwrap();assert!(v.retain_t8140_job().is_err());assert_eq!(v.unmaps.load(Ordering::SeqCst),0);assert_eq!(v.object_unmaps.load(Ordering::SeqCst),0);
 drop(b);assert_eq!(v.unmaps.load(Ordering::SeqCst),0);assert_eq!(v.object_unmaps.load(Ordering::SeqCst),0);drop(a);assert_eq!(v.unmaps.load(Ordering::SeqCst),1);assert_eq!(v.object_unmaps.load(Ordering::SeqCst),1);
}
#[test]fn open_vm_retirement_allows_next_job_and_quarantine_pins_mapping(){
 let v=Vm::new();let a=v.retain_t8140_job().unwrap();v.drop_mappings(&gem::Object(9)).unwrap();drop(a);assert_eq!(v.object_unmaps.load(Ordering::SeqCst),1);let quarantine=v.retain_t8140_job().unwrap();v.unmap_user_ranges(0..100,80..90).unwrap();drop(v.clone());assert_eq!(v.unmaps.load(Ordering::SeqCst),0);drop(quarantine);assert_eq!(v.unmaps.load(Ordering::SeqCst),1);
}
#[test]fn overflow_and_idle_close_do_not_create_unowned_job(){let v=Vm::new();v.job_lifetime.lock().active=usize::MAX;assert!(v.retain_t8140_job().is_err());v.job_lifetime.lock().active=0;v.unmap_user_ranges(0..100,80..90).unwrap();assert_eq!(v.unmaps.load(Ordering::SeqCst),1);assert!(v.retain_t8140_job().is_err());}
'''
with tempfile.TemporaryDirectory() as d:
 f=Path(d)/'test.rs';f.write_text(source);exe=Path(d)/'test'
 subprocess.run(['rustc','--edition=2021','--test',str(f),'-o',str(exe)],check=True)
 subprocess.run([str(exe),'--test-threads=1'],check=True)
print('PASS production VM job/close ownership with modeled GPUVM actuators: deferred file/GEM close, multiple jobs, quarantine, idle cleanup, overflow')
