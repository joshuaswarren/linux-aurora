#!/usr/bin/env python3
"""A recovery-blamed settle must be explained, and must not be asked the wrong question.

`retire_abandoned_render_if_firmware_retired` reaches its `true` through
`!completion.retired || !state.job_list_empty || execution.observed()`, and
`observed()` CONTAINS `job_list_empty`. The guard therefore demands
`job_list_empty == true` and `observed() == false` simultaneously, which is
satisfiable only when at least one timestamp is zero. It is a nop-retire
classifier -- it can only ever answer `true` for a group that did NOT execute.

The recovery-blamed settle asks it anyway. A stamp-blamed recovery serviced with
a live render scheduler deliberately skips the generation-0 timestamp wipe "so
the render's progress markers survive", so the timestamps are non-zero by
design, `observed()` is true, and `firmware-retired=false` is arithmetic rather
than evidence -- after which the caller poisons the client's VM and marks the
session crashed.

Two properties are pinned here: the settle path is always explained, and the
relaxation keeps BOTH ownership proofs so it is inert on the outcome that would
refute the diagnosis.
"""

from pathlib import Path
import subprocess
import sys
import tempfile


root = Path(__file__).resolve().parents[1]
live = (root / "g17_live_boot.rs").read_text()
manager = (root / "g17_manager.rs").read_text()
completion = (root / "g17_completion.rs").read_text()
if "--negative-control" in sys.argv:
    manager = manager.replace("|| (execution.observed() && !accept_executed)",
                              "|| execution.observed()")


def item(text: str, marker: str) -> str:
    start = text.index(marker)
    pos = text.index("{", start) + 1
    depth = 1
    while depth:
        depth += (text[pos] == "{") - (text[pos] == "}")
        pos += 1
    return text[start:pos]


retire = item(live, "    fn retire_failed_deferred_render(")
decide = item(manager, "    pub(crate) fn retire_abandoned_render_if_firmware_retired(")
observed = item(completion, "    pub(crate) fn observed(")

# --- the classifier runs on BOTH settle paths, and says which one ---------
assert "report_render_wait_timeout(" in retire
report = retire.index("report_render_wait_timeout(")
assert retire.index('"recovery-blamed"', report) > report
assert retire.index('"inline-wait-expired"', report) > report
# and it is no longer gated behind the timed-out branch
assert "if timed_out {\n            self.manager" not in retire

# --- the relaxation drops observed() and NOTHING else --------------------
assert "|| (execution.observed() && !accept_executed)" in decide
assert "!completion.retired" in decide          # cursors at the published target
assert "|| !state.job_list_empty" in decide     # unlinked from the firmware list
# only the recovery-blamed path may pass it, and only under the probe
assert "!timed_out" in retire
assert "g17p_render_recovery_retire" in retire

# --- the premise the whole argument rests on -----------------------------
assert "job_list_empty" in observed
assert "tiling_timestamps" in observed and "fragment_timestamps" in observed

rust = r'''
#[derive(Clone, Copy)]
struct Execution { job_list_empty: bool, tiling: [u64; 2], fragment: [u64; 2] }

impl Execution {
    fn observed(self) -> bool {
        self.job_list_empty
            && self.tiling.iter().all(|v| *v != 0)
            && self.fragment.iter().all(|v| *v != 0)
    }
}

// The guard, both modes.
fn retires(retired: bool, job_list_empty: bool, e: Execution, accept_executed: bool) -> bool {
    !(!retired || !job_list_empty || (e.observed() && !accept_executed))
}

const EXECUTED: Execution =
    Execution { job_list_empty: true, tiling: [1, 1], fragment: [1, 2] };
const NOP_RETIRED: Execution =
    Execution { job_list_empty: true, tiling: [0, 0], fragment: [0, 0] };
const STILL_LINKED: Execution =
    Execution { job_list_empty: false, tiling: [1, 1], fragment: [1, 2] };

#[test]
fn strict_mode_can_only_ever_accept_a_group_that_did_not_execute() {
    // The proof: observed() contains job_list_empty, so the guard needs
    // job_list_empty true AND observed() false at once.
    assert!(!retires(true, true, EXECUTED, false));
    assert!(retires(true, true, NOP_RETIRED, false));
}

#[test]
fn the_relaxation_accepts_the_executed_group() {
    assert!(retires(true, true, EXECUTED, true));
}

#[test]
fn the_relaxation_is_inert_when_the_firmware_still_holds_the_group() {
    // This is the outcome that would refute the diagnosis. Retaining the
    // job_list_empty term makes the parameter change nothing in that case.
    assert!(!retires(true, false, STILL_LINKED, true));
    assert!(!retires(true, false, STILL_LINKED, false));
}

#[test]
fn the_relaxation_never_overrides_the_cursor_proof() {
    // completion.retired is "both queue cursors at the published target".
    assert!(!retires(false, true, EXECUTED, true));
    assert!(!retires(false, true, NOP_RETIRED, true));
}

#[test]
fn nop_retire_still_works_in_the_relaxed_mode() {
    assert!(retires(true, true, NOP_RETIRED, true));
}
'''

with tempfile.TemporaryDirectory() as directory:
    directory = Path(directory)
    source = directory / "settle.rs"
    source.write_text(rust)
    executable = directory / "settle"
    subprocess.run(
        ["rustc", "--edition=2021", "--test", str(source), "-o", str(executable)],
        check=True,
    )
    subprocess.run([str(executable), "--test-threads=1"], check=True, timeout=60)

print(
    "PASS recovery-blamed settle: explained on both paths; the relaxation drops "
    "only observed() and stays inert while the firmware still holds the group"
)
