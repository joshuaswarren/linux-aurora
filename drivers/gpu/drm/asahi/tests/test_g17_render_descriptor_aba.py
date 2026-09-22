#!/usr/bin/env python3
"""Retired render contexts survive finite completion-ring history."""

from pathlib import Path


root = Path(__file__).resolve().parents[1]
manager = (root / "g17_manager.rs").read_text()
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
preserve = function(manager, "fn preserve_immediate_retired_render_graph(")
prune = function(manager, "fn prune_unreferenced_retired_render_graphs(")
stop = function(manager, "pub(crate) fn release_runtime_objects_after_processor_stop(")

assert ".push_within_capacity(previous)" in preserve
assert "completion_references_descriptor_range" not in preserve
assert handoff.index("self.preserve_immediate_retired_render_graph()") < handoff.index(
    "self.park_render_sksm_backing_for_graph_rebuild()"
)
assert '"handoff-preserve-before-fresh"' in handoff
assert '"handoff-park-sksm-backing"' in handoff
assert "G17P_TA_HARDWARE_BUFFER_ID_COUNT" in manager[
    manager.index("let retired_render_handoff_graphs =") :
    manager.index("let retired_render_handoff_graphs =") + 180
]
assert "Deliberately no completion-ring pruning" in prune
assert ".remove(index)" not in prune
assert "release_retired_handoff_graph(" not in prune
assert "self.retired_render_handoff_graphs.pop()" in stop


class Model:
    """Finite completion history cannot release a live descriptor context."""

    def __init__(self):
        self.records = []
        self.immediate = None
        self.quarantine = []
        self.reserved = set()

    def complete(self, generation, hwpb):
        descriptors = (generation * 0x40000, generation * 0x40000 + 0x14000)
        self.records.extend(descriptors)
        self.records = self.records[-24:]
        self.reserved.add(hwpb)
        return (generation, hwpb, descriptors)

    def handoff(self, current):
        if self.immediate is not None:
            self.quarantine.append(self.immediate)
        self.immediate = current

    def stop_processors(self):
        self.immediate = None
        self.quarantine.clear()
        self.reserved.clear()


# Reproduce the v18d context-6 fault. New completions overwrite the old
# descriptor pointer, but firmware can still name context 6 for a late PM read;
# its graph and HWPB remain owned until both firmware processors stop.
model = Model()
gen5 = model.complete(5, 3)
model.handoff(gen5)
gen7 = model.complete(7, 1)
model.handoff(gen7)
assert 3 in model.reserved
assert gen5 in model.quarantine
for generation in range(8, 20):
    model.complete(generation, generation + 10)
model.handoff(model.complete(20, 40))
assert gen5 in model.quarantine
assert 3 in model.reserved
model.stop_processors()
assert not model.quarantine
assert not model.reserved

print("PASS render descriptor-context/HWPB ownership survives completion-ring wrap")
