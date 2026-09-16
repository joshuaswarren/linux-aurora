# Full CTS accounting checks

`analyze-run.py` now requires one valid begin/end session pair, with every QPA
case marker inside it. Empty configuration results cannot satisfy completeness.
The final official XML must match the pinned profile, configuration filename,
and complete multiset of test filenames and command-line tokens. Duplicate
planned filenames are rejected. Missing or malformed official XML is an issue.

`fully_accounted` now includes completed-session checks directly. It still
means complete accounting, not a passing driver. `official_conformant` records
the official XML verdict separately, and `official_inventory_matches` records
the independent inventory comparison. Failure/NotSupported counts remain
unchanged and separate. Kernel faults, receipt identity and unsupported-case
applicability still require the existing final qualification review.

`test-analyze-run.py` passes ten synthetic regressions: complete accounting,
missing session end, empty configurations, changed seed, duplicate case,
truncated result, wrong profile, missing summary, duplicate inventory, and a
complete but failing run. This is host-only analysis; no CTS workload changed.
Output: `accounting-regression-test.log`.

An additional check reanalyzed the real archived `dj-agx3-full` run in a
temporary directory using symlinked read-only inputs, preserving its original
analysis. Counts are identical to its previous analysis: 40,614 Pass, 3,152 Fail,
150 InternalError. Its inventory matches and it is fully accounted for, while
its official conformant flag is false. This confirms that complete accounting
is not converted into a pass and that real QPA boundaries remain accepted.

The live host runner invokes this analyzer after collection, so the stricter
accounting applies when its current hardware run completes. The final audit
must still inspect the official verdict and receipt; an aggregate percentage
or a helper exit code alone cannot establish OpenGL 3.3 qualification.
