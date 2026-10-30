#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Host test for the G18 compute stream limit.

1. Builds the real g18_compute_stream.rs (pure, no kernel types) with `rustc --test` and runs its unit tests.
2. Checks the wiring by text, because the adapter itself needs the kernel build: the module parameter exists with the
   default 48, probe refuses a bad value before MMIO, the config carries it, and admit_compute uses the helper and no
   longer compares against a bare constant.
No hardware is involved.
"""
from pathlib import Path
import re
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[1]
failures = []


def check(ok, label):
    print(("ok   " if ok else "FAIL ") + label)
    if not ok:
        failures.append(label)


with tempfile.TemporaryDirectory() as tmp:
    binary = Path(tmp) / "g18_compute_stream_test"
    build = subprocess.run(["rustc", "--edition", "2021", "--test", str(root / "g18_compute_stream.rs"), "-o", str(binary)],
                           capture_output=True, text=True)
    check(build.returncode == 0, "g18_compute_stream.rs builds with rustc --test")
    if build.returncode != 0:
        print(build.stderr)
    else:
        run = subprocess.run([str(binary)], capture_output=True, text=True)
        print(run.stdout.strip().splitlines()[-1] if run.stdout.strip() else run.stderr)
        check(run.returncode == 0, "the unit tests pass")
        check("5 passed" in run.stdout, "all five unit tests ran")

asahi = (root / "asahi.rs").read_text()
m = re.search(r"g18_compute_stream_bytes:\s*u32\s*\{\s*default:\s*(\d+),", asahi)
check(m is not None and m.group(1) == "48", "module parameter g18_compute_stream_bytes exists and defaults to 48")
check(re.search(r"^\s*mod g18_compute_stream;", asahi, re.M) is not None, "the module is declared in asahi.rs")

driver = (root / "driver.rs").read_text()
check("g18_compute_stream::limit_from_param" in driver and "g18_compute_stream_bytes" in driver,
      "probe validates the parameter")
check(re.search(r"limit_from_param[^\n]*\n[^\n]*\n?[^\n]*EINVAL", driver) is not None
      or re.search(r"limit_from_param\(.*\)\.is_none\(\)", driver) is not None,
      "a refused value returns EINVAL")
check("compute_stream_bytes:" in driver, "the config receives the parameter")

drm = (root / "g18_drm.rs").read_text()
admit = drm[drm.index("fn admit_compute"):drm.index("let resource_descriptor")]
check("g18_compute_stream::admits(" in admit and "effective_limit(" in admit, "admit_compute uses the helper")
check("> G18_COMPUTE_STREAM_BYTES" not in admit, "admit_compute no longer compares against the bare constant")
check("pub(crate) compute_stream_bytes: u64" in drm, "G18DrmConfig carries compute_stream_bytes")
check('"one dispatch,"' in admit and "profile admits {} {}" in admit, "the default log text stays profile admits one dispatch, 48")

sys.exit(1 if failures else 0)
