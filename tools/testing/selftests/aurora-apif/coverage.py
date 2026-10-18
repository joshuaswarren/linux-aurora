#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Execute production coverage logic with host locks and a model page table."""
from pathlib import Path
import subprocess
import tempfile

tree = Path(__file__).resolve().parents[4]
source = tree / 'drivers/gpu/drm/asahi/mmu/coverage.rs'
body = source.read_text().replace('use super::Vm;\n', '')
body = body.replace('//!', '//')
body = body.replace('use kernel::{new_spinlock, prelude::*, sync::{Arc, SpinLock}};\n', '')
body = body.replace('#[pin_data]\n', '').replace('    #[pin]\n', '')
start = body.index('    pub(crate) fn new() -> Result<Arc<Self>> {')
end, depth = body.index('{', start) + 1, 1
while depth:
    depth += (body[end] == '{') - (body[end] == '}')
    end += 1
body = body[:start] + '''    pub(crate) fn new() -> Result<Arc<Self>> {
        Ok(Arc::new(Self { sequence: AtomicU64::new(0),
            ring: SpinLock::new(Ring { entries: [(0,0,0);LOG_ENTRIES],next:0,complete_from:1 }) }))
    }''' + body[end:]
body = body.replace('*crate::module_parameters::coverage_check.value()', 'crate::CHECK.load(Ordering::Relaxed)')
prefix = r'''
#![allow(dead_code,unused_imports)]
use std::sync::{Arc,Mutex};
use std::sync::atomic::{AtomicBool,AtomicU32,AtomicU64,Ordering};
use std::ops::{Deref,DerefMut,Range};
type Result<T=()> = std::result::Result<T,i32>;
const EIO:i32=5;const ENOMEM:i32=12;const GFP_KERNEL:u32=0;
static CHECK:AtomicU32=AtomicU32::new(0);
static FAIL_ALLOC:AtomicBool=AtomicBool::new(false);
macro_rules! pr_err {($($x:tt)*)=>{};}
struct SpinLock<T>(Mutex<T>);
impl<T> SpinLock<T>{fn new(v:T)->Self{Self(Mutex::new(v))}fn lock(&self)->std::sync::MutexGuard<'_,T>{self.0.lock().unwrap()}}
struct KVec<T>(Vec<T>);
impl<T> KVec<T>{fn new()->Self{Self(Vec::new())}fn with_capacity(n:usize,_g:u32)->Result<Self>{
 if FAIL_ALLOC.swap(false,Ordering::Relaxed){Err(ENOMEM)}else{Ok(Self(Vec::with_capacity(n)))}}
 fn push(&mut self,v:T,_g:u32)->Result{self.0.push(v);Ok(())}
 fn retain(&mut self,f:impl FnMut(&mut T)->bool){self.0.retain_mut(f)}}
impl<T>Deref for KVec<T>{type Target=Vec<T>;fn deref(&self)->&Vec<T>{&self.0}}
impl<T>DerefMut for KVec<T>{fn deref_mut(&mut self)->&mut Vec<T>{&mut self.0}}
mod mmu{pub const UAT_PGMSK:usize=16383;}
mod g17{pub fn now_ns()->u64{use std::sync::atomic::{AtomicU64,Ordering};static N:AtomicU64=AtomicU64::new(0);N.fetch_add(1,Ordering::Relaxed)}}
struct Table{runs:Vec<(u64,u64,u8)>,uncertain:bool}
impl Table{fn visit_runs(&self,range:Range<u64>,mut f:impl FnMut(u64,u64,u8)->Result)->Result<bool>{
 if self.uncertain{return Err(EIO)};let mut at=range.start;
 while at<range.end{let Some(&(s,e,a))=self.runs.iter().find(|&&(s,e,_)|s<=at&&at<e)else{return Ok(false)};
 f(s,e,a)?;at=e;}Ok(true)}}
struct Inner{va_range:Range<u64>,page_table:Table}
struct InnerLock(Mutex<Inner>);
impl InnerLock{fn exec_lock(&self,_a:Option<()>,_b:bool)->Result<std::sync::MutexGuard<'_,Inner>>{Ok(self.0.lock().unwrap())}}
struct Vm{removals:Option<Arc<production::RemovalLog>>,inner:InnerLock,epoch:AtomicU64,calls:AtomicU32,revoke_second:AtomicBool}
impl Vm{
 fn new()->Self{Self{removals:Some(production::RemovalLog::new().unwrap()),inner:InnerLock(Mutex::new(Inner{
 va_range:0..(1<<30),page_table:Table{runs:Vec::new(),uncertain:false}})),epoch:AtomicU64::new(0),calls:AtomicU32::new(0),revoke_second:AtomicBool::new(false)}}
 fn mapping_validation_epoch(&self)->Option<u64>{
  if self.calls.fetch_add(1,Ordering::Relaxed)==1&&self.revoke_second.swap(false,Ordering::Relaxed){self.set_page(0,0);}
  let e=self.epoch.load(Ordering::Acquire);(e!=u64::MAX).then_some(e)}
 fn covers_range_locked(inner:&mut Inner,address:u64,size:u64,read:bool,write:bool)->bool{
  let Some(end)=address.checked_add(size)else{return false};if size==0{return false}
  let start=address&!16383;let Some(end)=end.checked_add(16383).map(|e|e&!16383)else{return false};
  if start<inner.va_range.start||end>inner.va_range.end{return false};let access=u8::from(read)|u8::from(write)<<1;
  let mut allowed=true;inner.page_table.visit_runs(start..end,|_,_,a|{allowed&=a&access==access;Ok(())}).unwrap_or(false)&&allowed}
 fn covers_ranges_batch(&self,rs:&[(u64,u64,u8)])->(bool,Option<u64>){let mut i=self.inner.exec_lock(None,false).unwrap();
  (rs.iter().all(|&(a,n,p)|Self::covers_range_locked(&mut i,a,n,p&1!=0,p&2!=0)),Some(0))}
 fn set_page(&self,page:u64,access:u8){let mut i=self.inner.exec_lock(None,false).unwrap();let previous=self.epoch.swap(u64::MAX,Ordering::AcqRel);
  let start=page*16384;let exists=i.page_table.runs.iter().any(|r|r.0==start);
  if exists{self.removals.as_ref().unwrap().record(start,start+16384);}
  i.page_table.runs.retain(|r|r.0!=start);if access!=0{i.page_table.runs.push((start,start+16384,access));}
  i.page_table.runs.sort_unstable();self.epoch.store(previous.wrapping_add(1),Ordering::Release);}
}
mod production{use super::*;
'''
tests = r'''
#[cfg(test)]mod tests{use super::*;
fn ask(c:&mut CoverageCache,v:&Vm,page:u64,n:u64,a:u8)->bool{c.covers(v,&[(page*16384,n*16384,a)]).unwrap()}
#[test]fn cached_proof_survives_unrelated_addition(){let v=Vm::new();v.set_page(0,3);let mut c=CoverageCache::new();
 assert!(ask(&mut c,&v,0,1,3));let walks=c.stats.walks;v.set_page(5,1);assert!(ask(&mut c,&v,0,1,3));assert_eq!(c.stats.walks,walks);}
#[test]fn revoked_or_downgraded_permissions_never_hit(){let v=Vm::new();v.set_page(0,3);let mut c=CoverageCache::new();
 assert!(ask(&mut c,&v,0,1,3));v.set_page(0,1);assert!(!ask(&mut c,&v,0,1,2));assert!(ask(&mut c,&v,0,1,1));
 v.set_page(0,0);assert!(!ask(&mut c,&v,0,1,1));}
#[test]fn unrelated_removal_keeps_other_intervals(){let v=Vm::new();v.set_page(0,3);v.set_page(5,3);let mut c=CoverageCache::new();
 assert!(ask(&mut c,&v,0,1,3));let walks=c.stats.walks;v.set_page(5,0);assert!(ask(&mut c,&v,0,1,3));assert_eq!(c.stats.walks,walks);}
#[test]fn ring_wrap_discards_unrecoverable_history(){let v=Vm::new();v.set_page(0,3);let mut c=CoverageCache::new();assert!(ask(&mut c,&v,0,1,3));
 v.set_page(0,0);for n in 1..100{v.set_page(n,3);v.set_page(n,0);}assert!(!ask(&mut c,&v,0,1,1));}
#[test]fn sequence_exhaustion_stays_fail_closed(){let v=Vm::new();v.set_page(0,3);let mut c=CoverageCache::new();assert!(ask(&mut c,&v,0,1,3));
 let log=v.removals.as_ref().unwrap();log.sequence.store(u64::MAX-1,Ordering::Relaxed);v.set_page(0,0);
 for n in 1..100{v.set_page(n,3);v.set_page(n,0);}assert_eq!(log.sequence(),None);assert!(!ask(&mut c,&v,0,1,1));}
#[test]fn uncertain_table_clears_old_proofs(){let v=Vm::new();v.set_page(0,3);let mut c=CoverageCache::new();assert!(ask(&mut c,&v,0,1,3));
 v.inner.exec_lock(None,false).unwrap().page_table.uncertain=true;v.removals.as_ref().unwrap().record_all();assert!(!ask(&mut c,&v,0,1,1));}
#[test]fn mixed_permissions_and_holes_are_not_merged(){let v=Vm::new();v.set_page(0,3);v.set_page(1,1);v.set_page(3,3);let mut c=CoverageCache::new();
 assert!(!ask(&mut c,&v,0,2,3));assert!(ask(&mut c,&v,0,2,1));assert!(!ask(&mut c,&v,0,4,1));assert!(!ask(&mut c,&v,1,1,2));}
#[test]fn long_ranges_are_fully_learned(){let v=Vm::new();for n in 0..300{v.set_page(n,3)}let mut c=CoverageCache::new();assert!(ask(&mut c,&v,0,300,3));
 let walks=c.stats.walks;assert!(ask(&mut c,&v,100,100,3));assert_eq!(c.stats.walks,walks);}
#[test]fn allocation_failure_cannot_invent_a_proof(){let v=Vm::new();v.set_page(0,3);let mut c=CoverageCache::new();FAIL_ALLOC.store(true,Ordering::Relaxed);
 assert!(ask(&mut c,&v,0,1,3));assert!(c.intervals.is_empty());assert!(ask(&mut c,&v,0,1,3));v.set_page(0,0);assert!(!ask(&mut c,&v,0,1,3));}
#[test]fn removal_between_hit_checks_is_detected(){let v=Vm::new();v.set_page(0,3);let mut c=CoverageCache::new();assert!(ask(&mut c,&v,0,1,3));
 v.calls.store(0,Ordering::Relaxed);v.revoke_second.store(true,Ordering::Relaxed);assert!(!ask(&mut c,&v,0,1,3));}
#[test]fn invalid_ranges_fail_closed(){let v=Vm::new();v.set_page(0,3);let mut c=CoverageCache::new();assert_eq!(c.covers(&v,&[(0,0,1)]),Some(false));
 assert_eq!(c.covers(&v,&[(u64::MAX-4,10,1)]),Some(false));assert_eq!(c.covers(&v,&[(1<<30,16384,1)]),Some(false));}
#[test]fn subpage_requests_cover_every_intersected_page(){let v=Vm::new();v.set_page(0,3);v.set_page(1,1);let mut c=CoverageCache::new();
 assert_eq!(c.covers(&v,&[(16380,8,1)]),Some(true));assert_eq!(c.covers(&v,&[(16380,8,2)]),Some(false));}
#[test]fn in_progress_epoch_forces_a_locked_walk(){let v=Vm::new();v.set_page(0,3);let mut c=CoverageCache::new();assert!(ask(&mut c,&v,0,1,3));
 let walks=c.stats.walks;v.epoch.store(u64::MAX,Ordering::Release);assert!(ask(&mut c,&v,0,1,3));assert_eq!(c.stats.walks,walks+1);}
#[test]fn diagnostic_checks_the_real_page_table(){let v=Vm::new();v.set_page(0,3);let mut c=CoverageCache::new();assert!(ask(&mut c,&v,0,1,3));
 CHECK.store(1,Ordering::Relaxed);assert!(ask(&mut c,&v,0,1,3));CHECK.store(0,Ordering::Relaxed);assert_eq!(c.stats.checks,1);assert_eq!(c.stats.mismatches,0);}
#[test]fn interval_capacity_restarts_without_losing_validation(){let v=Vm::new();let mut c=CoverageCache::new();
 for n in 0..600{v.set_page(n*2,3);assert!(ask(&mut c,&v,n*2,1,3));}
 assert!(c.stats.restarts>0);assert!(ask(&mut c,&v,0,1,3));assert!(!ask(&mut c,&v,1,1,1));}
#[test]fn randomized_mappings_match_independent_page_permissions(){let v=Vm::new();let mut c=CoverageCache::new();let mut pages=[0u8;64];let mut x=7u64;
 for _ in 0..20000{x=x.wrapping_mul(6364136223846793005).wrapping_add(1);let page=((x>>32)%64)as usize;
 if x&3==0{let access=((x>>12)&3)as u8;v.set_page(page as u64,access);pages[page]=access;}
 else{let n=(((x>>16)%4)+1).min((64-page)as u64);let a=((x>>20)%3+1)as u8;
 let expected=pages[page..page+n as usize].iter().all(|&p|p!=0&&p&a==a);assert_eq!(ask(&mut c,&v,page as u64,n,a),expected);}}}
}}
'''
with tempfile.TemporaryDirectory(prefix='asahi-coverage-test-') as directory:
    path = Path(directory) / 'test.rs'
    path.write_text(prefix + body + tests)
    binary = Path(directory) / 'test'
    subprocess.run(['rustc', '--edition=2021', '--test', str(path), '-o', str(binary)], check=True)
    subprocess.run([str(binary), '--test-threads=1'], check=True)
    negatives = [
        ('missing invalidation', 'self.catch_up(log, sequence);', '',
         'revoked_or_downgraded_permissions_never_hit ... FAILED'),
        ('missing permission check', 'if allowed & access != access {', 'if false {',
         'mixed_permissions_and_holes_are_not_merged ... FAILED'),
    ]
    for name, before, after, failure in negatives:
        assert before in body
        path.write_text(prefix + body.replace(before, after) + tests)
        subprocess.run(['rustc', '--edition=2021', '--test', str(path), '-o', str(binary)], check=True)
        run = subprocess.run([str(binary), '--test-threads=1'], capture_output=True, text=True)
        assert run.returncode != 0 and failure in run.stdout, (name, run.stdout)
        print('Negative control rejected:', name)

pgtable = (tree / 'drivers/gpu/drm/asahi/mmu/sptm_pgtable.rs').read_text()
unmap = pgtable[pgtable.index('    pub(super) fn unmap_pages('):]
assert unmap.index('self.removals.record(range.start, range.end);') < unmap.index('self.space.unmap_ranges(')
uncertain = pgtable[pgtable.index('    fn report_uncertain('):pgtable.index('    pub(super) fn validate_range(')]
assert 'self.removals.record_all();' in uncertain
print('Provider unmap/uncertainty hooks are present before revocation')
