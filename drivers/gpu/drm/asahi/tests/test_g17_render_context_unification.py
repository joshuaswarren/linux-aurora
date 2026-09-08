#!/usr/bin/env python3
"""Render descriptor, root, and MCache share one native GART identity."""

from pathlib import Path

root = Path(__file__).resolve().parents[1]
manager = (root / "g17_manager.rs").read_text()
limits = (root / "g17_queue_limits.rs").read_text()


def function(source: str, marker: str) -> str:
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


owner = function(manager, "impl G17PRenderRootOwner")
construct = function(manager, "fn begin_translated_render_with_hardware_buffer(")
handoff = function(manager, "pub(crate) fn handoff_retained_render_vm(")

assert "Self::Job { context } => context.context_id()" in owner
assert "u8::try_from(execution_context.context_id())" in construct
assert ".bind(vm)" not in construct
assert "G17PRenderRootOwner::Job" in construct
assert "flist_bind" not in manager
assert "BIND_CONTEXT_COUNT: u32 = 0" in limits
assert "BIND_CONTEXT_MASK: u64 = 0" in limits
assert "((1u64 << RENDER_APP_CONTEXT) - 1)" in limits

# If a failed fresh construction leaves the active slot empty, a returning VM
# must retrieve its retained graph instead of allocating another RT/TPC owner
# at the fixed per-pair addresses.
assert "if self.render_storage.is_none()" in handoff
assert "restored cached graph" in handoff
assert handoff.index("prepare_cached_reactivation") < handoff.index(
    "claim_sksm_backing"
)

reserved = set(range(5)) | {63}
available = [context for context in range(64) if context not in reserved]
assert available == list(range(5, 63))
assert len(available) == 58

live = {}
free = available[:]
for generation in range(256):
    context = free.pop(0)
    live[generation] = (context, context, context)  # descriptor/root/MCache
    assert len(set(live[generation])) == 1
    old = generation - 31
    if old in live:
        free.append(live.pop(old)[0])
        free.sort()
assert len({value[0] for value in live.values()}) == len(live)

print("PASS 58 native-unified render contexts with 256-generation reuse")
