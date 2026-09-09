#!/usr/bin/env python3
"""VM_BIND bounds preflight and keeps dma-resv waits interruptible."""

from pathlib import Path
import random
import threading


ROOT = Path(__file__).resolve().parents[1]
FILE = (ROOT / "file.rs").read_text()
MMU = (ROOT / "mmu.rs").read_text()
PGTABLE = (ROOT / "pgtable.rs").read_text()


def block(source: str, marker: str) -> str:
    start = source.index(marker)
    brace = source.index("{", start)
    depth = 1
    pos = brace + 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


vm_bind = block(FILE, "    pub(crate) fn vm_bind(")
prepare_map = block(MMU, "    pub(crate) fn prepare_bind_object(")
commit_batch = block(MMU, "    pub(crate) fn commit_prepared_user_bind_batch(")

# The whole userspace array is copied, validated, and prepared before commit.
assert vm_bind.index("ops.push(bind") < vm_bind.index("for op in ops")
assert vm_bind.index("gem::Object::lookup_handle") < vm_bind.index(
    "commit_prepared_user_bind_batch"
)
assert vm_bind.index("vm.prepare_bind_object") < vm_bind.index(
    "commit_prepared_user_bind_batch"
)
assert vm_bind.index("vm.prepare_user_unmap") < vm_bind.index(
    "commit_prepared_user_bind_batch"
)
assert vm_bind.index("vm.reserve_deferred_user_unmaps") < vm_bind.index(
    "commit_prepared_user_bind_batch"
)

# Preparation reserves callback nodes, BO scatterlists, and page-table levels
# without installing a leaf. There is no address-sized PTE validation walk.
for token in ("new_va:", "prev_va:", "next_va:", "owned_sg_table", "alloc_pages"):
    assert token in prepare_map
assert "sm_map(" not in prepare_map
assert "matches_gpuva_ranges" not in MMU
assert "matches_gpuva_ranges" not in PGTABLE
assert "user_gpuva_ranges" not in MMU

# The core GPUVM array helper acquires every request BO in one interruptible
# wound/wait transaction. No leaf callback runs before that one acquisition.
GPUVM = (ROOT.parents[3] / "rust/kernel/drm/gpuvm.rs").read_text()
lock_array = block(GPUVM, "    pub fn exec_lock_array")
assert "DRM_EXEC_INTERRUPTIBLE_WAIT" in lock_array
assert "DRM_EXEC_IGNORE_DUPLICATES" in lock_array
assert "drm_gpuvm_exec_lock_array" in lock_array
assert "exec_lock_array(&objects, true)" in commit_batch
assert commit_batch.index("exec_lock_array(&objects, true)") < commit_batch.index(
    "for op in &mut batch.ops"
)
assert "exec_lock(Some(&prepared.gem), false)" not in MMU
assert "bind_lock" not in FILE
assert "inner.sm_map_indexed(" in commit_batch
assert "sm_unmap" in commit_batch
assert commit_batch.count("gate.blocked") == 2
assert "T8140VmMappingCommitGuard" in commit_batch
assert commit_batch.index("exec_lock_array(&objects, true)") < commit_batch.rindex(
    "gate.blocked"
)
assert commit_batch.index("core::mem::drop(inner)") < commit_batch.index(
    "core::mem::drop(commit)"
)


# Executable failure-injection model. Whole-array semantic/resource
# preparation fails before visible mutation. A signal during the first
# interruptible dma-resv wait likewise leaves the VM untouched.
def transact(leaves, ops, fail_prepare=None, interrupt_lock=False):
    prepared = []
    work = 0
    for index, op in enumerate(ops):
        work += 1
        if index == fail_prepare:
            return False, leaves.copy(), work
        prepared.append(op)

    work += 1
    if interrupt_lock:
        return False, leaves.copy(), work

    result = leaves.copy()
    for kind, start, size in prepared:
        work += 1
        if kind == "map":
            result[(start, start + size)] = kind
        else:
            result = {
                interval: value
                for interval, value in result.items()
                if interval[1] <= start or start + size <= interval[0]
            }
    return True, result, work


initial = {(0x10000, 0x14000): "map"}
ops = [
    ("map", 0x20000, 0x4000),
    ("map", 0x1_0002_0000, 0x4000),
    ("unmap", 0x10000, 0x4000),
]
for fail_at in range(len(ops)):
    ok, leaves, _ = transact(initial, ops, fail_prepare=fail_at)
    assert not ok and leaves == initial
ok, leaves, _ = transact(initial, ops, interrupt_lock=True)
assert not ok and leaves == initial
ok, leaves, _ = transact(initial, ops)
assert ok and (0x10000, 0x14000) not in leaves
assert (0x20000, 0x24000) in leaves
assert (0x1_0002_0000, 0x1_0002_4000) in leaves


# The semantic/handle preflight is O(operation count) and adds no leaf-sized
# validation walk. Stress giant sparse ranges without iterating their pages;
# the removed PTE oracle scaled with total address coverage instead.
rng = random.Random(0x8140)
stress = []
for index in range(50_000):
    start = (rng.randrange(1, 1 << 24) << 14) & ((1 << 42) - 1)
    size = (1 << rng.randrange(14, 39)) & ~0x3FFF
    stress.append(("map" if index & 1 else "unmap", start, size))
ok, _, work = transact({}, stress, interrupt_lock=True)
assert not ok
assert work == len(stress) + 1


# Exact race model for the two post-preflight orderings. Quarantine closes the
# gate before an error fence can wake teardown; a binder which slept on
# dma-resv must recheck after waking, while a commit already inside the gate
# must finish before quarantine is allowed to publish failure.
class MappingGate:
    def __init__(self):
        self.cv = threading.Condition()
        self.blocked = False
        self.commits = 0

    def enter(self):
        with self.cv:
            if self.blocked:
                return False
            self.commits += 1
            return True

    def leave(self):
        with self.cv:
            self.commits -= 1
            self.cv.notify_all()

    def quarantine(self):
        with self.cv:
            self.blocked = True
            while self.commits:
                self.cv.wait()


# Negative control: the former one-check path commits after its reservation
# wait even though quarantine has already published the error boundary.
old_resv_entered = threading.Event()
old_resv_released = threading.Event()
old_mutated = threading.Event()


def old_bind_after_resv():
    assert not old_gate.blocked
    old_resv_entered.set()
    old_resv_released.wait()
    old_mutated.set()


old_gate = MappingGate()
old_binder = threading.Thread(target=old_bind_after_resv)
old_binder.start()
old_resv_entered.wait()
old_gate.quarantine()
old_resv_released.set()
old_binder.join()
assert old_mutated.is_set(), "negative control did not reproduce post-quarantine mutation"


# Binder passed the cheap first check, then slept in drm_exec. Quarantine wins;
# the second check rejects the leaf update after dma-resv wakes.
gate = MappingGate()
resv_entered = threading.Event()
resv_released = threading.Event()
mutated = threading.Event()


def bind_after_resv():
    assert not gate.blocked
    resv_entered.set()
    resv_released.wait()
    if not gate.enter():
        return
    mutated.set()
    gate.leave()


binder = threading.Thread(target=bind_after_resv)
binder.start()
resv_entered.wait()
gate.quarantine()
error_fence_signalled = True
resv_released.set()
binder.join()
assert error_fence_signalled and not mutated.is_set()


# Binder entered the short prepared commit first. Quarantine must not return
# (and therefore cannot signal its error fence) until that transaction leaves.
gate = MappingGate()
commit_entered = threading.Event()
finish_commit = threading.Event()
quarantine_returned = threading.Event()


def active_commit():
    assert gate.enter()
    commit_entered.set()
    finish_commit.wait()
    gate.leave()


def quarantine_during_commit():
    gate.quarantine()
    quarantine_returned.set()


binder = threading.Thread(target=active_commit)
binder.start()
commit_entered.wait()
quarantine = threading.Thread(target=quarantine_during_commit)
quarantine.start()
assert not quarantine_returned.wait(0.05)
finish_commit.set()
binder.join()
quarantine.join()
assert quarantine_returned.is_set()

print("PASS bounded VM_BIND preflight and interruptible dma-resv regression model")
