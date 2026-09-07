#!/usr/bin/env python3
"""Exercise the actual retained-FList release lifecycle and admission gate."""
from pathlib import Path
import re
import subprocess
import tempfile

src = Path(__file__).resolve().parent.parent
manager = (src / "g17_manager.rs").read_text()
submission = (src / "g17_submission.rs").read_text()
initdata = (src / "g17_initdata.rs").read_text()
runtime = (src / "g17_live_boot.rs").read_text()


def block(source, pattern):
    match = re.search(pattern, source, re.M)
    assert match, pattern
    at = source.index("{", match.end())
    depth, end = 1, at + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[match.start():end]


def constant(source, name):
    match = re.search(r"(?:pub\(crate\) )?const " + name + r": .*?;", source, re.S)
    assert match, name
    return match.group()


derive = "#[derive(Debug, Copy, Clone, PartialEq, Eq)]\n"
submission_part = "\n".join([
    *[constant(submission, name) for name in [
        "G17P_USC_FREELIST_LOW_END", "G17P_USC_FREELIST_GROW_DESCRIPTOR_BYTES",
    ]],
    block(submission, r"^pub\(crate\) const fn g17p_render_flist_generation\("),
])
initdata_part = "\n".join(constant(initdata, name) for name in [
    "CONTROL_SHARED_ADDRESS", "CONTROL_OPERAND_TABLE_ADDRESS",
    "PARTIAL_OPENING_FREELIST_HARDWARE_BUFFER_ID",
])
lease_part = "\n".join([
    *[constant(manager, name) for name in [
        "G17P_BOOTSTRAP_UMA_SEQUENCE", "G17P_BOOTSTRAP_UMA_HARDWARE_BUFFER_ID",
        "G17P_BOOTSTRAP_UMA_STAMP", "G17P_BOOTSTRAP_UMA_COOKIE",
        "G17P_BOOTSTRAP_UMA_COUNT",
    ]],
    *[derive + block(manager, r"^(?:pub\(crate\) )?(?:struct|enum) " + name + r"\s*(?=\{)")
      for name in ["G17PBootstrapUmaCompletion", "G17PBootstrapUmaRelease",
                   "G17PBootstrapUmaRequestState", "G17PBootstrapUmaLifecycleError",
                   "G17PBootstrapUmaLifecycle"]],
    block(manager, r"^impl G17PBootstrapUmaLifecycle\s*"),
    *["#[test]\n" + block(manager, r"^    fn " + name + r"\(\)\s*") for name in [
        "bootstrap_async_flist_releases_only_after_type13_and_paired_render",
        "bootstrap_async_flist_release_backpressure_tracks_exact_producer",
        "shared_flist_holds_both_render_pairs_until_the_last_retirement",
        "retained_flist_leases_grow_once_and_release_every_captured_generation",
    ]],
])
body = ("#![allow(dead_code)]\nmod g17_submission {" + submission_part +
        "}\nmod g17_initdata {" + initdata_part + "}\n" + lease_part)
with tempfile.TemporaryDirectory(prefix="g17-flist-backpressure-") as temp:
    binary = Path(temp) / "tests"
    subprocess.run(["rustc", "--edition=2021", "--test", "-", "-o", str(binary)],
                   input=body, text=True, check=True)
    subprocess.run([str(binary)], check=True)

settle = block(runtime, r"^    fn settle_render_usc_freelist_release_before_submission\(")
assert "pending_render_usc_freelist_release_producer" in settle
assert "complete_render_usc_freelist_release_if_consumed(target)?" in settle
assert "for poll in 0..CONTROL_COUNTER_POLLS" in settle
submit = block(runtime, r"^    pub\(crate\) fn submit_translated_render\(")
assert submit.index("settle_render_usc_freelist_release_before_submission()?") < submit.index("settle_open_recovery_before_publication()?")
assert submit.index("settle_render_usc_freelist_release_before_submission()?") < submit.index(".begin_translated_render(")
print("retained FList release target and bounded next-admission backpressure PASS")
