#!/usr/bin/env python3
"""A later paired context update retires old roots without churn leaks."""

from dataclasses import dataclass
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
assert "retired_render_handoff_graphs.remove(index)" in reclaim
assert release.index("ids.unreserve(id)") < release.index("drop(retired)")
assert "take_retired_parameter_management" not in release
assert "complete_render_context_update_retirement" not in abandoned
assert "self.render_context_retirement = None" in stop
assert "self.render_context_epochs = [0; 2]" in stop
assert "retired_firmware_render_graphs" not in manager


@dataclass
class Graph:
    owner: int
    hwpb: int
    target_epoch: int
    released: bool = False
    root: bool = True
    aliases: bool = True
    storage: bool = True

    def firmware_read(self):
        assert self.root and self.aliases and self.storage and not self.released

    def reclaim(self, free_ids):
        assert not self.released
        self.root = self.aliases = self.storage = False
        self.released = True
        free_ids.add(self.hwpb)


class Pair:
    def __init__(self):
        self.epoch = 0
        self.current = None
        self.retired = []
        self.pending_root = None

    def replace_vm(self, owner, free_ids):
        if self.current is not None:
            self.current.target_epoch = self.epoch + 1
            self.retired.append(self.current)
        assert free_ids
        hwpb = min(free_ids)
        free_ids.remove(hwpb)
        self.current = Graph(owner, hwpb, self.epoch + 1)

    def retarget_same_vm(self, context):
        assert self.pending_root is None
        self.pending_root = context

    def complete_context_update(self, dead_owners, free_ids):
        for graph in self.retired:
            graph.firmware_read()
        self.epoch += 1
        self.pending_root = None
        kept = []
        for graph in self.retired:
            if graph.owner in dead_owners and graph.target_epoch <= self.epoch:
                graph.reclaim(free_ids)
            else:
                kept.append(graph)
        self.retired = kept


# The HWPB pool has 127 IDs, but sequential window churn must not impose a
# 127-window session lifetime. Two physical pairs keep only their selected
# graph plus the immediately superseded generation until the successor's
# context-update group completes.
free_ids = set(range(0x7F))
pairs = [Pair(), Pair()]
dead_owners = set()
high_water = 0
for owner in range(4096):
    pair = pairs[owner & 1]
    if pair.current is not None:
        dead_owners.add(pair.current.owner)
    pair.replace_vm(owner, free_ids)
    high_water = max(high_water, 0x7F - len(free_ids))
    pair.complete_context_update(dead_owners, free_ids)
    high_water = max(high_water, 0x7F - len(free_ids))

assert high_water <= 4
assert len(free_ids) == 0x7F - 2
assert all(not pair.retired for pair in pairs)

# In-place same-VM replacement reuses the graph/HWPB, but the old context root
# remains valid through the replacement's paired completion.
pair = pairs[0]
old_context = 41
pair.retarget_same_vm(old_context)
assert pair.pending_root == old_context
pair.complete_context_update(dead_owners, free_ids)
assert pair.pending_root is None

print("PASS 4096-client context epochs retain late roots and recycle all superseded HWPB IDs")
