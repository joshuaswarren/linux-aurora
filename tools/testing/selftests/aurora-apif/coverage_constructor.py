#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Exercise extracted old/new constructors with host-allocation/provider failures."""
from pathlib import Path
import subprocess
import tempfile
temporary=tempfile.TemporaryDirectory(prefix='asahi-coverage-constructor-')
r=Path(temporary.name)
tree=Path(__file__).resolve().parents[4]
relative='drivers/gpu/drm/asahi/mmu/sptm_pgtable.rs'
def constructor_text(s):
    a=s.index('    pub(super) fn new(transport:');b=s.index('{',a);depth=1;i=b+1
    while depth:
        depth+=(s[i]=='{')-(s[i]=='}');i+=1
    return s[a:i].replace('pub(super) fn new','pub fn new',1)
new=constructor_text((tree/relative).read_text())
old=new.replace('        let removals = super::coverage::RemovalLog::new()?;\n','')
assert old != new
old=old.replace(', removals })', ', removals: super::coverage::RemovalLog::new()? })')
assert 'let removals =' not in old
pre=r'''
#![allow(dead_code)]
use std::sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}};
type Result<T> = std::result::Result<T,()>;
static FAIL_LOG: AtomicBool=AtomicBool::new(false);
static FAIL_PROVIDER: AtomicBool=AtomicBool::new(false);
static CREATED: AtomicUsize=AtomicUsize::new(0);
static LIVE: AtomicUsize=AtomicUsize::new(0);
static LIVE_LOGS: AtomicUsize=AtomicUsize::new(0);
struct Transport;struct VmStatus;struct RBTree;struct DonorFrame;
struct KVec<T>(std::marker::PhantomData<T>);impl<T>KVec<T>{fn new()->Self{Self(std::marker::PhantomData)}}
impl RBTree { fn new()->Self {Self} }
struct AddressSpace;
impl AddressSpace {
 fn private(_: &Arc<Transport>)->Result<Self> {
  if FAIL_PROVIDER.load(Ordering::SeqCst) {return Err(())}
  CREATED.fetch_add(1,Ordering::SeqCst);LIVE.fetch_add(1,Ordering::SeqCst);Ok(Self)
 }
}
// As in production, AddressSpace/PrivateSpace has no implicit provider Drop.
mod coverage {
 use super::*;
 pub struct RemovalLog;
 impl RemovalLog {
  pub fn new()->Result<Arc<Self>> {
   if FAIL_LOG.load(Ordering::SeqCst) {return Err(())}
   LIVE_LOGS.fetch_add(1,Ordering::SeqCst);Ok(Arc::new(Self))
  }
 }
 impl Drop for RemovalLog { fn drop(&mut self) {LIVE_LOGS.fetch_sub(1,Ordering::SeqCst);} }
}
'''
struct=r'''
use super::*;
pub struct PageTable {space:AddressSpace,runs:RBTree,status:Arc<VmStatus>,tables:RBTree,table_count:u64,removals:Arc<coverage::RemovalLog>}
// Constructor tests concern empty/unbound tables, whose production Drop destroys the space.
impl Drop for PageTable { fn drop(&mut self) {LIVE.fetch_sub(1,Ordering::SeqCst);} }
'''
tests=r'''
fn reset(){FAIL_LOG.store(false,Ordering::SeqCst);FAIL_PROVIDER.store(false,Ordering::SeqCst);CREATED.store(0,Ordering::SeqCst);LIVE.store(0,Ordering::SeqCst);LIVE_LOGS.store(0,Ordering::SeqCst);}
#[test] fn old_constructor_reproduces_provider_handle_leak_on_log_failure(){
 reset();FAIL_LOG.store(true,Ordering::SeqCst);
 assert!(old::PageTable::new(&Arc::new(Transport),Arc::new(VmStatus)).is_err());
 assert_eq!(CREATED.load(Ordering::SeqCst),1);assert_eq!(LIVE.load(Ordering::SeqCst),1);
}
#[test] fn new_log_failure_never_acquires_provider_handle(){
 reset();FAIL_LOG.store(true,Ordering::SeqCst);
 assert!(new::PageTable::new(&Arc::new(Transport),Arc::new(VmStatus)).is_err());
 assert_eq!(CREATED.load(Ordering::SeqCst),0);assert_eq!(LIVE.load(Ordering::SeqCst),0);
}
#[test] fn provider_failure_drops_preallocated_log(){
 reset();FAIL_PROVIDER.store(true,Ordering::SeqCst);
 assert!(new::PageTable::new(&Arc::new(Transport),Arc::new(VmStatus)).is_err());
 assert_eq!(CREATED.load(Ordering::SeqCst),0);assert_eq!(LIVE_LOGS.load(Ordering::SeqCst),0);
}
#[test] fn successful_constructor_retains_until_table_drop(){
 reset();let table=new::PageTable::new(&Arc::new(Transport),Arc::new(VmStatus)).unwrap();
 assert_eq!(CREATED.load(Ordering::SeqCst),1);assert_eq!(LIVE.load(Ordering::SeqCst),1);assert_eq!(LIVE_LOGS.load(Ordering::SeqCst),1);
 drop(table);assert_eq!(LIVE.load(Ordering::SeqCst),0);assert_eq!(LIVE_LOGS.load(Ordering::SeqCst),0);
}
'''
p=r/'constructor-failure.rs';p.write_text(pre+'\nmod old {\n'+struct.replace('table_count:u64,','table_count:u64,donors:KVec<DonorFrame>,')+'impl PageTable {\n'+old+'\n}\n}\nmod new {\n'+struct.replace('table_count:u64,','table_count:u64,donors:KVec<DonorFrame>,')+'impl PageTable {\n'+new+'\n}\n}\n'+tests)
subprocess.run(['rustc','--test',str(p),'-o',str(p.with_suffix(''))],check=True)
subprocess.run([str(p.with_suffix('')),'--test-threads=1'],check=True)
