from pathlib import Path
import subprocess,tempfile
p=Path(__file__).resolve().parent
r=p.parent/'mmu.rs'
s=r.read_text()
a=s.index('struct VmDriverMappings {');b=s.index('/// Slot data for a',a)
structs=s[a:b]
a=s.index('    pub(crate) fn install_driver_mappings(');b=s.index('    /// Check that a byte range',a)
methods=s[a:b]
# Job ownership has its own production-source suite; this suite executes the
# full unmap method with no outstanding jobs and keeps testing exact exclusions.
a=methods.index('    pub(crate) fn retain_t8140_job(')
b=methods.index('    /// Remove userspace mappings',a)
methods=methods[:a]+methods[b:]
preamble=r'''
#![allow(dead_code)]
use std::ops::{Range,Deref,DerefMut};
use std::pin::Pin;
// Kernel prelude exports Init::chain for every type; model that ambiguity.
trait Init:Sized{fn chain(self,_next:impl Sized){}}
impl<T>Init for T{}
use std::sync::{Arc,Mutex as StdMutex,MutexGuard};
use std::sync::atomic::{AtomicBool,AtomicUsize,Ordering};
type KVec<T> = Vec<T>;
type Result<T=()> = std::result::Result<T,i32>;
const EINVAL:i32=22;const EBUSY:i32=16;const EIO:i32=5;
macro_rules! no_debug {($x:ty)=>{}}
trait RangeExt {fn overlaps(&self,other:Range<u64>)->bool;}
impl RangeExt for Range<u64>{fn overlaps(&self,other:Range<u64>)->bool{self.start<other.end&&other.start<self.end}}
struct TestMutex<T>{inner:StdMutex<T>,held:Arc<AtomicBool>}
struct TestGuard<'a,T>{guard:MutexGuard<'a,T>,held:&'a AtomicBool}
impl<T> TestMutex<T>{fn lock(&self)->TestGuard<'_,T>{let guard=self.inner.lock().unwrap();assert!(!self.held.swap(true,Ordering::SeqCst));TestGuard{guard,held:&self.held}}}
// Kernel guards expose an inherent pinned as_mut; Option::as_mut must be explicit.
impl<T:Unpin>TestGuard<'_,T>{fn as_mut(&mut self)->Pin<&mut T>{Pin::new(&mut *self.guard)}}
impl<T>Deref for TestGuard<'_,T>{type Target=T;fn deref(&self)->&T{&self.guard}}
impl<T>DerefMut for TestGuard<'_,T>{fn deref_mut(&mut self)->&mut T{&mut self.guard}}
impl<T>Drop for TestGuard<'_,T>{fn drop(&mut self){assert!(self.held.swap(false,Ordering::SeqCst));}}
struct KernelMapping{range:Range<u64>,held:Arc<AtomicBool>,drops:Arc<AtomicUsize>}
impl KernelMapping{fn iova_range(&self)->Range<u64>{self.range.clone()}}
impl Drop for KernelMapping{fn drop(&mut self){assert!(!self.held.load(Ordering::SeqCst),"mapping destroyed under driver_mappings lock");self.drops.fetch_add(1,Ordering::SeqCst);}}
'''
vm=r'''
struct JobLifetime{active:usize,closed:bool,close_ranges:Option<(Range<u64>,Range<u64>)>}
struct Vm{job_lifetime:TestMutex<JobLifetime>,driver_mappings:TestMutex<Option<VmDriverMappings>>,unmapped:StdMutex<Vec<Range<u64>>>}
impl Vm{
fn new()->Self{Self{job_lifetime:TestMutex{inner:StdMutex::new(JobLifetime{active:0,closed:false,close_ranges:None}),held:Arc::new(AtomicBool::new(false))},driver_mappings:TestMutex{inner:StdMutex::new(None),held:Arc::new(AtomicBool::new(false))},unmapped:StdMutex::new(Vec::new())}}
fn mapping(&self,range:Range<u64>,drops:&Arc<AtomicUsize>)->KernelMapping{KernelMapping{range,held:self.driver_mappings.held.clone(),drops:drops.clone()}}
fn unmap_range(&self,start:u64,size:u64)->Result{self.unmapped.lock().unwrap().push(start..start+size);Ok(())}
'''
tests=r'''
fn main(){
let drops=Arc::new(AtomicUsize::new(0));let vm=Vm::new();
assert!(!vm.render_pool_mappings_match(1));vm.clear_render_pool_mappings();
assert_eq!(vm.install_render_pool_mappings(1,vec![vm.mapping(96..112,&drops)]),Err(EINVAL));assert_eq!(drops.load(Ordering::SeqCst),1);
assert_eq!(vm.install_driver_mappings(vec![vm.mapping(16..32,&drops)],vec![192..224]),Ok(()));
assert_eq!(vm.install_render_pool_mappings(0,vec![vm.mapping(96..112,&drops)]),Err(EINVAL));assert_eq!(drops.load(Ordering::SeqCst),2);
assert_eq!(vm.install_render_pool_mappings(7,vec![vm.mapping(96..112,&drops),vm.mapping(160..176,&drops)]),Ok(()));
assert!(vm.render_pool_mappings_match(7));assert!(!vm.render_pool_mappings_match(0));assert!(!vm.render_pool_mappings_match(8));
assert_eq!(vm.install_render_pool_mappings(7,vec![vm.mapping(128..144,&drops)]),Err(EBUSY));
assert_eq!(vm.install_render_pool_mappings(8,vec![vm.mapping(128..144,&drops)]),Err(EBUSY));assert_eq!(drops.load(Ordering::SeqCst),4);
assert!(vm.driver_range_overlaps(20..21));assert!(vm.driver_range_overlaps(100..101));assert!(vm.driver_range_overlaps(168..169));assert!(vm.driver_range_overlaps(200..201));assert!(!vm.driver_range_overlaps(32..64));assert!(!vm.driver_range_overlaps(80..96));
assert_eq!(vm.unmap_user_ranges(0..256,64..80),Ok(()));
assert_eq!(*vm.unmapped.lock().unwrap(),vec![0..16,32..64,80..96,112..160,176..192,224..256]);
vm.clear_render_pool_mappings();assert_eq!(drops.load(Ordering::SeqCst),6);assert!(!vm.render_pool_mappings_match(7));assert!(!vm.driver_range_overlaps(100..101));assert!(vm.driver_range_overlaps(20..21));assert!(vm.driver_range_overlaps(200..201));
vm.clear_render_pool_mappings();assert_eq!(drops.load(Ordering::SeqCst),6);
vm.unmapped.lock().unwrap().clear();assert_eq!(vm.unmap_user_ranges(0..256,64..80),Ok(()));assert_eq!(*vm.unmapped.lock().unwrap(),vec![0..16,32..64,80..192,224..256]);
assert_eq!(vm.install_render_pool_mappings(8,vec![vm.mapping(128..144,&drops)]),Ok(()));assert!(vm.render_pool_mappings_match(8));
drop(vm);assert_eq!(drops.load(Ordering::SeqCst),8);
let vm=Vm::new();let drops=Arc::new(AtomicUsize::new(0));
vm.install_driver_mappings(vec![],vec![]).unwrap();
vm.install_render_pool_mappings(10,vec![vm.mapping(16..32,&drops)]).unwrap();
vm.install_compute_pool_mappings(20,vec![vm.mapping(64..96,&drops)]).unwrap();
assert!(vm.compute_pool_mappings_match(20));assert!(!vm.compute_pool_mappings_match(21));
assert!(vm.driver_range_overlaps(80..81));
vm.unmap_user_ranges(0..128,128..128).unwrap();
assert_eq!(*vm.unmapped.lock().unwrap(),vec![0..16,32..64,96..128]);
vm.clear_compute_pool_mappings();assert!(vm.render_pool_mappings_match(10));
assert_eq!(drops.load(Ordering::SeqCst),1);vm.clear_compute_pool_mappings();
drop(vm);assert_eq!(drops.load(Ordering::SeqCst),2);
println!("PASS actual mmu cache source: install/duplicate/clear/generation, mapping drops outside lock, fixed+cached overlap and exact user-unmap exclusions");
}
'''
code=preamble+structs+vm+methods+'}\n'+tests
with tempfile.TemporaryDirectory() as d:
    source=Path(d)/'actual_source_cache_test.rs'
    executable=Path(d)/'test-cache'
    source.write_text(code)
    subprocess.run(['rustc','--edition=2021','-C','opt-level=2',str(source),'-o',str(executable)],check=True)
    subprocess.run([str(executable)],check=True)
