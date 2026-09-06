#!/usr/bin/env python3
"""Compile the production render interval validator and audit log ordering."""
from pathlib import Path
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
completion = (root / "g17_completion.rs").read_text()
manager = (root / "g17_manager.rs").read_text()


def block(source, marker):
    start = source.index(marker)
    brace = source.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


state = block(completion, "pub(crate) struct RenderExecutionState")
validator = block(completion, "pub(crate) const fn render_gpu_intervals")
tests = r'''
#[test]
fn valid_pairs_preserve_engine_order() {
    let execution = RenderExecutionState {
        job_list_empty: true,
        tiling_timestamps: [10, 30],
        fragment_timestamps: [20, 40],
    };
    assert_eq!(render_gpu_intervals(execution), Some([[10, 30], [20, 40]]));
}

#[test]
fn incomplete_or_nonmoving_pairs_never_publish() {
    let good = RenderExecutionState {
        job_list_empty: true,
        tiling_timestamps: [10, 20],
        fragment_timestamps: [30, 40],
    };
    assert_eq!(render_gpu_intervals(RenderExecutionState {
        job_list_empty: false, ..good
    }), None);
    for pair in [[0, 20], [20, 20], [21, 20]] {
        assert_eq!(render_gpu_intervals(RenderExecutionState {
            tiling_timestamps: pair, ..good
        }), None);
        assert_eq!(render_gpu_intervals(RenderExecutionState {
            fragment_timestamps: pair, ..good
        }), None);
    }
}
'''

with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / "test.rs"
    source.write_text("#![allow(dead_code)]\n" + state + validator + tests)
    executable = Path(directory) / "test"
    subprocess.run(["rustc", "--edition=2021", "--test", str(source),
                    "-o", str(executable)], check=True)
    subprocess.run([str(executable), "--test-threads=1"], check=True)

poll = block(manager, "    pub(crate) fn poll_translated_render_completion(")
log = poll.index('"G17P render: GPU-INTERVAL')
clear_tracker = poll.index("self.render_tracker = None")
increment = poll.index("self.render_submission_ordinal = next_ordinal")
assert log < clear_tracker < increment
assert "render_gpu_intervals" in poll
assert "binding.vm_id()" in poll and "binding.root()" in poll
assert "queues.tiling.queue_id" in poll and "queues.fragment.queue_id" in poll
assert len(re.findall(r"GPU-INTERVAL", poll)) == 1
print("PASS render GPU interval publication: exact forward pairs, invalid retirement rejection, VM/root/QID identity, pre-clear/pre-increment ordering")
