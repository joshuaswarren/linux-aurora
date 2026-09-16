#!/usr/bin/env python3
"""A later paired context update permits complete-graph reactivation."""

from pathlib import Path


root = Path(__file__).resolve().parents[1]
manager = (root / "g17_manager.rs").read_text()
drm = (root / "g17_drm.rs").read_text()


def function(source, marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


graph = manager[manager.index("struct G17PRetiredRenderGraph {") :]
graph = graph[: graph.index("}\n")]
for complete_owner in (
    "bind: G17PRenderRootOwner",
    "context_aliases: Option<mmu::T8140NativeContextAliases>",
    "storage: g17_resources::G17PUserRenderStorage",
    "retire_after_context_epoch: u64",
):
    assert complete_owner in graph

pending = manager[manager.index("struct G17PRenderContextRetirement {") :]
pending = pending[: pending.index("}\n")]
assert "bind: G17PRenderRootOwner" in pending
assert "retire_after_context_epoch: u64" in pending

drop = function(drm, "impl Drop for G17PQueue")
handoff = function(manager, "pub(crate) fn handoff_retained_render_vm(")
poll = function(manager, "pub(crate) fn poll_translated_render_completion(")
complete = function(manager, "fn complete_render_context_update_retirement(")
reclaimable = function(manager, "fn retired_render_graph_reclaimable(")
reclaim = function(manager, "fn reclaim_released_superseded_render_graphs(")
release = function(manager, "fn release_superseded_render_graph(")
abandoned = function(manager, "pub(crate) fn retire_abandoned_render_if_firmware_retired(")
stop = function(manager, "pub(crate) fn release_runtime_objects_after_processor_stop(")

assert "release_render_owner" in drop
assert handoff.index(".retarget_command_context(vm, execution_context.clone())") < handoff.index(
    ".render_bind\n                .replace"
)
assert "render_context_retirement = Some(G17PRenderContextRetirement" in handoff
assert "self.next_render_context_epoch(self.active_render_slot)?" in handoff
assert poll.index("if !event_control.unlinked()") < poll.index(
    "self.complete_render_context_update_retirement()?"
)
assert complete.index(".context_update_pending()") < complete.index(
    ".complete_context_update()"
)
assert complete.index("self.render_context_epochs[slot] = next") < complete.index(
    "self.render_context_retirement = None"
)
assert complete.index("self.render_context_retirement = None") < complete.index(
    "self.reclaim_released_superseded_render_graphs()"
)
assert "*epoch >= graph.retire_after_context_epoch" in reclaimable
assert "release_superseded_render_graph" not in reclaim
assert ".remove(" not in reclaim
selection = function(manager, "fn select_cached_render_graph(")
assert "Self::retired_render_graph_reclaimable(" in selection
reactivate = function(manager, "fn reactivate_recycled_render_graph(")
assert reactivate.index(".retarget_command_context(") < reactivate.index("drop(cached.bind)")
assert "push_within_capacity(cached)" in reactivate
assert "storage.into_retired_parameter_management()" in release
assert "retired_parameter_managers" in release
assert release.index("into_retired_parameter_management") < release.index(
    "ids.unreserve(id)"
)
assert release.index("ids.unreserve(id)") < release.index("drop(bind)")
assert "complete_render_context_update_retirement" not in abandoned
assert "self.render_context_retirement = None" in stop
assert "self.render_context_epochs = [0; 2]" in stop
assert "retired_firmware_render_graphs" not in manager


print("PASS context epochs and root owners remain intact until complete-graph reactivation")
