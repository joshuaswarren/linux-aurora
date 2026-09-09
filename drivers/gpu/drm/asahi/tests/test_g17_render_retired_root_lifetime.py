#!/usr/bin/env python3
"""A retired handoff graph keeps its context root through queue teardown."""

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
):
    assert complete_owner in graph

drop = function(drm, "impl Drop for G17PQueue")
release = function(manager, "pub(crate) fn release_render_context_owner(")
drain = function(manager, "fn quarantine_drained_render_graph(")
stop = function(manager, "pub(crate) fn release_runtime_objects_after_processor_stop(")

assert "release_render_owner" in drop
assert "Self::quarantine_drained_render_graph(" in release
assert "retired_firmware_render_graphs: KVec<G17PRetiredRenderGraph>" in manager
assert "retired_firmware_render_graphs.push_within_capacity(retired)" in drain
for premature_release in (
    "release_client_context",
    "take_retired_parameter_management",
    "unreserve",
    "drop(bind)",
    "drop(context_aliases)",
):
    assert premature_release not in drain
assert stop.index("self.retired_firmware_render_graphs.pop()") < stop.index(
    "self.retired_parameter_managers.clear()"
)
stop_after_pop = stop[stop.index("self.retired_firmware_render_graphs.pop()") :]
assert "Self::release_retired_handoff_graph(" in stop_after_pop


@dataclass
class RetiredGraph:
    root: bool = True
    aliases: bool = True
    storage: bool = True
    hwpb_reserved: bool = True

    def firmware_read(self):
        assert self.root and self.aliases and self.storage and self.hwpb_reserved

    def release_after_processor_stop(self):
        self.root = False
        self.aliases = False
        self.storage = False
        self.hwpb_reserved = False


# Paired command completion and scheduler drain do not globally drain firmware
# references to an already-retired handoff graph. A late context-qualified read
# must remain valid after that graph's queue is destroyed and through arbitrary
# later queue activity. Selected same-VM graph retargeting is a separate
# lifetime transaction and is deliberately outside this invariant.
retired = RetiredGraph()
quarantine = [retired]
for _ in range(4096):
    retired.firmware_read()
assert quarantine == [retired]

# Stopping both firmware processors is the first release boundary established
# by the driver. Only then may roots, aliases, storage, and HWPB identity drop.
for graph_owner in quarantine:
    graph_owner.release_after_processor_stop()
quarantine.clear()
assert not retired.root
assert not retired.aliases
assert not retired.storage
assert not retired.hwpb_reserved

print("PASS retired render graph keeps roots, aliases, storage and HWPB until processor stop")
