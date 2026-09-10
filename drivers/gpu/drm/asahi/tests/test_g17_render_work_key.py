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
    "render_command_buffer_state_pairs: [Option<u32>; 2]",
    "allocate_active_render_command_buffer_state()",
    "release_active_render_command_buffer_states()?",
    "command_buffer_state_words",
]:
    assert required in manager, required

source = f"""
#![allow(dead_code)]
{allocator}

#[test]
fn native_empty_sequence_is_shared_by_the_pair_and_skips_excluded_state() {{
    let mut allocator = G17PCommandBufferStateAllocator::new();
    for expected in 0u32..63 {{
        let word = allocator.allocate_one().unwrap();
        assert_eq!(word, expected);
        assert!(allocator.release_one(word));
    }}
    assert_eq!(allocator.allocate_one(), Some(64));
    assert!(allocator.release_one(64));

    let mut full = G17PCommandBufferStateAllocator::new();
    for _ in 0..63 {{ assert!(full.allocate_one().is_some()); }}
    assert_eq!(full.allocate_one(), None);
}}

#[test]
fn exact_completion_identity_prevents_aba_release() {{
    let mut allocator = G17PCommandBufferStateAllocator::new();
    let old = allocator.allocate_one().unwrap();
    assert_eq!(old, 0);
    assert!(allocator.release_one(old));
    allocator.next = 64;
    let replacement = allocator.allocate_one().unwrap();
    assert_eq!(replacement, 64);
    assert!(!allocator.release_one(old));
    assert!(allocator.release_one(replacement));
}}

#[test]
fn full_words_wrap_but_low_six_state_identity_remains_valid() {{
    let mut allocator = G17PCommandBufferStateAllocator::new();
    allocator.next = u32::MAX - 1;
    let word = allocator.allocate_one().unwrap();
    assert_eq!(word, u32::MAX - 1);
    assert_eq!(word & 63, 62);
    assert!(allocator.release_one(word));
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
