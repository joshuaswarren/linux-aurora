#!/usr/bin/env python3
"""Exercise native G17P render work-key and Shared state allocation invariants."""

from pathlib import Path
import subprocess
import tempfile


root = Path(__file__).resolve().parents[1]
manager = (root / "g17_manager.rs").read_text()

start = manager.index("const G17P_COMMAND_BUFFER_STATE_COUNT")
end = manager.index("#[derive(Debug, Clone)]\nstruct G17PHardwareBufferIdAllocator", start)
allocator = manager[start:end]

for required in [
    "render_command_buffer_states: G17PCommandBufferStateAllocator",
    "render_command_buffer_state_pairs: [Option<[u32; 2]>; 2]",
    "allocate_active_render_command_buffer_states()",
    "release_active_render_command_buffer_states()?",
    "command_buffer_state_words",
]:
    assert required in manager, required

source = f"""
#![allow(dead_code)]
{allocator}

#[test]
fn native_empty_sequence_is_consecutive_and_skips_excluded_state() {{
    let mut allocator = G17PCommandBufferStateAllocator::new();
    for expected in (0u32..62).step_by(2) {{
        let pair = allocator.allocate_pair().unwrap();
        assert_eq!(pair, [expected, expected + 1]);
        assert!(allocator.release_pair(pair));
    }}
    assert_eq!(allocator.allocate_pair(), Some([62, 64]));
    assert!(allocator.release_pair([62, 64]));

    let mut full = G17PCommandBufferStateAllocator::new();
    for _ in 0..31 {{ assert!(full.allocate_pair().is_some()); }}
    assert_eq!(full.allocate_pair(), None);
}}

#[test]
fn exact_completion_identity_prevents_aba_release() {{
    let mut allocator = G17PCommandBufferStateAllocator::new();
    let old = allocator.allocate_pair().unwrap();
    assert_eq!(old, [0, 1]);
    assert!(allocator.release_pair(old));
    allocator.next = 64;
    let replacement = allocator.allocate_pair().unwrap();
    assert_eq!(replacement, [64, 65]);
    assert!(!allocator.release_pair(old));
    assert!(allocator.release_pair(replacement));
}}

#[test]
fn full_words_wrap_but_low_six_state_identity_remains_valid() {{
    let mut allocator = G17PCommandBufferStateAllocator::new();
    allocator.next = u32::MAX - 1;
    let pair = allocator.allocate_pair().unwrap();
    assert_eq!(pair, [u32::MAX - 1, 0]);
    assert_eq!(pair.map(|word| word & 63), [62, 0]);
    assert!(allocator.release_pair(pair));
}}
"""

with tempfile.TemporaryDirectory(prefix="g17-render-work-key-") as tmp:
    binary = Path(tmp) / "tests"
    subprocess.run(
        ["rustc", "--edition=2021", "--test", "-", "-o", str(binary)],
        input=source,
        text=True,
        check=True,
    )
    subprocess.run([str(binary)], check=True)

# Compile and execute the descriptor builder's own exact five-register oracle.
subprocess.run(
    ["python3", str(root / "tests/test_g17_fragment_pb_owner.py")],
    check=True,
)

print("PASS native GART-context work keys and Shared state allocation lifecycle")
