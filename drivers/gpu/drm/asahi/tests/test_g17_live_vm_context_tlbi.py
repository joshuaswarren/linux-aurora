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
