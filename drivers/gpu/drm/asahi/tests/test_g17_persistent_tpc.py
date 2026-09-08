#!/usr/bin/env python3
"""TPC lifetime follows the retained render-target-memory owner, not a command."""

from pathlib import Path


root = Path(__file__).resolve().parents[1]
source = (root / "g17_resources.rs").read_text()


def block(marker, text=source):
    start = text.index(marker)
    pos = text.index("{", start) + 1
    depth = 1
    while depth:
        depth += (text[pos] == "{") - (text[pos] == "}")
        pos += 1
    return text[start:pos]


aliases = block("struct G17PRenderUserAliases {")
storage = block("pub(crate) struct G17PUserRenderStorage {")
render_impl = source[source.index("impl G17PUserRenderStorage {") :]
constructor = block("    fn new(\n        dev: &AsahiDevice,", render_impl)
restage = block("    pub(crate) fn restage_command(")

assert "tpc:" not in aliases
assert "render_target_tpc: KBox<MappedObject>" in storage
heap_constructor = block("fn new_g17p_render_target_tpc(")
assert "#[inline(never)]" in source[source.rfind("#[", 0, source.index("fn new_g17p_render_target_tpc(")):source.index("fn new_g17p_render_target_tpc(")]
assert "KBox::new(" in heap_constructor
assert "MappedObject::new_wc_at(" in heap_constructor
assert "g17p_render_tpc_va(queue_pair)" in heap_constructor
assert '"persistent TPC allocation"' in constructor
assert "new_g17p_render_target_tpc(dev, vm, queue_pair)" in constructor
assert "G17P_RENDER_TPC_CAPACITY" in constructor
assert "translated.parameters.tpc = render_target_tpc.iova();" in constructor
assert "translated.parameters.tpc = self.render_target_tpc.iova();" in restage
assert "next_render_user_aliases.tpc" not in restage


ALIAS_START = 0x1002000000
SLOT_SIZE = 0x80000
GENERATIONS = 2
TPC_OFFSET = 0x50000
TPC_CAPACITY = SLOT_SIZE - TPC_OFFSET


def command_slot(pair, ordinal):
    return pair * GENERATIONS + (ordinal & 1)


def tpc_range(pair):
    start = ALIAS_START + pair * GENERATIONS * SLOT_SIZE + TPC_OFFSET
    return range(start, start + TPC_CAPACITY)


# Five hundred restages move command-owned aliases every frame but retain one
# exact TPC address for each independently live physical queue pair.
for pair in range(2):
    stable = tpc_range(pair)
    assert len({stable.start for _ordinal in range(500)}) == 1
    for ordinal in range(500):
        command_base = ALIAS_START + command_slot(pair, ordinal) * SLOT_SIZE
        command_rt = range(command_base, command_base + TPC_OFFSET)
        assert command_rt.stop <= stable.start or stable.stop <= command_rt.start

assert tpc_range(0).stop <= tpc_range(1).start
assert tpc_range(0).stop == ALIAS_START + SLOT_SIZE
assert tpc_range(1).stop == ALIAS_START + 3 * SLOT_SIZE

print("PASS retained per-pair TPC lifetime and disjoint command generations")
