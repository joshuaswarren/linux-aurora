#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Run the production GPU planning and uncertain-ownership state machines."""
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parents[4]
compiler = shutil.which("rustc")
if not compiler:
    print("1..0 # SKIP rustc is required")
    raise SystemExit(4)
source = root / "drivers/gpu/drm/asahi/mmu/sptm.rs"
with tempfile.TemporaryDirectory(prefix="apif-gpu-") as directory:
    temp = Path(directory)
    shutil.copyfile(source, temp / "planning.rs")
    (temp / "test.rs").write_text(r'''
#![allow(dead_code)]
mod planning;
use planning::*;
#[test]
fn rejects_invalid_whole_requests() {
    for pages in [MapPages::Slice(&[]), MapPages::Slice(&[PAGE, PAGE+1]),
                  MapPages::Contiguous{physical:u64::MAX-PAGE+1,count:2}] {
        assert!(MapPlan::new(Space::Private(0),0,pages,Protection::GpuReadWrite).is_err());
    }
    assert!(MapPlan::new(Space::Private(0),1,MapPages::Slice(&[PAGE]),Protection::GpuReadWrite).is_err());
    assert!(MapPlan::new(Space::Private(0xff),0,MapPages::Slice(&[PAGE]),Protection::GpuReadWrite).is_err());
    assert!(MapPlan::new(Space::Private(0),1<<42,MapPages::Slice(&[PAGE]),Protection::GpuReadWrite).is_err());
    assert!(MapPlan::new(Space::Private(0),0,MapPages::Slice(&[PAGE]),Protection::FirmwareMmioReadWrite).is_err());
}
#[test]
fn bounded_segments_cross_tables_without_losing_pages() {
    let va=(1<<25)-PAGE;
    let ops:Vec<_>=MapPlan::new(Space::Private(0),va,
        MapPages::Contiguous{physical:PAGE,count:130},Protection::GpuReadWrite).unwrap().collect();
    assert_eq!(ops[0].args[2],1);
    assert_eq!(ops.iter().filter(|o|o.opcode==2 && o.args[2]==2).count(),2);
    let maps:Vec<_>=ops.iter().filter(|o|o.opcode==3).collect();
    assert_eq!(maps.iter().map(|o|o.args[3]).collect::<Vec<_>>(),vec![1,64,64,1]);
    assert_eq!(maps[3].args[2],PAGE*130);
    let unmaps:Vec<_>=UnmapPlan::new(Space::Private(0),va,130).unwrap().collect();
    assert_eq!(unmaps.iter().map(|o|o.args[2]).collect::<Vec<_>>(),vec![1,64,64,1]);
}
#[test]
fn repeated_backing_is_not_coalesced() {
    let ops:Vec<_>=MapPlan::new(Space::Private(1),0,
        MapPages::Repeated{physical:PAGE,count:3},Protection::GpuReadOnly).unwrap().collect();
    let maps:Vec<_>=ops.iter().filter(|o|o.opcode==3).collect();
    assert_eq!(maps.len(),3);
    assert!(maps.iter().all(|o|o.args[2]==PAGE && o.args[3]==1));
}
#[test]
fn shared_space_does_not_create_private_roots() {
    let ops:Vec<_>=MapPlan::new(Space::Global,GLOBAL_RANGE.start,
        MapPages::Slice(&[PAGE]),Protection::FirmwareMmioReadOnly).unwrap().collect();
    assert_eq!(ops[0].args[2],2);
    assert_eq!(ops[1].opcode,9);
    assert_eq!(ops[1].args[0],0xff);
}
#[test]
fn failed_bind_retains_claim_and_refuses_reuse() {
    let mut state=PrivateState::new(1).unwrap();
    assert_eq!(state.bind(2, |_|Err(7)),Err(TransitionError::Provider(7)));
    assert_eq!(state.state(),ContextState::Uncertain(Some(2)));
    assert!(state.space().is_err());
    assert_eq!(state.bind(3, |_|Ok::<_,i32>(())),Err(TransitionError::Invalid));
    assert_eq!(state.destroy(|_|Ok::<_,i32>(())),Err(TransitionError::Invalid));
}
#[test]
fn uncertain_unbind_and_destroy_never_release_identity() {
    let mut state=PrivateState::new(1).unwrap();
    state.bind(63, |_|Ok::<_,i32>(())).unwrap();
    assert_eq!(state.unbind(|_|Err(8)),Err(TransitionError::Provider(8)));
    assert_eq!(state.state(),ContextState::Uncertain(Some(63)));
    let mut state=PrivateState::new(2).unwrap();
    assert_eq!(state.destroy(|_|Err(9)),Err(TransitionError::Provider(9)));
    assert_eq!(state.state(),ContextState::Uncertain(None));
}
#[test]
fn failed_batch_receipt_keeps_possible_published_backing() {
    let table=Operation::new(2,[0;5]);let map=Operation::new(3,[0;5]);
    assert!(!may_have_mapped(&[table,map],0));
    assert!(may_have_mapped(&[table,map],1));
    assert!(may_have_mapped(&[table],2));
}
''')
    subprocess.run([compiler,"--edition=2021","--test",str(temp / "test.rs"),"-o",str(temp / "tests")],check=True)
    subprocess.run([str(temp / "tests")],check=True)
