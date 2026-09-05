#!/usr/bin/env python3
"""Exercise the actual PM counter transaction; mock only GEM/VMap storage.

Also lock down the real outer-publication callback ordering. No firmware or
hardware is involved; this does not qualify page recycling on the GPU.
"""
from pathlib import Path
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
source = (root / "g17_resources.rs").read_text()
manager = (root / "g17_manager.rs").read_text()


def block(pattern):
    match = re.search(pattern, source, re.M)
    assert match, pattern
    end = source.index("{", match.start()) + 1
    depth = 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[match.start():end]


body = "\n".join([
    "#![allow(dead_code)]",
    "type Result<T = ()> = core::result::Result<T, i32>;",
    "const EOVERFLOW: i32 = 75; const ERANGE: i32 = 34; const EIO: i32 = 5;",
    "#[derive(Debug, Copy, Clone, PartialEq, Eq)]",
    block(r"^struct G17PPmSubmittedOperationPlan\s+"),
    block(r"^fn prepare_g17p_pm_submitted_operation\("),
    block(r"^fn put_u32\("),
    block(r"^pub\(crate\) struct G17PPmSubmittedOperation<'a>\s+"),
    block(r"^impl G17PPmSubmittedOperation<'_>\s+"),
    "struct Layout { counter: usize }",
    "struct MappedObject { object: TestGem, object_offset: usize, logical_size: usize }",
    "struct G17PParameterManagement { backing: MappedObject, layout: Layout }",
    "impl G17PParameterManagement {",
    block(r"^    fn prepare_submitted_operation\("),
    "}",
    r'''
use std::cell::{Cell, UnsafeCell};
struct TestGem { bytes: UnsafeCell<Vec<u8>>, fail: bool, maps: Cell<usize> }
impl TestGem {
    fn size(&self) -> usize { unsafe { (&*self.bytes.get()).len() } }
    fn bytes(&self) -> Vec<u8> { unsafe { (&*self.bytes.get()).clone() } }
    fn vmap(&self) -> Result<kernel::drm::gem::shmem::VMapRef<'_, gem::AsahiObject, u8>> {
        self.maps.set(self.maps.get() + 1);
        if self.fail { return Err(EIO); }
        Ok(kernel::drm::gem::shmem::VMapRef { gem: self, marker: core::marker::PhantomData })
    }
}
mod gem { pub struct AsahiObject; }
mod kernel { pub mod drm { pub mod gem { pub mod shmem {
    pub struct VMapRef<'a, T, U> {
        pub gem: &'a crate::TestGem,
        pub marker: core::marker::PhantomData<(T, U)>,
    }
    impl<T> VMapRef<'_, T, u8> {
        pub fn as_ptr(&self) -> *const u8 { unsafe { (&*self.gem.bytes.get()).as_ptr() } }
        pub fn as_mut_ptr(&self) -> *mut u8 { unsafe { (&mut *self.gem.bytes.get()).as_mut_ptr() } }
    }
}}}}
fn pm(size: usize, object_offset: usize, logical_size: usize, counter: usize, value: u32)
    -> G17PParameterManagement
{
    let mut bytes = vec![0xa5; size];
    if let Some(at) = object_offset.checked_add(counter) {
        if let Some(end) = at.checked_add(4) {
            if end <= size { bytes[at..end].copy_from_slice(&value.to_le_bytes()); }
        }
    }
    G17PParameterManagement {
        backing: MappedObject {
            object: TestGem { bytes: UnsafeCell::new(bytes), fail: false, maps: Cell::new(0) },
            object_offset, logical_size,
        },
        layout: Layout { counter },
    }
}
#[test]
fn actual_helper_checks_exact_span_and_wrapping_u32() {
    for (previous, next) in [(0,1), (1,2), (2,3), (u32::MAX - 1,u32::MAX), (u32::MAX,0)] {
        assert_eq!(prepare_g17p_pm_submitted_operation(&previous.to_le_bytes(), 0),
                   Some(G17PPmSubmittedOperationPlan { offset: 0, next }));
    }
    for size in 0..4 {
        assert_eq!(prepare_g17p_pm_submitted_operation(&vec![0; size], 0), None);
    }
    assert_eq!(prepare_g17p_pm_submitted_operation(&[0; 8], 5), None);
    assert_eq!(prepare_g17p_pm_submitted_operation(&[0; 8], usize::MAX), None);
}
#[test]
fn native_one_two_three_commit_only_four_counter_bytes_in_owned_subview() {
    let mut p = pm(256, 32, 128, 64, 0);
    for next in 1..=3 {
        let before = p.backing.object.bytes();
        let operation = p.prepare_submitted_operation().unwrap();
        assert_eq!(operation.next(), next);
        assert_eq!(operation.vmap.gem.bytes(), before, "preparation must not publish");
        operation.commit();
        let after = p.backing.object.bytes();
        assert_eq!(&after[..96], &before[..96]);
        assert_eq!(&after[96..100], &next.to_le_bytes());
        assert_eq!(&after[100..], &before[100..]);
    }
}
#[test]
fn actual_transaction_wraps_without_ordinal_or_generation_assignment() {
    let mut p = pm(32, 8, 16, 4, u32::MAX);
    let operation = p.prepare_submitted_operation().unwrap();
    assert_eq!(operation.next(), 0);
    operation.commit();
    let operation = p.prepare_submitted_operation().unwrap();
    assert_eq!(operation.next(), 1);
    operation.commit();
    assert_eq!(&p.backing.object.bytes()[12..16], &1u32.to_le_bytes());
}
#[test]
fn dropping_prepared_transaction_after_later_failure_publishes_nothing() {
    let mut p = pm(32, 8, 16, 4, 19);
    let before = p.backing.object.bytes();
    let operation = p.prepare_submitted_operation().unwrap();
    assert_eq!(operation.next(), 20);
    drop(operation); // Outer mapping/context-bind failure before the callback.
    assert_eq!(p.backing.object.bytes(), before);
    let retry = p.prepare_submitted_operation().unwrap();
    assert_eq!(retry.next(), 20);
    retry.commit();
    assert_eq!(&p.backing.object.bytes()[12..16], &20u32.to_le_bytes());
}
#[test]
fn every_invalid_span_or_mapping_is_transactional() {
    for (size, base, logical, counter, expected, maps) in [
        (16, 0, 3, 0, ERANGE, 0),
        (16, 0, 16, usize::MAX, EOVERFLOW, 0),
        (16, usize::MAX, 16, 4, EOVERFLOW, 0),
        (16, 14, 16, 0, ERANGE, 1),
        (16, 0, 16, 13, ERANGE, 0),
    ] {
        let mut p = pm(size, base, logical, counter, 0);
        let before = p.backing.object.bytes();
        assert_eq!(p.prepare_submitted_operation().err(), Some(expected));
        assert_eq!(p.backing.object.bytes(), before);
        assert_eq!(p.backing.object.maps.get(), maps);
    }
    let mut p = pm(16, 0, 16, 4, 17);
    p.backing.object.fail = true;
    let before = p.backing.object.bytes();
    assert_eq!(p.prepare_submitted_operation().err(), Some(EIO));
    assert_eq!(p.backing.object.bytes(), before);
}
''',
])

with tempfile.TemporaryDirectory(prefix="g17-pm-submitted-") as temp:
    binary = Path(temp) / "tests"
    subprocess.run(["rustc", "--edition=2021", "--test", "-", "-o", str(binary)],
                   input=body, text=True, check=True)
    subprocess.run([str(binary)], check=True)

# Check the actual publication implementation, not a second mock algorithm.
# All fallible preconditions (including optional context-alias installation)
# must finish before the one-shot callback, and neither producer may precede it.
publish = block(r"^    pub\(crate\) fn publish_partial_opening_pair<F, G>\(")
assert publish.count("commit_submitted_operation();") == 1
at = publish.index("commit_submitted_operation();")
for earlier in ["object.vmap()?", "uat.bind(vm)?", "install_t8140_render_mcache_context_alias",
                "install_t8140_partial_opening_empty_high_roots"]:
    assert publish.index(earlier) < at, earlier
assert "?" not in publish[at:], "fallible operation after committed count"
assert "return Err" not in publish[at:]
for later in ["state[fragment_producer_offset..", "publish_late_tiling_state();",
              "state[tiling_producer_offset.."]:
    assert publish.index(later) > at, later
assert "G: FnOnce()" in publish
prepare = manager.index(".prepare_pm_submitted_operation()")
publish_at = manager.index(".publish_partial_opening_pair(", prepare)
commit = manager.index("operation.commit();", publish_at)
assert prepare < publish_at < commit
assert manager.count("operation.commit();") == 1
print("PM submitted-operation transaction and actual publication ordering PASS")
