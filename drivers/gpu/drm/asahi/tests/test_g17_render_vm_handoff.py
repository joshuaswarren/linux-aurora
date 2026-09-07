#!/usr/bin/env python3
"""Source boundaries for job-private render roots and bounded slot switching."""
from pathlib import Path

root = Path(__file__).resolve().parents[1]
manager = (root / "g17_manager.rs").read_text()
runtime = (root / "g17_live_boot.rs").read_text()
drm = (root / "g17_drm.rs").read_text()
resources = (root / "g17_resources.rs").read_text()


def function(source, marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


handoff = function(manager, "pub(crate) fn handoff_retained_render_vm(")
assert handoff.index("self.require_retired_render_owner()?") < handoff.index("self.render_storage = None")
assert "install_t8140_native_context_aliases" not in handoff
assert "incoming VM builds a new root" in handoff

retired = function(manager, "fn require_retired_render_owner(")
assert "snapshot.job_list_empty" in retired
assert "snapshot.tiling.idle()" in retired
assert "snapshot.fragment.idle()" in retired
assert "partial_opening_outer_counters" not in retired

take = function(manager, "fn take_active_render_slot_state(")
install = function(manager, "fn install_active_render_slot_state(")
for owner in (
    "render_storage", "render_tracker", "render_bind", "render_context_aliases",
    "render_submission_ordinal", "render_auxiliary_timestamp",
    "render_hardware_buffer_id", "render_hardware_buffer_published", "render_sksm",
):
    assert owner in take
    assert owner in install

select = function(manager, "pub(crate) fn select_render_slot(")
assert "G17PRenderQueuePair::SLOT_COUNT" in select
assert "self.parked_render_slots[current_slot] = Some(current)" in select
assert ".unwrap_or_default()" in select

outer = function(resources, "pub(crate) fn publish_partial_opening_pair<")
assert "queue_pair.fragment as u8" in outer
assert "queue_pair.tiling as u8" in outer
assert "g17p_render_queue_id(" not in outer

late_tiling = function(resources, "fn build_g17p_late_tiling_bytes(")
assert "queue_pair.tiling" in late_tiling
assert "queue_pair.fragment" in late_tiling
assert "g17p_render_queue_id(" not in late_tiling

submit = function(runtime, "pub(crate) fn submit_translated_render(")
assert "select_render_slot(render_slot)?" in submit
assert "if defer_completion" in submit
wait = function(drm, "fn wait_render_slot(")
assert ".poll_deferred_render_completion(render_slot)?" in wait
assert wait.index(".poll_deferred_render_completion(render_slot)?") < wait.index("fsleep(Delta::from_millis(1))")

print("PASS job-private VM handoff and complete two-slot owner parking; completion polls release the runtime mutex")
