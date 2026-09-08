#!/usr/bin/env python3
"""One retired cross-VM graph stays alive through its successor's build."""

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
    "context_aliases: mmu::T8140NativeContextAliases"
)
assert graph.index("context_aliases: mmu::T8140NativeContextAliases") < graph.index(
    "storage: g17_resources::G17PUserRenderStorage"
)

handoff = function("pub(crate) fn handoff_retained_render_vm(")
assert handoff.index("self.require_retired_render_owner()?") < handoff.index(
    "G17PRetiredRenderGraph {"
)
for owner in (
    "self.render_bind.take().ok_or(EIO)?",
    "self.render_context_aliases.take().ok_or(EIO)?",
    "self.render_storage.take().ok_or(ENODEV)?",
):
    assert owner in handoff
assert "release_client_context" not in handoff
assert "unreserve_render_storage_hardware_buffer_id" not in handoff
assert handoff.index("self.retired_render_handoff_graph.replace(retired)") < handoff.index(
    "Self::release_retired_handoff_graph("
)

release = function("fn release_retired_handoff_graph(")
assert release.index("ids.unreserve(id)") < release.index("drop(retired)")

stop = function("pub(crate) fn release_runtime_objects_after_processor_stop(")
assert "state.retired_handoff_graph.take()" in stop
assert "self.retired_render_handoff_graph.take()" in stop


class Quarantine:
    def __init__(self):
        self.active = None
        self.retired = None
        self.reserved = set()

    def build(self, generation):
        assert generation not in self.reserved
        self.active = generation
        self.reserved.add(generation)

    def handoff(self):
        previous = self.retired
        self.retired = self.active
        self.active = None
        if previous is not None:
            self.reserved.remove(previous)


state = Quarantine()
state.build(1)
state.handoff()
assert state.retired == 1 and state.reserved == {1}
state.build(2)
assert state.reserved == {1, 2}
state.handoff()
assert state.retired == 2 and state.reserved == {2}
state.build(3)
assert state.reserved == {2, 3}

print("PASS bounded retired-render graph quarantine and teardown ownership")
