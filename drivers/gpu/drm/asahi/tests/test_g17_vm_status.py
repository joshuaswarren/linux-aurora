#!/usr/bin/env python3
"""Actual status, ioctl handler and completion code with CPU/fence actuators."""
from pathlib import Path
import subprocess, tempfile
p=Path(__file__).resolve().parents[1]
def item(text,needle):
 start=text.index(needle);op=text.index('{',start);depth=1;i=op+1
 while depth:
  depth+=(text[i]=='{')-(text[i]=='}');i+=1
 return text[start:i]
status=(p/'g17_status.rs').read_text().replace('//!','//')
job=(p/'g17_job.rs').read_text()
completion='\n'.join(item(job,n) for n in ['pub(crate) struct TimestampDestination','pub(crate) struct G17PJobCompletion','impl G17PJobCompletion'])
query=item((p/'file.rs').read_text(),'    fn get_vm_status(')
mock=r'''
#![allow(dead_code,unused_imports)]
use std::sync::{Arc,Mutex};use std::collections::BTreeMap;
type Result<T=()> = std::result::Result<T,i32>;
const EINVAL:i32=-22;const EOPNOTSUPP:i32=-95;const ENOENT:i32=-2;
trait Errno{fn to_errno(&self)->i32;}impl Errno for i32{fn to_errno(&self)->i32{*self}}
mod g17_drm {pub struct G17PSubmissionFence;}
mod mmu {
 pub struct KernelMapping {pub fail:bool}
 impl KernelMapping {pub fn iova(&self)->u64{0}pub fn with_cpu_bytes<T>(&self,f:impl FnOnce(&mut[u8])->super::Result<T>)->super::Result<T>{if self.fail{return Err(-14)}f(&mut [0;16])}}
}
struct UserFence<T>{status:Arc<g17_status::VmStatus>,expected:i32,done:std::sync::atomic::AtomicBool,_tag:std::marker::PhantomData<T>}
impl<T>UserFence<T>{fn set_error(&self,error:i32){assert_eq!(self.status.get(),error)}fn signal(&self){assert_eq!(self.status.get(),self.expected);self.done.store(true,std::sync::atomic::Ordering::Release)}}
struct Fence;impl Fence{fn from_fence<T>(_:&UserFence<T>)->Self{Self}}
mod uapi {pub struct drm_asahi_get_params{pub pointer:u64,pub size:u64,pub pad:u32}}
#[derive(Copy,Clone)]struct UserPtr(usize);impl UserPtr{fn from_addr(n:usize)->Self{Self(n)}}
struct UserSlice(UserPtr,usize);impl UserSlice{fn new(p:UserPtr,n:usize)->Self{Self(p,n)}fn reader(self)->Self{self}fn writer(self)->Self{self}fn read_slice(&self,b:&mut[u8])->Result{unsafe{b.copy_from_slice(std::slice::from_raw_parts(self.0.0 as *const u8,self.1))}Ok(())}fn write_slice(&self,b:&[u8])->Result{unsafe{std::slice::from_raw_parts_mut(self.0.0 as *mut u8,self.1).copy_from_slice(b)}Ok(())}}
struct TestVm(Arc<g17_status::VmStatus>);impl TestVm{fn status(&self)->&Arc<g17_status::VmStatus>{&self.0}}
struct FileVm{vm:TestVm}struct Vms(Mutex<BTreeMap<usize,FileVm>>);impl Vms{fn lock(&self)->VmsGuard<'_>{VmsGuard(self.0.lock().unwrap())}}
struct VmsGuard<'a>(std::sync::MutexGuard<'a,BTreeMap<usize,FileVm>>);impl VmsGuard<'_>{fn get(&self,id:usize)->Option<&FileVm>{self.0.get(&id)}}
struct DrmFile{vms:Vms}impl DrmFile{fn inner(&self)->&Self{self}fn vms(&self)->&Vms{&self.vms}}
struct Gpu{supported:bool,error:i32}impl Gpu{fn supports_vm_status(&self)->bool{self.supported}fn submission_error(&self)->i32{self.error}}
struct AsahiDevice{gpu:Gpu}
'''
tests=r'''
#[cfg(test)]mod tests{
 use super::*;
 fn file()->DrmFile{DrmFile{vms:Vms(Mutex::new([(7,FileVm{vm:TestVm(Arc::new(g17_status::VmStatus::new()))})].into()))}}
 fn query(f:&DrmFile,vm:u32,error:i32,flags:u32,pad:u32,size:u64)->(Result<u32>,[u8;16]){let mut b=g17_status::encode_response(vm,0);b[4..8].copy_from_slice(&flags.to_ne_bytes());b[12..].copy_from_slice(&pad.to_ne_bytes());let d=uapi::drm_asahi_get_params{pointer:b.as_mut_ptr() as u64,size,pad:0};let result=get_vm_status(&AsahiDevice{gpu:Gpu{supported:true,error}},&d,f);(result,b)}
 #[test]fn first_failure_sticks_and_vms_are_isolated(){let a=g17_status::VmStatus::new();let b=g17_status::VmStatus::new();a.record(-110);a.record(-5);a.record(0);assert_eq!(a.get(),-110);assert_eq!(b.get(),0);}
 #[test]fn query_enforces_file_vm_ownership_and_reserved_fields(){let f=file();assert_eq!(query(&f,7,0,0,0,16).0,Ok(0));assert_eq!(query(&f,8,0,0,0,16).0,Err(ENOENT));assert_eq!(query(&f,7,0,1,0,16).0,Err(EINVAL));assert_eq!(query(&f,7,0,0,1,16).0,Err(EINVAL));assert_eq!(query(&f,7,0,0,0,15).0,Err(EINVAL));assert_eq!(query(&f,7,0,0,0,32).0,Ok(0));}
 #[test]fn query_preserves_domain_failure_after_recovery(){let f=file();assert_eq!(query(&f,7,-5,0,0,16).1,g17_status::encode_response(7,-5));assert_eq!(query(&f,7,0,0,0,16).1,g17_status::encode_response(7,-5));assert_eq!(query(&file(),7,0,0,0,16).1,g17_status::encode_response(7,0));}
 fn completion(expected:i32,fail_copy:bool)->G17PJobCompletion{let status=Arc::new(g17_status::VmStatus::new());G17PJobCompletion{status:status.clone(),fence:UserFence{status,expected,done:false.into(),_tag:std::marker::PhantomData},timestamps:mmu::KernelMapping{fail:false},destinations:[Some(TimestampDestination{mapping:Arc::new(mmu::KernelMapping{fail:fail_copy}),offset:0}),None]}}
 #[test]fn error_is_visible_before_error_fence_and_timestamp_failure(){let a=completion(-110,false);a.complete(Err(-110));assert!(a.fence.done.load(std::sync::atomic::Ordering::Acquire));let b=completion(-14,true);b.complete(Ok([1,2]));assert_eq!(b.status.get(),-14);let c=completion(0,false);c.complete(Ok([1,2]));assert_eq!(c.status.get(),0);}
 #[test]fn concurrent_failures_publish_one_first_error(){let s=Arc::new(g17_status::VmStatus::new());let threads:Vec<_>=(1..17).map(|n|{let s=s.clone();std::thread::spawn(move||s.record(-n))}).collect();for t in threads{t.join().unwrap()}let first=s.get();assert!((-16..=-1).contains(&first));for n in 1..17{s.record(-n);assert_eq!(s.get(),first)}}
}
'''
with tempfile.TemporaryDirectory() as d:
 f=Path(d)/'status.rs';f.write_text(mock+'\nmod g17_status{'+status+'}\n'+completion+'\n'+query+'\n'+tests)
 subprocess.run(['rustc','--edition=2021','--test',str(f),'-o',str(Path(d)/'test')],check=True)
 subprocess.run([str(Path(d)/'test'),'--nocapture'],check=True)
