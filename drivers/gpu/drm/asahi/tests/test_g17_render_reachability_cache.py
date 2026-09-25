#!/usr/bin/env python3
"""Compile the production cache and its unit tests without kernel-only resources."""

from pathlib import Path
import subprocess
import tempfile


source = (Path(__file__).resolve().parents[1] / "g17_resources.rs").read_text()


def item(prefix):
    start = source.index(prefix)
    opening = source.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


tests = [
    "retained_reachability_keeps_both_command_generations",
    "retained_reachability_rejects_each_changed_vm_input",
    "retained_reachability_does_not_cache_overlapping_or_unavailable_epochs",
    "retained_reachability_topology_invalidation_clears_both_generations",
]
constant = next(line for line in source.splitlines()
                if line.startswith("pub(crate) const G17P_RENDER_MAX_IN_FLIGHT:"))
program = "\n".join([
    constant,
    "#[derive(Debug, Copy, Clone, PartialEq, Eq)]",
    item("struct G17PRenderReachabilityKey"),
    item("struct G17PRenderReachabilityCache"),
    item("impl G17PRenderReachabilityCache"),
    item("fn reachability_test_key"),
    *("#[test]\n" + item("fn " + name) for name in tests),
])

# Exercise the actual wrapper with page-table epochs and full-walk work replaced
# by deterministic mocks. The wrapper itself is extracted unchanged above.
program += r'''
mod mmu {
    use core::cell::Cell;
    pub struct Vm {
        pub id: u64,
        pub epoch: Cell<Option<u64>>,
        pub reads: Cell<u64>,
        pub mutate_on_read: Cell<Option<u64>>,
    }
    impl Vm {
        pub fn new(id: u64) -> Self {
            Self { id, epoch: Cell::new(Some(1)), reads: Cell::new(0),
                   mutate_on_read: Cell::new(None) }
        }
        pub fn id(&self) -> u64 { self.id }
        pub fn mapping_validation_epoch(&self) -> Option<u64> {
            self.reads.set(self.reads.get() + 1);
            if self.mutate_on_read.get() == Some(self.reads.get()) { self.bump(); }
            self.epoch.get()
        }
        pub fn bump(&self) { self.epoch.set(self.epoch.get().and_then(|v| v.checked_add(1))); }
    }
    pub struct Uat { pub kernel: Vm }
    impl Uat { pub fn kernel_vm(&self) -> &Vm { &self.kernel } }
}
struct G17PUserRenderStorage {
    render_command_generation: u8,
    reachability_cache: G17PRenderReachabilityCache,
    last_reachability_cache_hit: bool,
    walks: usize,
    fail: bool,
    mutate_during_walk: bool,
}
impl G17PUserRenderStorage {
    fn new() -> Self {
        Self { render_command_generation: 0, reachability_cache: G17PRenderReachabilityCache::new(),
               last_reachability_cache_hit: false,
               walks: 0, fail: false, mutate_during_walk: false }
    }
    fn first_unreachable(&mut self, _: &mmu::Uat, vm: &mmu::Vm) -> Option<&'static str> {
        self.walks += 1;
        if self.mutate_during_walk { vm.bump(); }
        if self.fail { Some("missing-block") } else { None }
    }
'''
program += item("fn reachability_key(") + "\n"
program += item("pub(crate) fn first_unreachable_cached(") + "\n}\n"
program += r'''
fn fixture() -> (G17PUserRenderStorage, mmu::Uat, mmu::Vm) {
    (G17PUserRenderStorage::new(), mmu::Uat { kernel: mmu::Vm::new(1) }, mmu::Vm::new(2))
}
#[test]
fn wrapper_skips_walk_only_for_matching_vm_epochs_and_generation() {
    let (mut storage, uat, vm) = fixture();
    assert_eq!(storage.first_unreachable_cached(&uat, &vm), None);
    assert_eq!(storage.walks, 1);
    assert_eq!(storage.first_unreachable_cached(&uat, &vm), None);
    assert_eq!(storage.walks, 1);
    assert!(storage.last_reachability_cache_hit);
    storage.render_command_generation = 1;
    storage.first_unreachable_cached(&uat, &vm);
    assert_eq!(storage.walks, 2);
    assert!(!storage.last_reachability_cache_hit);
    storage.render_command_generation = 0;
    storage.first_unreachable_cached(&uat, &vm);
    assert_eq!(storage.walks, 2);
    vm.bump();
    storage.first_unreachable_cached(&uat, &vm);
    assert_eq!(storage.walks, 3);
    uat.kernel.bump();
    storage.first_unreachable_cached(&uat, &vm);
    assert_eq!(storage.walks, 4);
    let other_vm = mmu::Vm::new(3);
    other_vm.epoch.set(vm.epoch.get());
    storage.first_unreachable_cached(&uat, &other_vm);
    assert_eq!(storage.walks, 5);
}
#[test]
fn wrapper_never_caches_failed_or_overlapping_walks() {
    let (mut storage, uat, vm) = fixture();
    storage.fail = true;
    for walks in 1..=2 {
        assert_eq!(storage.first_unreachable_cached(&uat, &vm), Some("missing-block"));
        assert_eq!(storage.walks, walks);
    }
    storage.fail = false;
    storage.mutate_during_walk = true;
    assert_eq!(storage.first_unreachable_cached(&uat, &vm), None);
    assert_eq!(storage.walks, 3);
    storage.mutate_during_walk = false;
    storage.first_unreachable_cached(&uat, &vm);
    assert_eq!(storage.walks, 4);
    storage.first_unreachable_cached(&uat, &vm);
    assert_eq!(storage.walks, 4);
}
#[test]
fn wrapper_double_collect_rejects_hit_race_and_unavailable_epochs() {
    let (mut storage, uat, vm) = fixture();
    storage.first_unreachable_cached(&uat, &vm);
    vm.mutate_on_read.set(Some(vm.reads.get() + 2));
    storage.first_unreachable_cached(&uat, &vm);
    assert_eq!(storage.walks, 2);
    // The miss overlapped the epoch change, so its success was not cached.
    storage.first_unreachable_cached(&uat, &vm);
    assert_eq!(storage.walks, 3);
    storage.first_unreachable_cached(&uat, &vm);
    assert_eq!(storage.walks, 3);
    vm.epoch.set(None);
    for walks in 4..=5 {
        storage.first_unreachable_cached(&uat, &vm);
        assert_eq!(storage.walks, walks);
    }
}
'''

with tempfile.TemporaryDirectory(prefix="g17p-reachability-") as directory:
    path = Path(directory)
    (path / "cache.rs").write_text(program)
    subprocess.run(["rustc", "--edition=2021", "--test", str(path / "cache.rs"),
                    "-o", str(path / "cache-tests")], check=True)
    subprocess.run([str(path / "cache-tests")], check=True)
