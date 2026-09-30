#!/usr/bin/env python3
"""Compile the production VM mutation journal with bounded-history races."""

from pathlib import Path
import subprocess
import tempfile


source = (Path(__file__).resolve().parents[1] / "mmu.rs").read_text()


def item(prefix):
    start = source.index(prefix)
    opening = source.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


program = "\n".join([
    "use core::ops::Range;",
    "use std::sync::Arc; use core::sync::atomic::{AtomicU64,AtomicU32,Ordering};",
    item("struct MappingValidationState {"),
    item("impl MappingValidationState {"),
    next(line for line in source.splitlines() if line.startswith("struct MappingValidationMutation(")),
    item("impl MappingValidationMutation {"),
    item("impl Drop for MappingValidationMutation {"),
    "const UAT_PGSZ: usize = 16384;",
    next(line for line in source.splitlines()
         if line.startswith("const MAPPING_VALIDATION_HISTORY:")),
    "#[derive(Clone, Copy)]",
    item("struct MappingValidationChange"),
    "type KBox<T> = Box<T>;",
    "struct VmInner { mapping_validation_epoch: Option<u64>, "
    "mapping_validation: Option<Arc<MappingValidationState>>, "
    "mapping_validation_changes: Option<KBox<[Option<MappingValidationChange>; MAPPING_VALIDATION_HISTORY]>> }",
    "impl VmInner {",
    item("fn note_mapping_validation_change("),
    "}",
    "struct Lock(std::cell::RefCell<VmInner>);",
    "impl Lock { fn exec_lock(&self, _: Option<()>, _: bool) "
    "-> Result<std::cell::Ref<'_, VmInner>, ()> { Ok(self.0.borrow()) } }",
    "struct Vm { inner: Lock, mapping_validation: Option<Arc<MappingValidationState>> }",
    "impl Vm {",
    item("pub(crate) fn mapping_validation_epoch("),
    item("pub(crate) fn mapping_changes_since("),
    "}",
    r'''
fn fixture() -> Vm {
    let state = Arc::new(MappingValidationState::new());
    Vm { mapping_validation: Some(state.clone()), inner: Lock(std::cell::RefCell::new(VmInner {
        mapping_validation_epoch: Some(0),
        mapping_validation: Some(state),
        mapping_validation_changes: Some(Box::new([None; MAPPING_VALIDATION_HISTORY])),
    })) }
}
#[test]
fn empty_and_disjoint_changes_keep_the_proof() {
    let vm = fixture();
    assert_eq!(vm.mapping_changes_since(0, |_| true), Some((0, false)));
    vm.inner.0.borrow_mut().note_mapping_validation_change(0x10001..0x10002);
    assert_eq!(vm.mapping_changes_since(0, |range| range.start < 0x2000 && range.end > 0x1000),
        Some((1, false)));
    assert_eq!(vm.mapping_changes_since(0, |range| range.start < 0x10001 && range.end > 0x10000),
        Some((1, true)));
}
#[test]
fn failed_attempt_and_guard_mapping_both_invalidate() {
    let vm = fixture();
    // A mutation is recorded before a page-table operation that may fail.
    vm.inner.0.borrow_mut().note_mapping_validation_change(0x10001..0x10002);
    assert_eq!(vm.mapping_changes_since(0, |range| range.start < 0x10002 && range.end > 0x10001),
        Some((1, true)));
    // A TVB guard is a negative predicate; a leaf reaching its adjacent tail
    // invalidates even when the caller's byte range ended before the tail.
    assert_eq!(vm.mapping_changes_since(0, |range| range.start < 0x14000 && range.end > 0x10002),
        Some((1, true)));
}
#[test]
fn stale_or_corrupt_history_never_returns_clean() {
    let vm = fixture();
    for epoch in 0..=MAPPING_VALIDATION_HISTORY {
        let address = 1000 + epoch as u64;
        vm.inner.0.borrow_mut().note_mapping_validation_change(address..address + 1);
    }
    assert_eq!(vm.mapping_changes_since(0, |_| false), None);
    assert_eq!(vm.mapping_changes_since(1, |_| false), Some((129, false)));
    vm.inner.0.borrow_mut().mapping_validation_changes.as_mut().unwrap()[17] = None;
    assert_eq!(vm.mapping_changes_since(1, |_| false), None);
}
#[test]
fn invalid_range_or_epoch_overflow_disables_reuse() {
    let vm = fixture();
    vm.inner.0.borrow_mut().note_mapping_validation_change(5..5);
    assert_eq!(vm.mapping_changes_since(0, |_| false), None);
    let vm = fixture();
    vm.inner.0.borrow_mut().mapping_validation_epoch = Some(u64::MAX);
    assert_eq!(vm.mapping_changes_since(u64::MAX, |_| true), Some((u64::MAX, false)));
    vm.inner.0.borrow_mut().note_mapping_validation_change(1..2);
    assert_eq!(vm.mapping_changes_since(u64::MAX, |_| false), None);
}
#[test]
fn snapshot_never_takes_the_exec_lock_and_refuses_in_progress_mutations() {
    let vm=fixture();
    let mut writer=vm.inner.0.borrow_mut();
    assert_eq!(vm.mapping_validation_epoch(),Some(0));
    let change=writer.note_mapping_validation_change(0x10000..0x14000);
    // The old method panics on this second RefCell borrow. The new snapshot
    // neither waits for the writer nor claims its partial mapping is valid.
    assert_eq!(vm.mapping_validation_epoch(),None);
    drop(change);
    assert_eq!(vm.mapping_validation_epoch(),Some(1));
}
#[test]
fn nested_leaf_mutations_publish_only_after_the_outer_scope() {
    let vm=fixture();let mut writer=vm.inner.0.borrow_mut();
    let outer=writer.note_mapping_validation_change(0x10000..0x14000);
    let inner=writer.note_mapping_validation_change(0x18000..0x1c000);
    drop(inner);assert_eq!(vm.mapping_validation_epoch(),None);
    drop(outer);assert_eq!(vm.mapping_validation_epoch(),Some(2));
}
#[test]
fn partial_failure_invalidates_old_proof_and_releases_writer_scope() {
    fn attempt(writer:&mut VmInner)->Result<(),()> {
        let _change=writer.note_mapping_validation_change(0x10000..0x14000);
        Err(())
    }
    let vm=fixture();let old=vm.mapping_validation_epoch();
    assert!(attempt(&mut vm.inner.0.borrow_mut()).is_err());
    assert_ne!(old,vm.mapping_validation_epoch());
    assert_eq!(vm.mapping_validation_epoch(),Some(1));
    assert_eq!(vm.mapping_changes_since(0, |_|true),Some((1,true)));
}
#[test]
fn acquiring_a_published_epoch_observes_prior_leaf_writes() {
    let state=Arc::new(MappingValidationState::new());
    let leaf=Arc::new(AtomicU64::new(0));
    let writer_state=state.clone();let writer_leaf=leaf.clone();
    let writer=std::thread::spawn(move||{
        for epoch in 1..10000 {
            let _change=MappingValidationMutation::new(Some(writer_state.clone()));
            writer_state.record(Some(epoch));
            writer_leaf.store(epoch,Ordering::Relaxed);
        }
    });
    while !writer.is_finished() {
        if let Some(epoch)=state.snapshot() {
            assert!(leaf.load(Ordering::Relaxed)>=epoch);
        }
    }
    writer.join().unwrap();assert_eq!(state.snapshot(),Some(9999));
}
''',
])

# All seven mutation entry points retain the invalidation across actual leaf
# work. map_io explicitly unlocks on two exits, so both release the
# version guard first; ordinary lexical exits drop guards before exec guards.
assert source.count('let _validation_change = ') == 6
assert source.count('let validation_change = ') == 1
assert source.count('drop(validation_change);') == 2
assert 'drop(validation_change);\n            core::mem::drop(inner);' in source
assert 'drop(validation_change);\n        core::mem::drop(inner);' in source

with tempfile.TemporaryDirectory(prefix="g17p-mapping-journal-") as directory:
    path = Path(directory)
    (path / "journal.rs").write_text(program)
    subprocess.run(["rustc", "--edition=2021", "--test", str(path / "journal.rs"),
                    "-o", str(path / "journal-tests")], check=True)
    subprocess.run([str(path / "journal-tests")], check=True)
