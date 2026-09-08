#!/usr/bin/env python3
"""Retired render graphs cannot be recycled beneath KSM back-references."""

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
scan = function(resources, "pub(crate) fn completion_references_descriptor_range(")
prune = function(manager, "fn prune_unreferenced_retired_render_graphs(")
stop = function(manager, "pub(crate) fn release_runtime_objects_after_processor_stop(")

assert handoff.index("completion_references_descriptor_range(&range)") < handoff.index(
    ".push_within_capacity(previous)"
)
assert handoff.index("self.prune_unreferenced_retired_render_graphs()?") < handoff.index(
    "self.park_render_sksm_backing_for_graph_rebuild()?"
)
assert "G17P_TA_HARDWARE_BUFFER_ID_COUNT" in manager[
    manager.index("let retired_render_handoff_graphs =") :
    manager.index("let retired_render_handoff_graphs =") + 180
]
assert "for ordinal in [0u8, 2u8]" in scan
assert "G17P_COMPUTE_COMPLETION_DESCRIPTOR_MASK" in scan
assert "descriptor >= range.start && descriptor < range.end" in scan
assert prune.index("completion_references_descriptor_range(&range)") < prune.index(
    ".remove(index)"
)
assert prune.index(".remove(index)") < prune.index("release_retired_handoff_graph(")
assert "self.retired_render_handoff_graphs.pop()" in stop


class Model:
    """Two-stage descriptors, 24-record ring, and native-style ID leases."""

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
            old = self.immediate
            if any(pointer in self.records for pointer in old[2]):
                self.quarantine.append(old)
            else:
                self.reserved.remove(old[1])
        retained = []
        for graph in self.quarantine:
            if any(pointer in self.records for pointer in graph[2]):
                retained.append(graph)
            else:
                self.reserved.remove(graph[1])
        self.quarantine = retained
        self.immediate = current


# Reproduce v15's third-generation ABA shape. ID 3 and descriptor memory stay
# unavailable while gen5's record survives; after twelve newer two-stage
# completions overwrite the 24-record ring, both may be reclaimed.
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
assert gen5 not in model.quarantine
assert 3 not in model.reserved

print("PASS render descriptor/HWPB ABA quarantine and bounded reclamation")
