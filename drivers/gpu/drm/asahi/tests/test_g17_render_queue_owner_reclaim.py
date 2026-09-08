#!/usr/bin/env python3
"""Dead DRM queues leave a bounded graph that the next same-VM queue retargets."""

from pathlib import Path

root = Path(__file__).resolve().parents[1]
manager = (root / "g17_manager.rs").read_text()
runtime = (root / "g17_live_boot.rs").read_text()
drm = (root / "g17_drm.rs").read_text()


def function(source, marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


drop = function(drm, "impl Drop for G17PQueue")
release = function(manager, "pub(crate) fn release_render_context_owner(")
release_slot = function(manager, "fn release_render_slot_context_owner(")
preserve = function(manager, "fn preserve_immediate_retired_render_graph(")
runtime_release = function(runtime, "pub(crate) fn release_render_owner(")

assert drop.index("drop(self.render_scheduler.take())") < drop.index(
    ".release_render_owner(self.id, self.context.context_id() as u16)"
)
assert "released_render_contexts |= 1u64 << context" in release
assert "retired_render_handoff_graphs.remove(index)" in release
assert "take_active_render_slot_state()" in release
assert "install_active_render_slot_state(active)" in release
assert "parked_render_slots.iter_mut().flatten()" in release
assert "release_render_slot_context_owner" in release
assert "state.retired_handoff_graph.take()" in release_slot
assert "snapshot.job_list_empty" in release_slot
assert "quarantine_drained_render_graph" in release_slot
assert "state.storage.take()" not in release_slot
assert "state.bind.take()" not in release_slot
handoff = function(manager, "pub(crate) fn handoff_retained_render_vm(")
assert "same_vm_different_context" in handoff
assert ".retarget_command_context(vm, execution_context.clone())" in handoff
assert "released_render_contexts & (1u64 << context)" in preserve
assert "manager.release_render_context_owner(context)" in runtime_release


class Slot:
    def __init__(self):
        self.active = None
        self.immediate = None
        self.sksm = True

    def release(self, context):
        if self.immediate == context:
            self.immediate = None

    def retarget(self, context):
        assert self.sksm
        self.active = context


class Owners:
    def __init__(self):
        self.live = {5, 6}  # compositor queues A/B
        self.retired = set()
        self.slots = [Slot(), Slot()]

    def create_and_destroy(self, context):
        slot = self.slots[context & 1]
        slot.active = context
        # Scheduler drain leaves the completed physical graph intact. The next
        # same-VM queue retargets its context without rebuilding fixed mappings.
        slot.release(context)
        slot.retarget(context + 64)
        self.retired.discard(context)
        assert self.live == {5, 6}


owners = Owners()
for generation in range(32):
    owners.create_and_destroy(7 + generation % 4)
assert owners.live == {5, 6}
assert not owners.retired
assert all(slot.active is not None for slot in owners.slots)

# The exact leak seen on hardware was two selected plus two immediate graphs:
# immediate graphs are reclaimed at their queue boundary while the two selected
# physical-pair graphs remain bounded and reusable.
leaked = Owners()
leaked.slots[0].active, leaked.slots[0].immediate = 9, 7
leaked.slots[1].active, leaked.slots[1].immediate = 10, 8
for context in (7, 8):
    for slot in leaked.slots:
        slot.release(context)
assert all(slot.active is not None and slot.immediate is None for slot in leaked.slots)

print("PASS dead queues preserve two bounded graphs and same-VM contexts retarget in place")
