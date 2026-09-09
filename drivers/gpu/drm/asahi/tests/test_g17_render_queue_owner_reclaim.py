#!/usr/bin/env python3
"""Dead queues retain retired roots while selected same-VM graphs retarget."""

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
assert "reclaim_released_superseded_render_graphs()" in release
assert "take_active_render_slot_state()" in release
assert "install_active_render_slot_state(active)" in release
assert "parked_render_slots.iter_mut().flatten()" in release
assert "release_render_slot_context_owner" in release
assert "snapshot.job_list_empty" in release_slot
assert "state.storage.take()" not in release_slot
assert "state.bind.take()" not in release_slot
handoff = function(manager, "pub(crate) fn handoff_retained_render_vm(")
assert "same_vm_different_context" in handoff
assert ".retarget_command_context(vm, execution_context.clone())" in handoff
assert "render_context_retirement = Some(G17PRenderContextRetirement" in handoff
assert "Self::retired_render_graph_reclaimable(" in preserve
assert "manager.release_render_context_owner(context)" in runtime_release
complete = function(manager, "fn complete_render_context_update_retirement(")
assert complete.index("complete_context_update()") < complete.index(
    "self.render_context_retirement = None"
)
assert "self.reclaim_released_superseded_render_graphs()" in complete


class Slot:
    def __init__(self):
        self.active = None
        self.immediate = None
        self.sksm = True
        self.pending_root = None
        self.epoch = 0

    def release(self, context):
        return self.immediate == context

    def retarget(self, context):
        assert self.sksm
        self.pending_root = self.active
        self.active = context

    def complete_context_update(self):
        self.epoch += 1
        self.pending_root = None


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
        assert slot.pending_root == context
        slot.complete_context_update()
        assert slot.pending_root is None
        self.retired.discard(context)
        assert self.live == {5, 6}


owners = Owners()
for generation in range(32):
    owners.create_and_destroy(7 + generation % 4)
assert owners.live == {5, 6}
assert not owners.retired
assert all(slot.active is not None for slot in owners.slots)

# The exact fault window was two selected plus two immediate graphs. Queue
# destruction alone keeps those roots; the successor epoch on each physical
# pair releases them without waiting for processor stop.
leaked = Owners()
leaked.slots[0].active, leaked.slots[0].immediate = 9, 7
leaked.slots[1].active, leaked.slots[1].immediate = 10, 8
for context in (7, 8):
    for slot in leaked.slots:
        slot.release(context)
assert all(slot.active is not None and slot.immediate is not None for slot in leaked.slots)
for slot in leaked.slots:
    slot.complete_context_update()
    slot.immediate = None
assert all(slot.immediate is None for slot in leaked.slots)

print("PASS dead queues retain old roots through selected same-VM supersession")
