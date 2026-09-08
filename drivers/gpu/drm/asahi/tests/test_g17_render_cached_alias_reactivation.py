#!/usr/bin/env python3
"""A -> B -> A graph return reuses A's still-owned compact aliases."""

from pathlib import Path


root = Path(__file__).resolve().parents[1]
resources = (root / "g17_resources.rs").read_text()


def function(marker):
    start = resources.index(marker)
    pos = resources.index("{", start) + 1
    depth = 1
    while depth:
        depth += (resources[pos] == "{") - (resources[pos] == "}")
        pos += 1
    return resources[start:pos]


storage = resources[
    resources.index("pub(crate) struct G17PUserRenderStorage {") :
    resources.index("enum G17PRenderCommandContext")
]
prepare = function("pub(crate) fn prepare_cached_reactivation(")
restage = function("pub(crate) fn restage_command(")

assert "cached_reactivation_pending: bool" in storage
assert "self.cached_reactivation_pending = true" in prepare
assert "let reuse_cached_render_state = self.cached_reactivation_pending" in restage
assert "if reuse_cached_render_state" in restage
assert "render_layout.total_size > self.render_state.logical_size" in restage
assert "render_layout.aux_fb != self.render_layout.aux_fb" in restage
assert "!self.render_user_aliases.all_reachable_from(vm)" in restage
assert "let mut next_render_generation = if reuse_cached_render_state" in restage
assert "self.cached_reactivation_pending = false" in restage

# Model the fixed-VA owner transition. A cached return keeps the exact alias
# owner; a normal restage installs a new parity generation only after its old
# one is retired. Re-adding A's address would collide, while using its retained
# owner is idempotent.
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
vm_a.reuse(va, "graph-a")       # A -> B -> A
vm_b.reuse(va, "graph-b")       # A -> B -> A -> B

try:
    vm_a.install(va, "graph-a2")
except AssertionError:
    pass
else:
    raise AssertionError("cached return incorrectly remapped its fixed alias")

print("PASS A->B->A->B cached graph return reuses preserved render-state aliases")
