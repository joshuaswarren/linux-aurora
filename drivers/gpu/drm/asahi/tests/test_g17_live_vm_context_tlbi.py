#!/usr/bin/env python3
"""A replaced render pool is visible in every context rooted in its VM."""

from pathlib import Path


root = Path(__file__).resolve().parents[1]
mmu = (root / "mmu.rs").read_text()
resources = (root / "g17_resources.rs").read_text()


def function(source, marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


flush = function(mmu, "pub(crate) fn flush_t8140_vm_contexts(")
assert "owner.page_table.ttb()" in flush
assert "ttbs[context_id].ttb0.load(Ordering::Acquire)" in flush
assert "== t8140_tagged_root(root, context_id)" in flush
assert flush.index("fence(Ordering::SeqCst)") < flush.index("mem::tlbi_asid(")
assert flush.index("mem::tlbi_asid(") < flush.index("mem::sync()")
assert "T8140_COMPUTE_CTXS" not in flush
assert "RENDER_APP_CONTEXT" not in flush

new_storage = function(resources, "pub(crate) fn new_user_render_storage(")
assert new_storage.index("vm.install_render_pool_mappings(") < new_storage.index(
    "self.address_space.uat.flush_t8140_vm_contexts(vm)?"
)
assert new_storage.index("self.address_space.uat.flush_t8140_vm_contexts(vm)?") < new_storage.index(
    "self.stage_render_parameter_buffer_page_list(&mut storage)?"
)

restage = function(resources, "pub(crate) fn restage_command(")
old_alias_drop = restage.index("drop(old_render_user_aliases);")
old_state_drop = restage.index("drop(old_render_state);")
restage_flush = restage.index("uat.flush_t8140_vm_contexts(vm)")
assert old_alias_drop < restage_flush
assert old_state_drop < restage_flush
assert restage_flush < restage.index("self.staging_descriptors = Some(next_descriptors)")


class ContextTlb:
    def __init__(self):
        self.cached = {}

    def translate(self, context, page_table, va):
        key = (context, va)
        if key not in self.cached:
            self.cached[key] = page_table.get(va)
        return self.cached[key]

    def flush(self, contexts):
        self.cached = {
            key: value for key, value in self.cached.items() if key[0] not in contexts
        }


# A cold fresh-storage image owns parity zero, then the first real odd ordinal
# removes it.  The following even ordinal reuses exactly that VA.  Invalidating
# only the bind context leaves the independently allocated render context with
# its cached absent translation; matching-root invalidation exposes generation
# two before its descriptor can be published.
parity_even_tpc = 0x1002050000
page_table = {parity_even_tpc: "cold"}
tlb = ContextTlb()
assert tlb.translate(9, page_table, parity_even_tpc) == "cold"
del page_table[parity_even_tpc]
tlb.flush({1})
page_table[parity_even_tpc] = "ordinal-14"
assert tlb.translate(9, page_table, parity_even_tpc) == "cold"
tlb.flush({1, 9})
assert tlb.translate(9, page_table, parity_even_tpc) == "ordinal-14"


def contexts_for_root(entries, root):
    return [index for index, entry in enumerate(entries) if entry == (root, index)]


# Bind, descriptor and app-GART contexts may all carry one VM. A different
# live client's entries are left untouched; no fixed context identity is used.
ttbat = [None] * 64
ttbat[1] = (0x111000, 1)
ttbat[7] = (0x111000, 7)
ttbat[63] = (0x111000, 63)
ttbat[8] = (0x222000, 8)
assert contexts_for_root(ttbat, 0x111000) == [1, 7, 63]
assert contexts_for_root(ttbat, 0x222000) == [8]

print("PASS live-VM TTBAT root matching flushes every alias context")
