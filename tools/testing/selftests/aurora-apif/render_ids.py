#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Run the actual RenderIds/RenderId/RenderIdPin methods with host synchronization."""
from pathlib import Path
import hashlib
import json
import re
import subprocess
import tempfile

temporary = tempfile.TemporaryDirectory(prefix='asahi-render-ids-')
root = Path(temporary.name)
tree = Path(__file__).resolve().parents[4]
source = tree / 'drivers/gpu/drm/asahi/g17/freelist.rs'
text = source.read_text()
start = text.index('const NO_RENDER_ID:')
end = text.index('/// Profile-specific descriptor data', start)
body = text[start:end].replace('#[pin_data]\n', '').replace('    #[pin]\n', '')
begin = body.index('    pub(crate) fn new() -> Result<Arc<Self>> {')
brace = body.index('{', begin)
depth = 1
stop = brace + 1
while depth:
    depth += (body[stop] == '{') - (body[stop] == '}')
    stop += 1
constructor = '''    pub(crate) fn new() -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            used: [AtomicU64::new(0), AtomicU64::new(1 << 63)],
            available: super::availability::Availability::new()?,
            state: Mutex::new(RenderIdState { leases: render_ids::State::new(), cells: [const { None }; render_ids::COUNT] }),
        }))
    }'''
body = body[:begin] + constructor + body[stop:]
body = body.replace('Arc::new(RenderIdCell {', 'host_arc(RenderIdCell {')
pin_start = text.index('pub(crate) struct PoolPin')
pin_end = text.index('\nimpl RenderPool {', pin_start)
pin_type = text[pin_start:pin_end]
method_start = text.index('    pub(crate) fn pin_id(', pin_end)
method_brace = text.index('{', method_start)
method_end = method_brace + 1
depth = 1
while depth:
    depth += (text[method_end] == '{') - (text[method_end] == '}')
    method_end += 1
pool_stub = '''
struct RenderPool { id: RenderId, epoch: AtomicU64, served: AtomicBool,
 storage:Arc<RenderStorage>, layout:u8 }
'''
cache_start = text.index('    pub(crate) fn cache_reclaimed(')
cache_brace = text.index('{', cache_start)
cache_end = cache_brace + 1
depth = 1
while depth:
    depth += (text[cache_end] == '{') - (text[cache_end] == '}')
    cache_end += 1
body += pool_stub + pin_type + '\nimpl RenderPool {\n' + text[method_start:method_end] + text[cache_start:cache_end] + '\n}\n'
prefix = r'''
#![allow(dead_code, unused_variables)]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool,AtomicU16,AtomicU32,AtomicU64,Ordering};
type Result<T=()> = std::result::Result<T,i32>;
const ENOSPC:i32=28; const EOVERFLOW:i32=75; const EIO:i32=5; const GFP_KERNEL:u32=0;
macro_rules! enospc_site { ($x:expr) => { ENOSPC }; }
macro_rules! pr_info { ($($x:tt)*) => {}; }
struct Mutex<T>(std::sync::Mutex<T>);
impl<T> Mutex<T> { fn new(x:T)->Self { Self(std::sync::Mutex::new(x)) }
fn lock(&self)->std::sync::MutexGuard<'_,T> { self.0.lock().unwrap() } }
fn host_arc<T>(x:T,_flags:u32)->Result<Arc<T>> { Ok(Arc::new(x)) }
mod availability { use super::*; pub struct Availability{ pub notifications:AtomicU64 }
impl Availability { pub fn new()->Result<Arc<Self>>{Ok(Arc::new(Self{notifications:AtomicU64::new(0)}))}
pub fn notify(&self){self.notifications.fetch_add(1,Ordering::Relaxed);} } }
mod progress { use super::*; pub fn sampled(counter:&AtomicU64)->Option<u64> {
let n=counter.fetch_add(1,Ordering::Relaxed)+1; if n<=8 || n.is_power_of_two(){Some(n)}else{None} } }
mod production { use super::*;
struct RenderBacking { drops: Arc<AtomicU64> }
impl Drop for RenderBacking {
 fn drop(&mut self) { self.drops.fetch_add(1,Ordering::Relaxed); }
}
struct Pages { layout:u8 }
impl RenderBacking { fn into_pages(self)->Pages { Pages{layout:1} } }
struct RenderStorage { backing: Mutex<Option<RenderBacking>>, spare: Mutex<Option<Pages>>, page_list: AtomicU64 }
impl RenderStorage {
 fn new(backing:Option<RenderBacking>)->Result<Arc<Self>> {
  Ok(Arc::new(Self{backing:Mutex::new(backing),spare:Mutex::new(None),page_list:AtomicU64::new(0)}))
 }
}
'''
module = tree / 'drivers/gpu/drm/asahi/g17/freelist/render_ids.rs'
prefix += f'#[path="{module}"] mod render_ids;\n'
tests = r'''
#[cfg(test)] mod wrapper_tests {
use super::*;
fn host_pool(a:&Arc<RenderIds>,layout:u8)->Arc<RenderPool> {
 let id=RenderId::logical(a).unwrap();let storage=id.storage().unwrap();
 Arc::new(RenderPool{id,storage,layout,epoch:AtomicU64::new(0),served:AtomicBool::new(false)})
}
#[test] fn pinned_real_ids_are_never_reclaimed() {
 let a=RenderIds::new().unwrap(); let mut ids=Vec::new(); let mut pins=Vec::new();
 for _ in 0..127 {let id=RenderId::logical(&a).unwrap();pins.push(id.pin(||{}).unwrap());ids.push(id);}
 assert!(!a.reclaim_one(|_|panic!("a live pin reached the firmware proof")));
 assert!(matches!(RenderId::logical(&a).unwrap().pin(||{}),Err(ENOSPC)));
}
#[test] fn reclaim_invalidates_only_the_old_cell_after_firmware_release() {
 let a=RenderIds::new().unwrap(); let mut ids=Vec::new(); let mut pins=Vec::new();
 for _ in 0..127 {let id=RenderId::logical(&a).unwrap();pins.push(id.pin(||{}).unwrap());ids.push(id);}
 let old=ids[12].slot();drop(pins[12].take());
 assert!(!a.reclaim_one(|_|false));assert_eq!(ids[12].slot(),old);
 assert!(a.reclaim_one(|x|x==old));assert_eq!(ids[12].slot(),NO_RENDER_ID);
 let next=RenderId::logical(&a).unwrap();let _pin=next.pin(||{}).unwrap();assert_eq!(next.slot(),old);
 assert!(matches!(ids[12].pin(||{}),Err(ENOSPC)));
}
#[test] fn construction_failure_releases_after_the_last_pin() {
 let a=RenderIds::new().unwrap();let id=RenderId::logical(&a).unwrap();let pin=id.pin(||{}).unwrap();
 let slot=id.slot();drop(id);assert!(!a.reclaim_one(|_|true));drop(pin);
 let next=RenderId::logical(&a).unwrap();let _pin=next.pin(||{}).unwrap();assert_eq!(next.slot(),slot);
}
#[test] fn same_owner_does_not_repeat_the_assignment_callback() {
 let a=RenderIds::new().unwrap();let id=RenderId::logical(&a).unwrap();let n=AtomicU32::new(0);
 let first=id.pin(||{n.fetch_add(1,Ordering::Relaxed);}).unwrap();
 let second=id.pin(||{n.fetch_add(1,Ordering::Relaxed);}).unwrap();assert_eq!(n.load(Ordering::Relaxed),1);
 drop(first);assert!(!a.reclaim_one(|_|true));drop(second);assert!(a.reclaim_one(|_|true));
 let _third=id.pin(||{n.fetch_add(1,Ordering::Relaxed);}).unwrap();assert_eq!(n.load(Ordering::Relaxed),2);
}
#[test] fn legacy_ids_keep_the_original_fixed_lifetime() {
 let a=RenderIds::new().unwrap();let mut ids:Vec<_>=(0..127).map(|_|RenderId::new(&a).unwrap()).collect();
 assert!(matches!(RenderId::new(&a),Err(ENOSPC)));assert!(!a.reclaim_one(|_|true));
 let id=ids.remove(4);let slot=id.slot();drop(id);assert_eq!(RenderId::new(&a).unwrap().slot(),slot);
}
#[test] fn concurrent_pins_balance_without_rebinding() {
 let a=RenderIds::new().unwrap();let id=Arc::new(RenderId::logical(&a).unwrap());
 let first=id.pin(||{}).unwrap();let slot=id.slot();let mut threads=Vec::new();
 for _ in 0..8 {let id=id.clone();threads.push(std::thread::spawn(move||{for _ in 0..1000{let p=id.pin(||panic!("unexpected reassignment")).unwrap();drop(p);}}));}
 for thread in threads{thread.join().unwrap();}assert_eq!(id.slot(),slot);assert!(!a.reclaim_one(|_|true));
 drop(first);assert!(a.reclaim_one(|_|true));
}
#[test] fn production_pool_pin_keeps_the_logical_pool_alive() {
 let a=RenderIds::new().unwrap();let pool=host_pool(&a,1);
 let weak=Arc::downgrade(&pool);let pin=pool.pin_id().unwrap();drop(pool);
 assert!(weak.upgrade().is_some());assert!(!a.reclaim_one(|_|true));drop(pin);
 assert!(weak.upgrade().is_none());
}
#[test] fn production_pool_epoch_advances_once_per_reassignment() {
 let a=RenderIds::new().unwrap();let pool=host_pool(&a,1);
 let pin=pool.pin_id().unwrap();let next=pool.pin_id().unwrap();
 assert_eq!(pool.epoch.load(Ordering::Acquire),1);drop((pin,next));
 assert!(a.reclaim_one(|_|true));let _pin=pool.pin_id().unwrap();
 assert_eq!(pool.epoch.load(Ordering::Acquire),2);assert!(pool.served.load(Ordering::Acquire));
}
#[test] fn concurrent_reclaim_never_changes_pinned_cells() {
 let a=RenderIds::new().unwrap();let owners=Arc::new((0..256)
 .map(|_|Arc::new(RenderId::logical(&a).unwrap())).collect::<Vec<_>>());
 let mut threads=Vec::new();
 for thread in 0..8 {let a=a.clone();let owners=owners.clone();threads.push(std::thread::spawn(move||{
  let mut success=0;
  for i in 0..1000 {let id=&owners[(i*17+thread*31)%owners.len()];
   if let Ok(Some(pin))=id.pin(||{}) {let slot=id.slot();assert_eq!(slot,pin.lease.slot);
    a.reclaim_one(|_|true);std::thread::yield_now();assert_eq!(id.slot(),slot);drop(pin);success+=1;
   } else {a.reclaim_one(|_|true);}
  } success
 }));}
 for thread in threads {assert!(thread.join().unwrap()>0);}
}
#[test] fn backing_detaches_only_after_unpin_and_firmware_release() {
 let a=RenderIds::new().unwrap();let id=RenderId::logical(&a).unwrap();
 let storage=id.storage().unwrap();let drops=Arc::new(AtomicU64::new(0));
 *storage.backing.lock()=Some(RenderBacking{drops:drops.clone()});
 storage.page_list.store(0x10000,Ordering::Release);
 let pin=id.pin(||{}).unwrap();
 assert!(a.reclaim_one(|_|panic!("pinned backing reached release proof")).is_none());
 assert_eq!(storage.page_list.load(Ordering::Acquire),0x10000);drop(pin);
 assert!(a.reclaim_one(|_|false).is_none());assert!(storage.backing.lock().is_some());
 let reclaimed=a.reclaim_one(|_|true).unwrap();
 assert_eq!(storage.page_list.load(Ordering::Acquire),0);
 assert!(storage.backing.lock().is_none());assert_eq!(id.slot(),NO_RENDER_ID);
 assert_eq!(drops.load(Ordering::Acquire),0);
 // The returned backing is not freed by reclaim_one or under its lock.
 let lock=a.state.lock();drop(reclaimed);drop(lock);
 assert_eq!(drops.load(Ordering::Acquire),1);
}
#[test] fn released_spare_follows_the_new_owner_without_a_client_mapping() {
 let a=RenderIds::new().unwrap();let old=host_pool(&a,1);
 let pin=old.pin_id().unwrap();*old.storage.spare.lock()=Some(Pages{layout:1});drop(pin);
 let reclaimed=a.reclaim_one(|_|true).unwrap();assert!(old.storage.spare.lock().is_none());
 let next=host_pool(&a,1);let pin=next.pin_id().unwrap();next.cache_reclaimed(reclaimed);
 assert!(next.storage.backing.lock().is_none());assert!(next.storage.spare.lock().is_some());
 assert_eq!(next.storage.page_list.load(Ordering::Acquire),0);drop(pin);
 let reclaimed=a.reclaim_one(|_|true).unwrap();let last=host_pool(&a,1);
 let _pin=last.pin_id().unwrap();last.cache_reclaimed(reclaimed);
 assert!(next.storage.spare.lock().is_none());assert!(last.storage.spare.lock().is_some());
}
#[test] fn mismatched_spare_is_not_installed() {
 let a=RenderIds::new().unwrap();let old=host_pool(&a,1);
 let pin=old.pin_id().unwrap();*old.storage.spare.lock()=Some(Pages{layout:1});drop(pin);
 let reclaimed=a.reclaim_one(|_|true).unwrap();let next=host_pool(&a,2);
 let _pin=next.pin_id().unwrap();next.cache_reclaimed(reclaimed);
 assert!(next.storage.backing.lock().is_none());assert!(next.storage.spare.lock().is_none());
}
#[test] fn reclamation_prefers_reusable_storage_over_empty_metadata() {
 let a=RenderIds::new().unwrap();let empty=host_pool(&a,1);let full=host_pool(&a,1);
 drop(empty.pin_id().unwrap());let empty_id=empty.id.slot();
 let pin=full.pin_id().unwrap();*full.storage.spare.lock()=Some(Pages{layout:1});drop(pin);
 let reclaimed=a.reclaim_one(|_|true).unwrap();
 assert!(reclaimed.spare.is_some());assert_eq!(empty.id.slot(),empty_id);
 assert_eq!(full.id.slot(),NO_RENDER_ID);
}
#[test] fn storage_only_reclaim_preserves_empty_identity() {
 let a=RenderIds::new().unwrap();let empty=host_pool(&a,1);
 drop(empty.pin_id().unwrap());let slot=empty.id.slot();
 assert!(a.reclaim_backing(|_|true).is_none());assert_eq!(empty.id.slot(),slot);
}
}
}
'''
# Keep original assertions readable while adapting the ownership-bearing API.
tests = re.sub(r'assert!\(!a\.reclaim_one\(([^;]*?)\)\);',
               r'assert!(a.reclaim_one(\1).is_none());', tests)
tests = re.sub(r'assert!\(a\.reclaim_one\(([^;]*?)\)(?<!\(\))\);',
               r'assert!(a.reclaim_one(\1).is_some());', tests)
harness = root / 'wrapper-tests.rs'
harness.write_text(prefix + body + tests)
binary = root / 'wrapper-tests'
subprocess.run(['rustc', '--edition=2021', '--test', str(harness), '-o', str(binary)], check=True)
run = subprocess.run([str(binary)], capture_output=True, text=True)
(root / 'wrapper-test.log').write_text(run.stdout + run.stderr)
(root / 'wrapper-test.json').write_text(json.dumps({'source_sha256':hashlib.sha256(source.read_bytes()).hexdigest(),
    'scope':'Actual identity methods; host Arc/Mutex/availability and allocation setup. No firmware or pool-memory lifetime proof.',
    'returncode':run.returncode},indent=2)+'\n')
print(run.stdout,end='')
raise SystemExit(run.returncode)
