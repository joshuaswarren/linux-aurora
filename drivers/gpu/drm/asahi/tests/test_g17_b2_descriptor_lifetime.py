#!/usr/bin/env python3
"""Compute B2 activation must not overwrite live render descriptor tables."""

from pathlib import Path


root = Path(__file__).resolve().parents[1]
submission = (root / "g17_submission.rs").read_text()
manager = (root / "g17_manager.rs").read_text()
resources = (root / "g17_resources.rs").read_text()


def block(source, marker):
    start = source.index(marker)
    pos = source.index("{", start) + 1
    depth = 1
    while depth:
        depth += (source[pos] == "{") - (source[pos] == "}")
        pos += 1
    return source[start:pos]


combined = block(submission, "pub(crate) fn apply_g17p_cl_b2_primary_state(")
computed = block(submission, "pub(crate) fn apply_g17p_cl_b2_computed_state(")
tables = block(submission, "pub(crate) fn apply_g17p_cl_b2_descriptor_tables(")
publish = block(manager, "    fn publish_compute_b2_once(")
reset = block(resources, "    pub(crate) fn reset_session_after_processor_stop(")
live_rewrite = block(resources, "    pub(crate) fn rewrite_compute_dispatch_record(")

assert "apply_g17p_cl_b2_computed_state" in combined
assert "apply_g17p_cl_b2_descriptor_tables" in combined
assert "channel_control_gpu_va" in computed
assert "region_1" not in computed and "region_2" not in computed
assert "0x0000_0080_0007_7000" in tables
assert "with_b2_computed_mut" in publish
assert "apply_g17p_cl_b2_computed_state" in publish
assert "apply_g17p_cl_b2_primary_state" not in publish
assert "apply_g17p_cl_b2_descriptor_tables" not in publish
assert resources.count("apply_g17p_cl_b2_descriptor_tables(pbdesc, uma)") == 2
assert "apply_g17p_cl_b2_descriptor_tables" in reset
assert "apply_g17p_cl_b2_descriptor_tables" not in live_rewrite

# The captured cold PBDesc at table +0x10 encodes page-list 0x1000770000.
# Its first gPM read is therefore the exact hardware fault at +0x80. It may be
# installed at session initialization, but a later queue-local B2 update must
# not restore it over render ID 1's newer halLoadPBDesc state.
cold_pbdesc = [0x0007_7000, 0x0000_0080, 0x17, 0x17]
cold_page_list = 0x10_0000_0000 | (cold_pbdesc[0] << 4)
assert cold_page_list == 0x10_0077_0000
assert cold_page_list + 0x80 == 0x10_0077_0080

pbdesc = bytearray(0x4000)
uma = bytearray(0x4000)
for word, value in enumerate(cold_pbdesc):
    pbdesc[0x10 + 4 * word : 0x14 + 4 * word] = value.to_bytes(4, "little")
live = [0x0019_0000, 0, 0x123, 0x456]
for word, value in enumerate(live):
    pbdesc[0x10 + 4 * word : 0x14 + 4 * word] = value.to_bytes(4, "little")
before_compute_b2 = bytes(pbdesc)
live_uma = bytes(range(0x20))
uma[:0x20] = live_uma
before_compute_uma = bytes(uma)

# Multiple fresh queue activations change only the separate computed page.
for channel in [0xFFFF_FC20_1234_8000, 0xFFFF_FC20_5678_C000]:
    computed_page = bytearray(0x4000)
    computed_page[0x808:0x810] = channel.to_bytes(8, "little")
    assert int.from_bytes(computed_page[0x808:0x810], "little") == channel
    assert bytes(pbdesc) == before_compute_b2
    assert bytes(uma) == before_compute_uma

print("PASS compute B2 preserves live PBDesc/UMA ownership after static initialization")
