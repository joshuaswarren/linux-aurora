#!/usr/bin/env python3
"""Retired client graphs keep firmware-high PM managers, not VM roots."""
from pathlib import Path


root = Path(__file__).resolve().parents[1]
manager = (root / "g17_manager.rs").read_text()
resources = (root / "g17_resources.rs").read_text()


def function(text, marker):
    start = text.index(marker)
    pos = text.index("{", start) + 1
    depth = 1
    while depth:
        depth += (text[pos] == "{") - (text[pos] == "}")
        pos += 1
    return text[start:pos]


take = function(resources, "    pub(crate) fn take_retired_parameter_management(")
consume = function(resources, "    pub(crate) fn into_retired_parameter_management(")
release = function(manager, "    fn release_retired_handoff_graph(")
superseded = function(manager, "    fn release_superseded_render_graph(")
quarantine = function(manager, "    fn quarantine_retired_parameter_management(")
desync = function(manager, "    fn recycle_desynced_render_storage(")
stop = function(manager, "    pub(crate) fn release_runtime_objects_after_processor_stop(")

assert "self.parameter_management" in take and ".take()" in take
assert take.index("drop(_page_list_low)") < take.index("_support: None")
assert "_support: None" in take
assert consume.index("_support: Some(support)") > consume.index("drop(_page_list_low)")
assert release.index("take_retired_parameter_management()") < release.index(
    "ids.unreserve(id)"
)
assert "quarantine_retired_parameter_management" in release
assert superseded.index("into_retired_parameter_management()") < superseded.index(
    "ids.unreserve(id)"
)
assert "quarantine_retired_parameter_management" in superseded
assert "retired_parameter_managers.push_within_capacity(manager)" in quarantine
assert "core::mem::forget(error.0)" in quarantine
assert desync.index("take_retired_parameter_management") < desync.index(
    "self.render_storage = None"
)
assert "self.retired_parameter_managers.clear()" in stop


class Graph:
    def __init__(self, context, pm):
        self.context = context
        self.pm = pm
        self.vm_aliases = True


quarantine = []
free_contexts = [1, 60, 61, 62]
for generation in range(127):
    context = free_contexts.pop(0)
    graph = Graph(context, f"pm-{generation}")
    # Paired completion + queue drain authorizes the client mapping/root drop,
    # but the firmware's background PB walk can still name the high PM object.
    graph.vm_aliases = False
    quarantine.append(graph.pm)
    free_contexts.append(context)
    assert len(free_contexts) == 4
    assert quarantine[generation] == f"pm-{generation}"

# Processor stop is the first global firmware-reference drain.
quarantine.clear()
assert not quarantine

print("PASS retired PM lifetime: 127 queue graphs recycle all four VM roots while high PM backings survive until processor stop")
