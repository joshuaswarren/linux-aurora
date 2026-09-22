#!/usr/bin/env python3
"""PM/support quarantine is immutable; ordinary reuse transfers a whole graph."""
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


retired = resources[resources.index("pub(crate) struct G17PRetiredParameterManagement {"):]
retired = retired[:retired.index("}\n")]
for field in ("_backing: MappedObject", "_support: Option<MappedObject>", "hardware_buffer_id: u32"):
    assert field in retired

# The rejected partial reset must not be reachable from either constructor.
for source in (manager, resources):
    assert "recycled_firmware_state" not in source
    assert "G17PPreparedParameterManagement" not in source
begin = function(manager, "pub(crate) fn begin_translated_render(")
assert "retired_parameter_managers" not in begin

consume = function(resources, "pub(crate) fn into_retired_parameter_management(")
assert "_backing: backing" in consume
assert "_support: Some(support)" in consume
assert "hardware_buffer_id: manager.hardware_buffer_id" in consume
for marker in ("fn release_retired_handoff_graph(", "fn release_superseded_render_graph("):
    release = function(manager, marker)
    assert "into_retired_parameter_management()" in release
    assert release.index("if !retained_firmware_state") < release.index("ids.unreserve(id)")

quarantine = function(manager, "fn quarantine_retired_parameter_management(")
assert "retired_parameter_managers.push_within_capacity(manager)" in quarantine
assert "core::mem::forget(error.0)" in quarantine

stop = function(manager, "pub(crate) fn release_runtime_objects_after_processor_stop(")
assert stop.index("unreserve(manager.hardware_buffer_id())") < stop.index(
    "self.retired_parameter_managers.clear()"
)

# Real cache-selection and injected-failure behavior are compiled directly from
# the driver by the adjacent cache_selection and rebind_failure tests.
rebind = function(manager, "fn reactivate_recycled_render_graph(")
for forbidden in ("into_retired_parameter_management", "new_user_render_storage", ".fill(0)"):
    assert forbidden not in rebind
assert "self.render_storage = Some(cached.storage)" in rebind
assert "push_within_capacity(cached)" in rebind
assert rebind.index("push_within_capacity(cached)") < rebind.index("return Err(error)")
aliases = function(resources, "pub(crate) fn rebind_client_vm_aliases(")
assert ".fill(0)" not in aliases
assert "self.support =" not in aliases
assert "self.parameter_management =" not in aliases

print("PASS immutable PM quarantine, reserved hardware IDs, and whole-graph reuse boundaries")
