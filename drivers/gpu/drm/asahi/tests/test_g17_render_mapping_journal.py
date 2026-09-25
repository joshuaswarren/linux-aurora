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
    "const UAT_PGSZ: usize = 16384;",
    next(line for line in source.splitlines()
         if line.startswith("const MAPPING_VALIDATION_HISTORY:")),
    "#[derive(Clone, Copy)]",
    item("struct MappingValidationChange"),
    "struct VmInner { mapping_validation_epoch: Option<u64>, "
    "mapping_validation_changes: [Option<MappingValidationChange>; MAPPING_VALIDATION_HISTORY] }",
    "impl VmInner {",
    item("fn note_mapping_validation_change("),
    "}",
    "struct Lock(std::cell::RefCell<VmInner>);",
    "impl Lock { fn exec_lock(&self, _: Option<()>, _: bool) "
    "-> Result<std::cell::Ref<'_, VmInner>, ()> { Ok(self.0.borrow()) } }",
    "struct Vm { inner: Lock }",
    "impl Vm {",
    item("pub(crate) fn mapping_changes_since("),
    "}",
    r'''
fn fixture() -> Vm {
    Vm { inner: Lock(std::cell::RefCell::new(VmInner {
        mapping_validation_epoch: Some(0),
        mapping_validation_changes: [None; MAPPING_VALIDATION_HISTORY],
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
    vm.inner.0.borrow_mut().mapping_validation_changes[17] = None;
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
''',
])

with tempfile.TemporaryDirectory(prefix="g17p-mapping-journal-") as directory:
    path = Path(directory)
    (path / "journal.rs").write_text(program)
    subprocess.run(["rustc", "--edition=2021", "--test", str(path / "journal.rs"),
                    "-o", str(path / "journal-tests")], check=True)
    subprocess.run([str(path / "journal-tests")], check=True)
