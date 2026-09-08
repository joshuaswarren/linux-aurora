#!/usr/bin/env python3
"""Firmware-referenced cross-VM graphs stay alive through later handoffs."""

from pathlib import Path


source = (Path(__file__).resolve().parents[1] / "g17_manager.rs").read_text()


def function(marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


graph = source[source.index("struct G17PRetiredRenderGraph {"):]
graph = graph[:graph.index("}\n")]
assert graph.index("bind: G17PRenderRootOwner") < graph.index(
    "context_aliases: Option<mmu::T8140NativeContextAliases>"
)
assert graph.index("context_aliases: Option<mmu::T8140NativeContextAliases>") < graph.index(
    "storage: g17_resources::G17PUserRenderStorage"
)

handoff = function("pub(crate) fn handoff_retained_render_vm(")
preserve = function("fn preserve_immediate_retired_render_graph(")
assert handoff.index("self.require_retired_render_owner()?") < handoff.index(
    "G17PRetiredRenderGraph {"
)
for owner in (
    "self.render_bind.take().ok_or(EIO)?",
    "self.render_context_aliases.take()",
    "self.render_storage.take().ok_or(ENODEV)?",
):
    assert owner in handoff
assert "release_client_context" not in handoff
assert "unreserve_render_storage_hardware_buffer_id" not in handoff
assert "completion_references_descriptor_range(&range)" in preserve
assert ".push_within_capacity(previous)" in preserve
assert "self.retired_render_handoff_graph = Some(retired)" in handoff
assert preserve.index("completion_references_descriptor_range(&range)") < preserve.index(
    "Self::release_retired_handoff_graph("
)

release = function("fn release_retired_handoff_graph(")
assert release.index("ids.unreserve(id)") < release.index("drop(retired)")

stop = function("pub(crate) fn release_runtime_objects_after_processor_stop(")
assert "state.retired_handoff_graph.take()" in stop
assert "self.retired_render_handoff_graph.take()" in stop
assert "self.retired_render_handoff_graphs.pop()" in stop


class Quarantine:
    def __init__(self):
        self.active = None
        self.retired = None
        self.completion_quarantine = []
        self.reserved = set()
        self.referenced = set()

    def build(self, generation):
        assert generation not in self.reserved
        self.active = generation
        self.reserved.add(generation)

    def handoff(self):
        previous = self.retired
        self.retired = self.active
        self.active = None
        if previous is not None:
            if previous in self.referenced:
                self.completion_quarantine.append(previous)
            else:
                self.reserved.remove(previous)

    def overwrite_completion(self, generation):
        self.referenced.remove(generation)
        if generation in self.completion_quarantine:
            self.completion_quarantine.remove(generation)
            self.reserved.remove(generation)


state = Quarantine()
state.build(1)
state.referenced.add(1)
state.handoff()
assert state.retired == 1 and state.reserved == {1}
state.build(2)
state.referenced.add(2)
assert state.reserved == {1, 2}
state.handoff()
assert state.retired == 2 and state.completion_quarantine == [1]
assert state.reserved == {1, 2}
state.build(3)
assert state.reserved == {1, 2, 3}
state.overwrite_completion(1)
assert state.reserved == {2, 3}

print("PASS completion-referenced retired-render graph quarantine and teardown ownership")
