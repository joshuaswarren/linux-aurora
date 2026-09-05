#!/usr/bin/env python3
"""Compile actual fragment scratch codec/staging code with a byte-map adapter."""
from pathlib import Path
import re
import subprocess
import tempfile

source_dir = Path(__file__).resolve().parent.parent
submission = (source_dir / "g17_submission.rs").read_text()
resources = (source_dir / "g17_resources.rs").read_text()
manager = (source_dir / "g17_manager.rs").read_text()
render = (source_dir / "g17_render.rs").read_text()


def block(text, pattern):
    match = re.search(pattern, text, re.M)
    assert match, pattern
    end = text.index("{", match.start()) + 1
    depth = 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[match.start():end]


constants = "\n".join(
    re.search(r"^pub\(crate\) const " + name + r": .*?;", submission, re.M).group()
    for name in ["KICK_TS_BITS", "KICK_TS_MASK", "KICK_QID_SHIFT"]
)
body = "\n".join([
    "#![allow(dead_code)]",
    "type Result<T = ()> = core::result::Result<T, i32>;",
    "const EINVAL: i32 = 22; const EIO: i32 = 5;",
    "mod g17_submission {",
    constants,
    "#[derive(Debug, Copy, Clone, PartialEq, Eq)]",
    block(submission, r"^pub\(crate\) enum SubmissionError\s+"),
    block(submission, r"^pub\(crate\) const fn g17p_serialized_render_scratch_payload\("),
    block(submission, r"^pub\(crate\) const fn encode_g17p_3d_completion_scratch\("),
    "#[test]",
    block(submission, r"^    fn g17p_3d_completion_scratch_matches_native_full_command\("),
    "#[test]",
    block(submission, r"^    fn g17p_serialized_render_scratch_payload_advances_and_wraps_independently\("),
    "}",
    "mod g17_render {",
    re.search(r"^pub\(crate\) const TA_DESCRIPTOR_SIZE: .*?;", render, re.M).group(),
    "#[derive(Debug, Copy, Clone, PartialEq, Eq)]",
    block(render, r"^pub\(crate\) enum RenderBuildError\s+"),
    block(render, r"^fn put_u8\("),
    block(render, r"^fn put_u64\("),
    block(render, r"^pub\(crate\) fn apply_g17p_ta_linked_completion_scratch\("),
    "}",
    block(resources, r"^fn put_u64\("),
    # The production method is verbatim; the test harness supplies only the
    # descriptor field it accesses, backed by a fallible mapped-byte adapter.
    "struct G17PUserRenderStorage { descriptors: MappedObject }",
    "impl G17PUserRenderStorage {",
    block(resources, r"^    pub\(crate\) fn stage_fragment_completion_scratch\("),
    "}",
    r'''
struct MappedObject { bytes: Vec<u8>, accesses: usize, fail: bool }
impl MappedObject {
    fn with_bytes_mut<T>(&mut self, f: impl FnOnce(&mut [u8]) -> Result<T>) -> Result<T> {
        self.accesses += 1;
        if self.fail { return Err(EIO); }
        f(&mut self.bytes)
    }
}
fn storage(size: usize) -> G17PUserRenderStorage {
    G17PUserRenderStorage { descriptors: MappedObject { bytes: vec![0xa5; size], accesses: 0, fail: false } }
}
#[test]
fn actual_staging_changes_only_owned_ta_fields_and_fragment_scratch() {
    let mut s = storage(0x9000);
    for ordinal in [0, 1, 2, 254, 255, 256, 999] {
        let before = s.descriptors.bytes.clone();
        let stamp = ordinal as u64 + 1;
        let payload = g17_submission::g17p_serialized_render_scratch_payload(ordinal);
        let value = s.stage_fragment_completion_scratch(stamp, stamp, payload).unwrap();
        assert_eq!(value & g17_submission::KICK_TS_MASK, stamp);
        assert_eq!(value >> 40, payload as u64);
        let at = 0x4000 + 0x2160;
        assert_eq!(&s.descriptors.bytes[at..at + 8], &value.to_le_bytes());
        assert_eq!(&s.descriptors.bytes[0x7a8..0x7b0], &stamp.to_le_bytes());
        assert_eq!(&s.descriptors.bytes[0x7b0..0x7b2], &[payload; 2]);
        assert_eq!(&s.descriptors.bytes[0x8c6..0x8ce], &value.to_le_bytes());
        for byte in 0..before.len() {
            if !(0x7a8..0x7b2).contains(&byte) && !(0x8c6..0x8ce).contains(&byte)
                && !(at..at + 8).contains(&byte) {
                assert_eq!(s.descriptors.bytes[byte], before[byte], "unexpected write at {byte:#x}");
            }
        }
    }
}
#[test]
fn actual_staging_preserves_distinct_physical_timestamps_and_payload() {
    let mut s = storage(0x8000);
    let ta = 0x12_3456_789au64;
    let fragment = 0x98_7654_3210u64;
    let payload = 7;
    let value = s.stage_fragment_completion_scratch(ta, fragment, payload).unwrap();
    assert_eq!(value, fragment | ((payload as u64) << 40));
    assert_eq!(&s.descriptors.bytes[0x7a8..0x7b0], &fragment.to_le_bytes());
    assert_eq!(&s.descriptors.bytes[0x7b0..0x7b2], &[payload; 2]);
    assert_eq!(&s.descriptors.bytes[0x8c6..0x8ce], &(ta | ((payload as u64) << 40)).to_le_bytes());
    assert_eq!(&s.descriptors.bytes[0x6160..0x6168], &value.to_le_bytes());
}
#[test]
fn staging_rejects_bad_timestamp_or_backing_without_partial_writes() {
    let mut s = storage(0x8000);
    let before = s.descriptors.bytes.clone();
    assert_eq!(s.stage_fragment_completion_scratch(1, 1 << 40, 1), Err(EINVAL));
    assert_eq!(s.descriptors.accesses, 0);
    assert_eq!(s.descriptors.bytes, before);
    // Fragment encoding succeeds but the real TA helper rejects its input:
    // neither descriptor may be partially updated inside the mapped closure.
    assert_eq!(s.stage_fragment_completion_scratch(1 << 40, 1, 1), Err(EINVAL));
    assert_eq!(s.descriptors.accesses, 1);
    assert_eq!(s.descriptors.bytes, before);
    s.descriptors.fail = true;
    assert_eq!(s.stage_fragment_completion_scratch(1, 2, 3), Err(EIO));
    assert_eq!(s.descriptors.bytes, before);
    for size in [0, 0x4000, 0x7fff] {
        let mut short = storage(size);
        let before = short.descriptors.bytes.clone();
        assert_eq!(short.stage_fragment_completion_scratch(1, 2, 3), Err(EINVAL));
        assert_eq!(short.descriptors.bytes, before);
    }
}
''',
])
with tempfile.TemporaryDirectory(prefix="g17-fragment-scratch-") as temp:
    binary = Path(temp) / "tests"
    subprocess.run(["rustc", "--edition=2021", "--test", "-", "-o", str(binary)],
                   input=body, text=True, check=True)
    subprocess.run([str(binary)], check=True)

# The actual runtime must capture the payload from this pass's ordinal and
# feed it to the method; otherwise isolated encoder tests would miss the bug.
assert "G17P_3D_COMPLETION_SCRATCH_PAYLOAD" not in manager + resources + submission
assert "let channel_payload = g17_submission::g17p_serialized_render_scratch_payload(ordinal);" in manager
assert re.search(r"\.stage_fragment_completion_scratch\(\s*tiling_command_timestamp,\s*fragment_command_timestamp,\s*channel_payload,?\s*\)", manager)
assert re.search(r"let tiling_command_timestamp = self\.render_sksm\.as_ref\(\)\.ok_or\(ENODEV\)\?\s*\.tiling\.producer\.current_timestamp;", manager)
assert manager.index("let channel_payload =") < manager.index(".stage_fragment_completion_scratch(")
print("actual fragment scratch codec, byte staging and runtime caller wiring PASS")
