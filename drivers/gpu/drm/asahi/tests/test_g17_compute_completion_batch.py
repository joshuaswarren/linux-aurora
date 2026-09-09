#!/usr/bin/env python3
"""Exercise the production one-pass G17P completion batch index."""
from pathlib import Path
import subprocess, tempfile

p = Path(__file__).resolve().parents[1]
resources = (p / "g17_resources.rs").read_text()
completion = (p / "g17_completion.rs").read_text()


def item(source, marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


batch_start = resources.index(
    "#[derive(Debug, Copy, Clone, PartialEq, Eq)]",
    resources.index("const G17P_COMPUTE_COMPLETION_QID_COUNT"),
)
batch_impl_start = resources.index("impl G17PComputeCompletionBatch {")
batch = resources[batch_start:batch_impl_start] + item(
    resources, "impl G17PComputeCompletionBatch {"
)
scan = item(resources, "    pub(crate) fn scan_compute_completion_batch(")
qid = item(completion, "pub(crate) const fn g17p_ksm_completion_queue_id(")
assert "Ordering::Acquire" in scan
assert ".scan_compute_completion_batch(&mut self.compute_completion_batch)" in (
    p / "g17_manager.rs"
).read_text()

source = r'''
#![allow(dead_code)]
use std::cell::Cell;
type Error=i32;type Result<T=()>=std::result::Result<T,Error>;
const EINVAL:i32=22;const EBUSY:i32=16;const ERANGE:i32=34;const EIO:i32=5;const EOVERFLOW:i32=75;
const G17P_COMPUTE_COMPLETION_RECORD_SIZE:usize=0x40;
const G17P_COMPUTE_COMPLETION_TIMESTAMP_MASK:u64=0x003f_ffff_ffff_ffff;
const G17P_COMPUTE_COMPLETION_DESCRIPTOR_MASK:u64=!1u64;
const G17P_COMPUTE_COMPLETION_QID_COUNT:usize=128;
mod g17_completion{const KSM_QID_MASK:u8=0x7f;
'''+qid+r'''
}
'''+batch+r'''
mod g17_initdata{pub const G17P_KSM_COMPLETION_CAPACITY:u32=0x800;}
thread_local!{static BORROWS:Cell<usize>=const{Cell::new(0)};}
struct Object{logical_size:usize,bytes:Vec<u8>}
impl Object{fn with_bytes_mut<T>(&mut self,f:impl FnOnce(&mut[u8])->Result<T>)->Result<T>{BORROWS.with(|n|n.set(n.get()+1));f(&mut self.bytes)}}
struct Objects{completion_ordinal_0:Object}
struct Owner{resources:Objects}
impl Owner{
'''+scan+r'''
}
fn owner()->Owner{Owner{resources:Objects{completion_ordinal_0:Object{logical_size:0x800*0x40,bytes:vec![0;0x800*0x40]}}}}
fn put(raw:&mut[u8],slot:usize,at:usize,value:u64){let offset=slot*0x40+at;raw[offset..offset+8].copy_from_slice(&value.to_le_bytes());}
fn record(raw:&mut[u8],slot:usize,qid:u8,descriptor:u64,a_start:u64,b_start:u64,b_end:u64){put(raw,slot,0,(u64::from(qid)<<9)|0x10);put(raw,slot,0x10,a_start);put(raw,slot,0x20,b_start);put(raw,slot,0x28,b_end);put(raw,slot,0x30,descriptor|1);put(raw,slot,0x38,0x7000_0000+u64::from(qid));}
#[test]fn fifty_eight_coalesced_qids_share_one_full_ring_borrow(){
 let mut owner=owner();let mut batch=G17PComputeCompletionBatch::new();
 for qid in 4u8..62{let descriptor=u64::from(qid)<<12;batch.request(qid,descriptor,100).unwrap();record(&mut owner.resources.completion_ordinal_0.bytes,usize::from(qid),qid,descriptor,u64::from(qid),u64::from(qid)+1,u64::from(qid)+200);}
 BORROWS.with(|n|n.set(0));owner.scan_compute_completion_batch(&mut batch).unwrap();assert_eq!(BORROWS.with(Cell::get),1);
 for qid in 4u8..62{let descriptor=u64::from(qid)<<12;let found=batch.take(qid,descriptor,100).unwrap().unwrap();assert_eq!(found.timestamps,[u64::from(qid),u64::from(qid)+200]);assert_eq!(found.interval,Some([u64::from(qid)+1,u64::from(qid)+200]));}
}
#[test]fn newest_exact_record_wins_across_physical_ring_wrap(){
 let mut owner=owner();let mut batch=G17PComputeCompletionBatch::new();let descriptor=0x7f000;
 batch.request(127,descriptor,100).unwrap();record(&mut owner.resources.completion_ordinal_0.bytes,2047,127,descriptor,10,20,200);record(&mut owner.resources.completion_ordinal_0.bytes,0,127,descriptor,11,21,201);record(&mut owner.resources.completion_ordinal_0.bytes,1,127,0xdead000,99,100,999);
 owner.scan_compute_completion_batch(&mut batch).unwrap();let found=batch.take(127,descriptor,100).unwrap().unwrap();assert_eq!(found.timestamps,[11,201]);assert_eq!(found.interval,Some([21,201]));
}
#[test]fn stale_partial_foreign_and_tag_only_records_do_not_retire(){
 let mut owner=owner();let mut batch=G17PComputeCompletionBatch::new();let descriptor=0x41000;let tags=0x3ffu64<<54;
 batch.request(65,descriptor,100).unwrap();record(&mut owner.resources.completion_ordinal_0.bytes,0,65,descriptor,1,2,99);record(&mut owner.resources.completion_ordinal_0.bytes,1,65,0x99000,9,10,900);record(&mut owner.resources.completion_ordinal_0.bytes,2,65,descriptor,tags,tags,tags);record(&mut owner.resources.completion_ordinal_0.bytes,3,65,descriptor,tags|12,tags|30,tags|140);
 owner.scan_compute_completion_batch(&mut batch).unwrap();let found=batch.take(65,descriptor,100).unwrap().unwrap();assert_eq!(found.timestamps,[12,140]);assert_eq!(found.interval,Some([30,140]));
}
#[test]fn publication_after_snapshot_is_seen_only_by_a_later_scan(){
 let mut owner=owner();let mut batch=G17PComputeCompletionBatch::new();let descriptor=0x4000;
 batch.request(4,descriptor,0).unwrap();owner.scan_compute_completion_batch(&mut batch).unwrap();record(&mut owner.resources.completion_ordinal_0.bytes,7,4,descriptor,2,3,8);assert_eq!(batch.take(4,descriptor,0).unwrap(),None);
 batch.clear();batch.request(4,descriptor,0).unwrap();owner.scan_compute_completion_batch(&mut batch).unwrap();assert_eq!(batch.take(4,descriptor,0).unwrap().unwrap().timestamps,[2,8]);
}
#[test]fn duplicate_invalid_and_changed_requests_fail_closed(){
 let mut batch=G17PComputeCompletionBatch::new();assert_eq!(batch.request(4,0,0),Err(EINVAL));assert_eq!(batch.request(200,0x1000,0),Err(EINVAL));batch.request(4,0x1000,7).unwrap();assert_eq!(batch.request(4,0x1000,7),Err(EBUSY));assert_eq!(batch.take(4,0x2000,7),Err(EIO));assert_eq!(batch.take(4,0x1000,8),Err(EIO));
}
'''

with tempfile.TemporaryDirectory() as tmp:
    src = Path(tmp) / "test.rs"
    exe = Path(tmp) / "test"
    src.write_text(source)
    subprocess.run(
        ["rustc", "--edition=2021", "--test", str(src), "-o", str(exe)],
        check=True,
    )
    subprocess.run([str(exe), "--test-threads=1"], check=True)

print("PASS one-pass G17P completion batch: 58 QIDs, wrap/newest, acquire snapshot, exact identity")
