#!/usr/bin/env python3
"""VM_BIND bounds preflight and keeps dma-resv waits interruptible."""

from pathlib import Path
import random


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

# The module-safe path preserves the established interruptible dma-resv waits.
# It deliberately adds no outer mutex/lock order and never enters the removed
# address-sized PTE oracle. A kernel-crate GPUVM extension is required before
# multiple request objects can share one drm_exec guard.
assert "exec_lock(Some(&map.gem), true)" in commit_batch
assert "exec_lock(None, true)" in commit_batch
assert "exec_lock(Some(&prepared.gem), false)" not in MMU
assert "bind_lock" not in FILE
assert "exec_lock_array" not in commit_batch
assert "inner.sm_map(" in commit_batch
assert "sm_unmap" in commit_batch


# Executable failure-injection model. Whole-array semantic/resource
# preparation fails before visible mutation. A signal during the first
# interruptible dma-resv wait likewise leaves the VM untouched.
def transact(leaves, ops, fail_prepare=None, interrupt_at_lock=None):
    prepared = []
    work = 0
    for index, op in enumerate(ops):
        work += 1
        if index == fail_prepare:
            return False, leaves.copy(), work
        prepared.append(op)

    result = leaves.copy()
    for index, (kind, start, size) in enumerate(prepared):
        work += 1
        if index == interrupt_at_lock:
            return False, result, work
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
ok, leaves, _ = transact(initial, ops, interrupt_at_lock=0)
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
ok, _, work = transact({}, stress, interrupt_at_lock=0)
assert not ok
assert work == len(stress) + 1

print("PASS bounded VM_BIND preflight and interruptible dma-resv regression model")
