#!/usr/bin/env python3
"""Execute actual eager queue constructor/drop with faulting allocators."""
from pathlib import Path
import subprocess,tempfile
p=Path(__file__).resolve().parents[1];s=(p/'g17_drm.rs').read_text()
def block(text,needle,start=0):
 a=text.index(needle,start);b=text.index('{',a);i=b+1;d=1
 while d:d+=(text[i]=='{')-(text[i]=='}');i+=1
 return text[a:i]
constructor=block(s,'    fn new(\n',s.index('impl G17PQueue {'))
production='\n'.join(block(s,n) for n in ['struct G17PQueue {','impl Drop for G17PQueue {','pub(crate) struct G17PAsyncQueueBackend {','impl Drop for G17PAsyncQueueBackend {'])
limits=(p/'g17_queue_limits.rs').read_text().replace('//!','//')
reserve=block(s,'    fn reserve_queue(');release=block(s,'    fn release_queue(')
mock=r'''
#![allow(dead_code,unused_imports)]
use std::sync::{Mutex,MutexGuard};use std::sync::atomic::{AtomicBool,AtomicU32,Ordering};use std::ops::{Deref,Range};
type Result<T=()> = std::result::Result<T,i32>;const EINVAL:i32=22;const ENOSPC:i32=28;const GFP_KERNEL:u32=0;
static FAIL_PREPARE:AtomicBool=AtomicBool::new(false);static FAIL_BACKEND:AtomicBool=AtomicBool::new(false);static FAIL_SCHEDULER:AtomicBool=AtomicBool::new(false);static FAIL_FENCE:AtomicBool=AtomicBool::new(false);
struct Arc<T>(std::sync::Arc<T>);impl<T>Arc<T>{fn new(v:T,_flags:u32)->Result<Self>{if std::any::type_name::<T>().ends_with("G17PAsyncQueueBackend")&&FAIL_BACKEND.swap(false,Ordering::SeqCst){drop(v);Err(12)}else{Ok(Self(std::sync::Arc::new(v)))}}}impl<T>Clone for Arc<T>{fn clone(&self)->Self{Self(self.0.clone())}}impl<T>Deref for Arc<T>{type Target=T;fn deref(&self)->&T{&self.0}}
struct G17PQueueRegistry{live_queues:AtomicU32}
#[derive(Clone)]struct AsahiDevice;type AsahiDevRef=AsahiDevice;impl From<&AsahiDevice> for AsahiDevice{fn from(d:&AsahiDevice)->Self{d.clone()}}
mod mmu{#[derive(Clone)]pub struct Vm;pub struct T8140ComputeExecutionContext;}
struct FenceContexts;impl FenceContexts{fn new(_n:u32,_name:&str,_key:u32)->Result<Self>{if FAIL_FENCE.swap(false,Ordering::SeqCst){Err(12)}else{Ok(Self)}}}
const QUEUE_NAME:&str="queue";const QUEUE_CLASS_KEY:u32=0;const T8140_USER_START:u64=0x4000;const T8140_UNKNOWN_PAGE:u64=1<<42;
mod g17_uapi{pub struct QueueUscWindow{pub base:u64,pub user_start:u64,pub user_end:u64}impl QueueUscWindow{pub fn validate(&self)->Result<(),()>{Ok(())}}}
struct State{owners:Vec<u64>,released:Vec<u64>}
#[derive(Clone,Copy)]struct G17PRuntimeRef(&'static Mutex<State>);
impl G17PRuntimeRef{fn new()->Self{Self(Box::leak(Box::new(Mutex::new(State{owners:vec![],released:vec![]}))))}fn mutex(&self)->&Self{self}fn lock(&self)->Guard<'_>{Guard(self.0.lock().unwrap())}fn release_compute_owner(&self,_id:u64){}}
struct Guard<'a>(MutexGuard<'a,State>);impl Guard<'_>{fn prepare_async_compute_queue(&mut self,id:u64,_vm:&mmu::Vm)->Result<(u8,Arc<mmu::T8140ComputeExecutionContext>)>{if FAIL_PREPARE.swap(false,Ordering::SeqCst){return Err(12)}assert!(!self.0.owners.contains(&id));self.0.owners.push(id);Ok((4,Arc::new(mmu::T8140ComputeExecutionContext,0)?))}fn release_async_compute_queue(&mut self,id:u64){let i=self.0.owners.iter().position(|x|*x==id).expect("only owned reservation can release");self.0.owners.remove(i);self.0.released.push(id)}}
mod g17_job{use super::*;pub struct G17PJobScheduler<T>(Arc<T>);impl<T>G17PJobScheduler<T>{pub fn new(_d:&AsahiDevice,b:Arc<T>,_credits:u32)->Result<Self>{if FAIL_SCHEDULER.swap(false,Ordering::SeqCst){Err(12)}else{Ok(Self(b))}}}}
'''
tests=r'''
#[test]fn constructor_reserves_before_success_and_drop_releases_once(){let rt=G17PRuntimeRef::new();let reg=Arc::new(G17PQueueRegistry{live_queues:AtomicU32::new(0)},0).unwrap();reg.reserve_queue().unwrap();let q=G17PQueue::new(&AsahiDevice,rt,reg.clone(),0,0,0,mmu::Vm,7,0,0).unwrap();assert_eq!(rt.0.lock().unwrap().owners,vec![7]);drop(q);let state=rt.0.lock().unwrap();assert!(state.owners.is_empty());assert_eq!(state.released,vec![7]);assert_eq!(reg.live_queues.load(Ordering::Acquire),0);}
#[test]fn every_constructor_allocation_failure_rolls_back_ownership(){for failure in [&FAIL_FENCE,&FAIL_PREPARE,&FAIL_BACKEND,&FAIL_SCHEDULER]{let rt=G17PRuntimeRef::new();let reg=Arc::new(G17PQueueRegistry{live_queues:AtomicU32::new(0)},0).unwrap();failure.store(true,Ordering::Release);assert!(G17PQueue::new(&AsahiDevice,rt,reg,0,0,0,mmu::Vm,9,0,0).is_err());assert!(rt.0.lock().unwrap().owners.is_empty());assert!(rt.0.lock().unwrap().released.len()<=1);}}
#[test]fn capacity_is_shared_and_released_slots_are_reusable(){let reg=G17PQueueRegistry{live_queues:AtomicU32::new(0)};for _ in 0..G17P_MAX_COMPUTE_QUEUES{reg.reserve_queue().unwrap()}assert_eq!(reg.reserve_queue(),Err(ENOSPC));reg.release_queue();assert_eq!(reg.reserve_queue(),Ok(()));assert_eq!(reg.live_queues.load(Ordering::Acquire),58);}
'''
with tempfile.TemporaryDirectory() as d:
 f=Path(d)/'test.rs';f.write_text(mock+'mod g17_queue_limits{'+limits+'}\nconst G17P_MAX_COMPUTE_QUEUES:u32=g17_queue_limits::MAX_QUEUES;\nimpl G17PQueueRegistry{'+reserve+release+'}\n'+production+'\nimpl G17PQueue{'+constructor+'}\n'+tests)
 Path('/tmp/g17-create-model.rs').write_text(f.read_text())
 subprocess.run(['rustc','--edition=2021','--test',str(f),'-o',str(Path(d)/'test')],check=True)
 subprocess.run([str(Path(d)/'test'),'--test-threads=1'],check=True)
print('PASS actual constructor/drop: scarce ownership exists before success, all allocation failures unwind, shared pool exhaustion/reuse')
