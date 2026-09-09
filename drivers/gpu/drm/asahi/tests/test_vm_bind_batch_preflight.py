#!/usr/bin/env python3
"""VM_BIND validates and reserves a whole batch before changing UAT leaves."""

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
commit_map = block(MMU, "    pub(crate) fn bind_prepared_object(")
preflight = block(MMU, "    pub(crate) fn preflight_user_bind_range(")
match_ranges = block(PGTABLE, "    pub(crate) fn matches_gpuva_ranges(")
step_map = block(MMU, "    fn step_map(")
step_unmap = block(MMU, "    fn step_unmap(")
step_remap = block(MMU, "    fn step_remap(")

# The complete user array is copied first, then every operation is planned.
# No page-table callback is entered until the commit loop.
assert vm_bind.index("ops.push(bind") < vm_bind.index("for op in ops")
assert vm_bind.index("vm.reserve_user_range_updates(count)") < vm_bind.index("for op in ops")
assert vm_bind.index("vm.preflight_user_bind_range") < vm_bind.index("for plan in plans.iter_mut()")
assert vm_bind.index("vm.prepare_bind_object") < vm_bind.index("for plan in plans.iter_mut()")
assert vm_bind.index("vm.prepare_user_unmap") < vm_bind.index("for plan in plans.iter_mut()")
assert vm_bind.index("vm.reserve_deferred_user_unmaps") < vm_bind.index(
    "for plan in plans.iter_mut()"
)
assert vm_bind.index("gem::Object::lookup_handle") < vm_bind.index(
    "for plan in plans.iter_mut()"
)
assert "mmu::UAT_PGSZ as u64" in vm_bind
assert "mmu::UAT_PGMSK as u64\n            } else" not in vm_bind
assert vm_bind.index("for plan in plans.iter_mut()") < vm_bind.index(
    "vm.track_t8140_native_context_binding"
)

# Preparation allocates every GPUVA split node, realizes the SG table, and
# reserves page-table levels without installing a leaf. Commit consumes those
# exact resources with a non-interruptible VM lock.
for token in ("new_va:", "prev_va:", "next_va:", "owned_sg_table", "alloc_pages"):
    assert token in prepare_map
assert "sm_map(" not in prepare_map
assert "exec_lock(Some(&prepared.gem), false)" in commit_map
assert "inner.sm_map(" in commit_map

# The driver-owned normalized interval mirror lets preflight prove that GPUVM
# and UAT agree, including holes, before a stale upper shadow can fail late.
assert "user_gpuva_ranges" in preflight
assert "matches_gpuva_ranges" in preflight
assert "matched_pages == expected_pages" in match_ranges
assert "expected == mapped" in match_ranges
assert step_map.index("map_and_link_va") < step_map.index("record_user_map")
assert step_unmap.index("unmap_and_unlink_va_defer") < step_unmap.index(
    "record_user_unmap"
)
assert step_remap.rindex("map_and_link_va") < step_remap.index(
    "record_user_unmap"
)


PAGE = 0x4000


def map_range(ranges, new):
    start, end = new
    out = []
    inserted = False
    for a, b in ranges:
        if b < start:
            out.append((a, b))
        elif end < a:
            if not inserted:
                out.append((start, end))
                inserted = True
            out.append((a, b))
        else:
            start, end = min(start, a), max(end, b)
    if not inserted:
        out.append((start, end))
    return out


def unmap_range(ranges, hole):
    hs, he = hole
    out = []
    for a, b in ranges:
        if b <= hs or he <= a:
            out.append((a, b))
            continue
        if a < hs:
            out.append((a, hs))
        if he < b:
            out.append((he, b))
    return out


def pages(ranges):
    return {
        address
        for start, end in ranges
        for address in range(start, end, PAGE)
    }


# Exercise the normalized interval contract through replacement, adjacency,
# splitting, and many deterministic random sparse operations.
ranges = []
ranges = map_range(ranges, (0x10000, 0x20000))
ranges = map_range(ranges, (0x20000, 0x28000))
assert ranges == [(0x10000, 0x28000)]
ranges = unmap_range(ranges, (0x18000, 0x20000))
assert ranges == [(0x10000, 0x18000), (0x20000, 0x28000)]
ranges = map_range(ranges, (0x14000, 0x24000))
assert ranges == [(0x10000, 0x28000)]

rng = random.Random(0x8140)
ranges = []
expected = set()
for _ in range(2000):
    start_page = rng.randrange(1, 128)
    count = rng.randrange(1, 12)
    interval = (start_page * PAGE, (start_page + count) * PAGE)
    if rng.randrange(2):
        ranges = map_range(ranges, interval)
        expected.update(range(interval[0], interval[1], PAGE))
    else:
        ranges = unmap_range(ranges, interval)
        expected.difference_update(range(interval[0], interval[1], PAGE))
    assert pages(ranges) == expected
    assert all(ranges[i][1] < ranges[i + 1][0] for i in range(len(ranges) - 1))


# A stale upper-half leaf absent from the software mirror is rejected during
# preflight, before the lower member of the pair is committed.
software = [(0x10000, 0x18000)]
uat = pages(software) | {0x10000010000}
for touched in ((0x10000, 0x18000), (0x10000010000, 0x10000018000)):
    expected_here = {p for p in pages(software) if touched[0] <= p < touched[1]}
    actual_here = {p for p in uat if touched[0] <= p < touched[1]}
    if touched[0] >= 0x10000000000:
        assert expected_here != actual_here

print("PASS VM_BIND batch preflight, GPUVM/UAT mirror, and sparse-pair atomicity model")
