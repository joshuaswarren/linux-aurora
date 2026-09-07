#!/usr/bin/env python3
"""Device-global render publication fields follow the device, not the slot.

Two bounded render slots share the outer data-master channels and the USC
FList. Two fields in a render's publication describe that shared state, and
both were derived from the submitting slot's own ordinal:

* the outer ring entry's channel-started byte (+0x17, native `!chan[0x3c]`),
  passed as `first_submit = render_submission_ordinal == 0`;
* tag-15 +0x3e, the FList generation, written as the slot ordinal.

With one client the slot ordinal and the device counters coincide, so both
were correct for every qualified run. With per-VM slot affinity a second
client's first render is the first time a slot ordinal of 0 meets a warm
device: it claimed a never-started channel that carried the other slot's whole
history and announced FList generation 0 against a pool the firmware last saw
at N. Measured 2026-09-07 as the firmware NOP-retiring that render (tiler never
stamped its start) and raising a stamp-blamed recovery on its TA queue.

This compiles the real tag-15 optional encoder and pins the manager's sources
for both fields.
"""
from pathlib import Path
import re
import subprocess
import tempfile

src = Path(__file__).resolve().parent.parent
sub = (src / "g17_submission.rs").read_text()
manager = (src / "g17_manager.rs").read_text()
resources = (src / "g17_resources.rs").read_text()


def block(text, name):
    m = re.search(r"^[ \t]*(?:pub\(crate\) )?(?:const )?(?:fn|struct|enum) " + name + r"\b", text, re.M)
    assert m, name
    start = text.index("{", m.end())
    end, depth = start + 1, 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[m.start():end]


def constant(name):
    return re.search(r"^pub\(crate\) const " + name + r": .*?;", sub, re.M | re.S).group()


# --- channel-started byte -----------------------------------------------------
inner = block(manager, "begin_translated_render_with_hardware_buffer")
assert "let first_outer_submit =" in inner
assert "!self.render_outer_channels_started" in inner
publish = inner[inner.index(".publish_partial_opening_pair("):inner.index("move || {", inner.index(".publish_partial_opening_pair("))]
assert "first_outer_submit," in publish
assert "self.render_submission_ordinal == 0" not in publish, \
    "the shared channels' started byte is still derived from the slot ordinal"
assert inner.index("self.render_hardware_buffer_published = true;") \
    < inner.index("self.render_outer_channels_started = true;")
stop = block(manager, "release_runtime_objects_after_processor_stop")
assert "self.render_outer_channels_started = false;" in stop
outer = block(sub, "prepare_g17p_partial_opening_work_pair")
assert "let render_channel_started = !first_submit;" in outer

# --- FList generation ---------------------------------------------------------
assert inner.index("set_parameter_buffer_lease_token(") < inner.index("set_flist_generation(") \
    < inner.index("stage_render_groups_with_sksm("), "the generation must be set before staging"
assert "lifecycle.sequence" in inner[inner.index("set_flist_generation("):][:600]
for name in ["stage_fragment", "stage_fragment_prefix"]:
    body = block(resources, name)
    assert "apply_g17p_retained_optional_with_flist_generation(" in body, name
    assert "let flist_generation = self.flist_generation;" in body, name
late = block(resources, "prepare_late_tiling_publication_with_stamp")
assert "self.flist_generation," in late
restage = block(resources, "restage_command")
assert "self.flist_generation = g17_submission::g17p_render_flist_generation(submission_ordinal);" in restage
assert "apply_g17p_retained_optional(" not in resources.replace("apply_g17p_retained_optional_with_flist_generation(", ""), \
    "a tag-15 optional is still encoded without the device-global generation"

body = "\n".join([
    "#![allow(dead_code, non_upper_case_globals)]",
    r'''
mod module_parameters {
    pub struct Param(u32);
    impl Param { pub fn value(&self) -> &u32 { &self.0 } }
    pub static g17p_ta_oracle: Param = Param(0);
    pub static g17p_frag_oracle: Param = Param(0);
    pub static g17p_render_ta_qid_field: Param = Param(1);
    pub static g17p_render_3d_dm: Param = Param(0xff);
    pub static g17p_render_ta_dm: Param = Param(0xff);
}
''',
    "mod g17_submission {",
    *[constant(n) for n in [
        "G17P_COLD_OPENING_OPTIONAL_SIZE", "G17P_NATIVE_RENDER_DOORBELL_PRIORITY",
        "G17P_NATIVE_RENDER_QOS_HARDWARE_BUFFER_ID", "G17P_NATIVE_RENDER_POLICY"]],
    *["#[derive(Debug, Copy, Clone, PartialEq, Eq)]\n" + block(sub, n) for n in [
        "SubmissionError", "G17PColdOpeningStage", "G17PColdOpeningOptionalAddresses"]],
    *[block(sub, n) for n in [
        "put_u16_le", "put_u32_le", "put_u64_le", "apply_g17p_cold_opening_optional",
        "apply_g17p_retained_optional", "apply_g17p_retained_optional_with_flist_generation",
        "g17p_render_flist_generation"]],
    "}",
    r'''
use g17_submission::*;
fn u64_at(b: &[u8], at: usize) -> u64 { u64::from_le_bytes(b[at..at+8].try_into().unwrap()) }
fn u32_at(b: &[u8], at: usize) -> u32 { u32::from_le_bytes(b[at..at+4].try_into().unwrap()) }
fn addresses() -> G17PColdOpeningOptionalAddresses {
    G17PColdOpeningOptionalAddresses { context_scratch: 0x7000100000,
        firmware_scratch: 0x7000104000, usc_priv_mem_flist: 0xfffffc20001f8000,
        usc_freelist_hardware_buffer_id: 0x11223344, channel_control: 0x700010c000,
        tiling_shared_object: 0xfffffc20c0060000, parameter_buffer_token: 0x123456789abc,
        owner_pid: 0x5678, ta_hardware_buffer_id: 7 }
}

#[test]
fn plus_3e_follows_the_device_generation_and_nothing_else_moves() {
    for stage in [G17PColdOpeningStage::Tiling, G17PColdOpeningStage::Fragment] {
        for ordinal in [0u32, 5, 1458] {
            for generation in [0u64, 1, 1459, 0x1_0000_0000] {
                let mut raw = [0xa5; G17P_COLD_OPENING_OPTIONAL_SIZE];
                apply_g17p_retained_optional_with_flist_generation(
                    stage, addresses(), ordinal, generation, false, 0, 1, 1, 1, 1, &mut raw).unwrap();
                assert_eq!(u64_at(&raw, 0x3e), generation, "ordinal {ordinal}");
                // Per-QID change-detected generation still follows the slot.
                assert_eq!(raw[0x2f], ordinal as u8);
                // +0x46 (hardware-buffer ID) is untouched by the qword at +0x3e.
                assert_eq!(u32_at(&raw, 0x46), addresses().usc_freelist_hardware_buffer_id);
                let mut per_slot = [0xa5; G17P_COLD_OPENING_OPTIONAL_SIZE];
                apply_g17p_retained_optional(stage, addresses(), ordinal, false, 0, 1, 1, 1, 1, &mut per_slot).unwrap();
                if generation == ordinal as u64 {
                    assert_eq!(raw, per_slot, "a single client is byte-identical to the previous encoder");
                } else {
                    let mut expected = per_slot;
                    expected[0x3e..0x46].copy_from_slice(&generation.to_le_bytes());
                    assert_eq!(raw, expected, "only the FList generation may differ");
                }
            }
        }
    }
}

#[test]
fn the_second_slots_first_render_names_the_live_pool_generation() {
    // Slot 1, ordinal 0, on a device whose FList sequence is at 1459.
    let mut raw = [0u8; G17P_COLD_OPENING_OPTIONAL_SIZE];
    apply_g17p_retained_optional_with_flist_generation(
        G17PColdOpeningStage::Tiling, addresses(), 0, 1459, false, 2, 3, 2, 1, 6, &mut raw).unwrap();
    assert_eq!(u64_at(&raw, 0x3e), 1459);
    let mut old = [0u8; G17P_COLD_OPENING_OPTIONAL_SIZE];
    apply_g17p_retained_optional(
        G17PColdOpeningStage::Tiling, addresses(), 0, false, 2, 3, 2, 1, 6, &mut old).unwrap();
    assert_eq!(u64_at(&old, 0x3e), 0, "the per-slot encoder regressed the pool to generation 0");
}
''',
])
with tempfile.TemporaryDirectory(prefix="g17-slot-globals-") as tmp:
    binary = Path(tmp) / "tests"
    subprocess.run(["rustc", "-Awarnings", "--edition=2021", "--test", "-", "-o", str(binary)],
                   input=body, text=True, check=True)
    subprocess.run([str(binary), "--test-threads=1"], check=True)
print("PASS: channel-started byte and FList generation follow the device, not the slot")
