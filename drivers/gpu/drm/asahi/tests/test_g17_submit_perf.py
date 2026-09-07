#!/usr/bin/env python3
"""Compile the submit-path throughput accounting and audit the submit path.

Two halves:

* `g17_perf.rs` carries no kernel types on purpose, so its unit tests are
  compiled and RUN here with rustc rather than merely inspected. They cover the
  poll backoff schedule and every division in the window report, including the
  degenerate windows that used to be a divide-by-zero.
* The submit path itself is audited at source level for the properties a
  throughput change must not break: the completion wait still runs between
  publication and success, the deadline still comes from the same 2000 ms the
  poll-count bound expressed, the staging image is only reused where its
  address never reaches firmware, and no per-submission log line is left
  ungated.
"""
from pathlib import Path
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
perf = (root / "g17_perf.rs").read_text()
drm = (root / "g17_drm.rs").read_text()
resources = (root / "g17_resources.rs").read_text()
manager = (root / "g17_manager.rs").read_text()
asahi = (root / "asahi.rs").read_text()


def block(source, marker):
    start = source.index(marker)
    brace = source.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


# --- the module's own unit tests, actually executed -------------------------
with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / "g17_perf.rs"
    source.write_text(perf)
    executable = Path(directory) / "g17_perf"
    subprocess.run(
        ["rustc", "--edition=2021", "--test", "-A", "dead_code", str(source),
         "-o", str(executable)],
        check=True,
    )
    completed = subprocess.run(
        [str(executable), "--test-threads=1"],
        check=True, capture_output=True, text=True,
    )
ran = re.search(r"(\d+) passed", completed.stdout)
assert ran and int(ran.group(1)) >= 6, completed.stdout
assert "0 failed" in completed.stdout, completed.stdout

# --- the wait is still a wait ----------------------------------------------
wait = block(drm, "    fn wait_render_slot(")
# It must still poll until the firmware says complete, and still fail the
# submission when the pair retired without completing.
assert "poll_deferred_render_completion(render_slot)?" in wait
assert "if completion.complete" in wait and "Err(EIO)" in wait
assert "abandon_deferred_render(render_slot)?" in wait and "Err(ETIMEDOUT)" in wait
# The interval and poll count are observations, never gates: they are read out
# of the completion that was already accepted.
assert wait.index("if completion.complete") < wait.index("render_gpu_interval")
# Backoff, not a flat millisecond, and a wall-clock deadline rather than a
# poll count -- the count no longer bounds anything once the interval varies.
assert "PollSchedule::new" in wait
assert "Delta::from_millis" not in wait
assert "g17p_render_wait_ms" in wait
assert "started.elapsed().as_nanos() >= deadline_ns" in wait

# The deadline default and the poll-count constant must not drift apart: the
# constant is what the bound used to be spelled as.
polls = re.search(r"RENDER_COMPLETION_POLLS: usize = (\d+);", manager)
assert polls and polls.group(1) == "2000", manager[:0]
wait_ms = re.search(r"g17p_render_wait_ms: u32 \{\s*default: (\d+),", asahi)
assert wait_ms and wait_ms.group(1) == polls.group(1), "wait deadline drifted from the poll bound"

# --- every wait loop is bounded on every path ------------------------------
# Replacing a fixed poll count with a deadline makes an unbounded `loop`, so a
# poll outcome that asks to re-poll immediately must still be checked against
# the deadline. Missing that is a kernel spin, not a slow frame.
live = (root / "g17_live_boot.rs").read_text()
for name, source, marker in (
    ("g17_manager", manager, "    pub(crate) fn wait_translated_render_completion("),
    ("g17_live_boot", live, "    fn wait_render_completion_servicing_events("),
):
    body = block(source, marker)
    assert "loop {" in body, name
    retry = body[body.index("RenderPollOutcome::Retry"):]
    retry = retry[: retry.index("Pending")]
    assert "deadline_ns" in retry, f"{name}: the Retry path can spin past the deadline"

# --- ordering: publish, then wait, then declare success --------------------
submit = block(drm, "    fn submit(\n")
publish_at = submit.index("submit_translated_render")
wait_at = submit.index("wait_render_slot")
success_at = submit.index("publication.succeeded()")
record_at = submit.index("self.registry.record_render")
assert publish_at < wait_at < success_at < record_at, "the submit path was reordered"
# Accounting must never be able to turn a failed submission into a recorded
# one: it is guarded by the flag the completed wait sets.
assert "if render_completed {" in submit
assert submit.index("render_completed = true;") < success_at

# --- the retained staging image is scratch only ----------------------------
restage = block(resources, "    pub(crate) fn restage_command(")
assert "self.staging_descriptors.take()" in restage
assert "self.staging_descriptors = Some(next_descriptors);" in restage
# A reused buffer must be zeroed exactly as a fresh allocation is; otherwise a
# field the descriptor builders leave untouched would carry the previous
# submission's bytes.
reset = restage[restage.index("self.staging_descriptors.take()"):]
assert "memset(0)" in reset[: reset.index("};")]
# And its own address must never be published: the only addresses that go into
# the command come from the retained storage and the freshly mapped state.
assert "next_descriptors.iova()" not in restage
assert "render_state_va = next_render_state.iova()" in restage

# --- nothing per-submission logs unconditionally ---------------------------
headers = block(drm, "    fn log_cmdbuf_headers(")
assert headers.index("g17p_verbose()") < headers.index("while offset")
async_submit = block(drm, "    fn submit_compute_async(")
queued = async_submit.index('"G17P async submit: QUEUED')
gate = async_submit.rindex("crate::util::g17p_verbose()", 0, queued)
assert queued - gate < 120, "the async QUEUED line is not behind the verbose gate"

print(
    "PASS submit perf: backoff schedule and window arithmetic execute; "
    "publish/wait/success ordering intact; deadline matches the 2000-poll bound; "
    "staging reuse is scratch-only and zeroed; no ungated per-submission log"
)
