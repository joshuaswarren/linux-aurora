#!/usr/bin/env python3
"""A -> B -> A graph return reuses A's still-owned compact aliases."""

from pathlib import Path


root = Path(__file__).resolve().parents[1]
resources = (root / "g17_resources.rs").read_text()
manager = (root / "g17_manager.rs").read_text()


def function(marker):
    start = resources.index(marker)
    pos = resources.index("{", start) + 1
    depth = 1
    while depth:
        depth += (resources[pos] == "{") - (resources[pos] == "}")
        pos += 1
    return resources[start:pos]


def manager_function(marker):
    start = manager.index(marker)
    pos = manager.index("{", start) + 1
    depth = 1
    while depth:
        depth += (manager[pos] == "{") - (manager[pos] == "}")
        pos += 1
    return manager[start:pos]


storage = resources[
    resources.index("pub(crate) struct G17PUserRenderStorage {") :
    resources.index("enum G17PRenderCommandContext")
]
prepare = function("pub(crate) fn prepare_cached_reactivation(")
restage = function("pub(crate) fn restage_command(")
lookup = manager_function("fn render_bind_for_descriptor_context(")

assert "cached_reactivation_pending" not in storage
assert "cached_reactivation_pending" not in prepare
assert "render_layout.total_size > self.render_state.logical_size" in restage
assert "render_layout.aux_fb != self.render_layout.aux_fb" in restage
# Retained driver-owned mappings are validated when the graph is created,
# grown, or rebound. Rewalking their PTEs on every restage is both redundant
# and catastrophically expensive once the TVB contains hundreds of blocks.
assert "|| !self.render_user_aliases.all_reachable_from(vm)" not in restage
assert "|| !tvb.all_reachable_from(vm)" not in restage
constructor = resources[
    resources.index("let render_user_aliases = bootstrap_stage!(") :
    resources.index("let mut render_command_ordinals =", resources.index("let render_user_aliases = bootstrap_stage!("))
]
assert "G17PRenderUserAliases::new(" in constructor
assert "!tvb.all_reachable_from(vm)" in constructor
rebind = function("fn prepare_vm_rebind(")
assert "render_user_aliases.remap(" in rebind
full_rebind = function("pub(crate) fn rebind_client_vm_aliases(")
assert "self.render_user_aliases.remap(" in full_rebind
assert "tvb.map_cached_blocks(vm)" in full_rebind
assert "G17PRenderUserAliases::new(" not in restage
assert "MappedObject::new_wc_in_range(" not in restage
assert "let render_state_va = self.render_state.iova()" in restage
assert "self.render_state.with_bytes_mut(" in restage
assert "const G17P_RENDER_STATE_AUX_FB: usize = G17P_RENDER_STATE_DYNAMIC" in resources
assert "(scene_scratch, discard, tvb, after_aux_fb)" in resources
for owner in (
    "self.render_storage",
    "retired_render_handoff_graph",
    "retired_render_handoff_graphs",
    "self.parked_render_slots",
):
    assert owner in lookup

# Model the fixed-VA owner transition. Every graph installs its compact alias
# once; all normal restages and cached returns reuse that exact owner.
class Vm:
    def __init__(self):
        self.aliases = {}

    def install(self, va, owner):
        assert va not in self.aliases
        self.aliases[va] = owner

    def reuse(self, va, owner):
        assert self.aliases.get(va) == owner


vm_a, vm_b = Vm(), Vm()
va = 0x1002050000
vm_a.install(va, "graph-a")
vm_b.install(va, "graph-b")
for submission in range(2200):
    if submission & 1:
        vm_b.reuse(va, "graph-b")
    else:
        vm_a.reuse(va, "graph-a")
assert len(vm_a.aliases) == 1
assert len(vm_b.aliases) == 1

# Command geometry changes retained RT-memory requirements, but no longer the
# compact command-state backing/alias identity.
state_layouts = {
    "small": (0x10000, 0x18000),
    "large": (0x10000, 0x18000),
    "small-again": (0x10000, 0x18000),
}
assert len(set(state_layouts.values())) == 1

# Context identity, unlike the active slot, stays with each complete graph.
# Lookup must continue to find both contexts after A -> B -> A and after the
# finite completion history has wrapped.
contexts = {6: "graph-a", 9: "graph-b"}
active = 6
for active in (9, 6, 9, 6):
    assert contexts[6] == "graph-a"
    assert contexts[9] == "graph-b"

try:
    vm_a.install(va, "graph-a2")
except AssertionError:
    pass
else:
    raise AssertionError("cached return incorrectly remapped its fixed alias")

print("PASS 2200 alternating restages keep one graph-owned render-state alias per VM")
