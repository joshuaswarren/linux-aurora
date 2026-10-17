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
    let mut state=PrivateState::new(1,false).unwrap();
    assert_eq!(state.bind(2, |_|Err(7)),Err(TransitionError::Provider(7)));
    assert_eq!(state.state(),ContextState::Uncertain(Some(2)));
    assert!(state.space().is_err());
    assert_eq!(state.bind(3, |_|Ok::<_,i32>(())),Err(TransitionError::Invalid));
    assert_eq!(state.destroy(|_|Ok::<_,i32>(())),Err(TransitionError::Invalid));
}
#[test]
fn uncertain_unbind_and_destroy_never_release_identity() {
    let mut state=PrivateState::new(1,false).unwrap();
    state.bind(63, |_|Ok::<_,i32>(())).unwrap();
    assert_eq!(state.unbind(|_|Err(8)),Err(TransitionError::Provider(8)));
    assert_eq!(state.state(),ContextState::Uncertain(Some(63)));
    let mut state=PrivateState::new(2,false).unwrap();
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
#[test] fn legacy_handle_bounds_stay_reserved() {
 assert!(PrivateState::new(254,false).is_ok());
 for h in [255,256,4095,4096,u64::MAX] { assert!(PrivateState::new(h,false).is_err()); }
}
#[test] fn wide_handles_preserve_global_sentinel() {
 for h in [0,254,256,4095] { assert_eq!(PrivateState::new(h,true).unwrap().handle() as u64,h); }
 for h in [255,4096,u64::MAX] { assert!(PrivateState::new(h,true).is_err()); }
}
#[test] fn wide_handle_survives_every_lifecycle_command() {
 let mut state=PrivateState::new(4095,true).unwrap();
 state.bind(63,|op| {assert_eq!(op.args[0],4095);assert_eq!(op.opcode,5);Ok::<(),()>(())}).unwrap();
 state.unbind(|op| {assert_eq!(op.args[0],4095);assert_eq!(op.opcode,6);Ok::<(),()>(())}).unwrap();
 assert_eq!(state.destroy(|op| {assert_eq!(op.args[0],4095);assert_eq!(op.opcode,7);op.result=2;Ok::<(),()>(())}),Ok(2));
 assert_eq!(state.state(),ContextState::Destroyed);assert!(state.space().is_err());
}
#[test] fn failed_destroy_never_authorizes_identity_reuse() {
 let mut state=PrivateState::new(256,true).unwrap();
 assert_eq!(state.destroy(|_| Err::<(),_>(9)),Err(TransitionError::Provider(9)));
 assert!(state.space().is_err());assert_eq!(state.state(),ContextState::Uncertain(None));
 assert_eq!(state.destroy(|_| panic!("uncertain identity submitted again")),Err::<u64,TransitionError<()>>(TransitionError::Invalid));
}
#[test] fn bound_space_cannot_destroy_donors() {
 let mut state=PrivateState::new(256,true).unwrap();state.bind(2,|_|Ok::<(),()>(())).unwrap();
 assert_eq!(state.destroy(|_|panic!("bound destroy submitted")),Err::<u64,TransitionError<()>>(TransitionError::Invalid));
}
#[test] fn donated_tables_require_private_aligned_frames_and_correct_levels() {
 for (space,va,level,frame) in [(Space::Global,0,1,PAGE),(Space::Private(255),0,1,PAGE),(Space::Private(256),0,0,PAGE),(Space::Private(256),0,3,PAGE),(Space::Private(256),0,1,0),(Space::Private(256),0,1,PAGE+1),(Space::Private(256),PAGE,1,PAGE),(Space::Private(256),1<<42,2,PAGE)] {
  assert!(donated_table(space,va,level,frame).is_err());
 }
 for (va,level) in [(0,1),(1<<36,1),(1<<25,2)] {
  let op=donated_table(Space::Private(4095),va,level,PAGE).unwrap();assert_eq!(op.args,[4095,va,level,PAGE,0,0]);
 }
}
#[test] fn donation_leaf_plan_emits_no_heap_tables_and_keeps_batching() {
 let ops:Vec<_>=MapPlan::without_tables(Space::Private(4095),0,MapPages::Contiguous{physical:PAGE,count:129},Protection::GpuReadWrite).unwrap().collect();
 assert_eq!(ops.len(),3);assert!(ops.iter().all(|o|o.opcode==3&&o.args[0]==4095));
 assert_eq!(ops.iter().map(|o|o.args[3]).collect::<Vec<_>>(),[64,64,1]);
}
#[test] fn legacy_plan_keeps_heap_table_operations() {
 let ops:Vec<_>=MapPlan::new(Space::Private(254),0,MapPages::Contiguous{physical:PAGE,count:1},Protection::GpuReadOnly).unwrap().collect();
 assert_eq!(ops.iter().map(|o|o.opcode).collect::<Vec<_>>(),[2,2,3]);
}
''')
    subprocess.run([compiler,"--edition=2021","--test",str(temp / "test.rs"),"-o",str(temp / "tests")],check=True)
    subprocess.run([str(temp / "tests")],check=True)

# Compile the production donation-ownership decisions with a counted release
# stand-in. In particular, destroy success covers installed tables only.
page_source = (root / "drivers/gpu/drm/asahi/mmu/sptm_pgtable.rs").read_text()
start = page_source.index("#[derive(Debug, PartialEq, Eq)]\nenum DonatedPage")
end = page_source.index("// SAFETY: An exclusively owned page", start)
with tempfile.TemporaryDirectory(prefix="apif-gpu-ownership-") as directory:
    temp = Path(directory)
    (temp / "test.rs").write_text(
        "type Result<T> = core::result::Result<T, i32>;\n" + page_source[start:end] + r'''
#[test] fn completed_receipts_keep_their_exact_meaning() {
    assert_eq!(donated_page_receipt(Some(0), Err(5)), DonatedPage::Taken);
    assert_eq!(donated_page_receipt(Some(1), Err(5)), DonatedPage::Unused);
}
#[test] fn failed_transition_only_frees_a_confirmed_guest_page() {
    assert_eq!(donated_page_receipt(None, Ok(0)), DonatedPage::Unused);
    for state in [Ok(1), Ok(2), Ok(99), Err(5)] {
        assert_eq!(donated_page_receipt(None, state), DonatedPage::Uncertain);
    }
}
#[test] fn undefined_positive_receipt_never_proves_installation() {
    for receipt in [2, 3, u64::MAX] {
        assert_eq!(donated_page_receipt(Some(receipt), Ok(0)), DonatedPage::Uncertain);
    }
}
#[test] fn successful_destroy_of_a_known_table_keeps_the_fast_path() {
    assert!(donated_page_returned(0, false, || panic!("unnecessary provider query")));
}
#[test] fn zero_destroy_receipt_cannot_free_a_quarantined_failed_installation() {
    for state in [Ok(1), Ok(2), Ok(99), Err(5)] {
        assert!(!donated_page_returned(0, true, || state));
    }
    assert!(donated_page_returned(0, true, || Ok(0)));
}
#[test] fn partial_destroy_queries_each_page_and_keeps_unknown_ownership() {
    assert!(donated_page_returned(2, false, || Ok(0)));
    for state in [Ok(1), Ok(2), Err(5)] {
        assert!(!donated_page_returned(2, false, || state));
    }
}
#[test] fn mixed_failure_prefix_never_releases_the_quarantined_page() {
    let dispositions = [donated_page_receipt(Some(0),Ok(0)),
        donated_page_receipt(None,Ok(2)), donated_page_receipt(None,Ok(0))];
    let mut freed = Vec::new();
    let mut retained = Vec::new();
    for (index, disposition) in dispositions.into_iter().enumerate() {
        match disposition {
            DonatedPage::Unused => freed.push(index),
            DonatedPage::Taken => if donated_page_returned(0,false,||panic!()) {freed.push(index)},
            DonatedPage::Uncertain => if donated_page_returned(0,true,||Ok(2)) {freed.push(index)} else {retained.push(index)},
        }
    }
    assert_eq!(freed,[0,2]); assert_eq!(retained,[1]);
}
''')
    subprocess.run([compiler,"--edition=2021","--test",str(temp / "test.rs"),"-o",str(temp / "tests")],check=True)
    subprocess.run([str(temp / "tests")],check=True)
