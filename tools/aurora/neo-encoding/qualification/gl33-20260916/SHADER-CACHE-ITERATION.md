# Shader cache iteration qualification — 2026-09-15

The old harness forced `MESA_SHADER_CACHE_DISABLE=true` for every run. An
identical official swizzle case showed a repeatable improvement when normal
Mesa caching was enabled. This is a case-specific measurement, not a promised
speedup for the entire CTS campaign.

Case: `KHR-GL33.texture_swizzle.smoke_access_idx_4_channel_idx_2`, original
64x64 / seed 1 / EGL configuration 1 settings in `cache-ab-session.json`.
Same Neo boot, kernel, Mesa, dedicated Xorg and unmodified CTS binary throughout.

| Run | Cache | CTS seconds | Result |
| --- | --- | ---: | --- |
| `shader-cache-ab-off2-20260915` | Disabled | 24.549 | Pass |
| `shader-cache-ab-first-20260915` | Isolated, first use | 10.554 | Pass |
| `shader-cache-ab-warm-20260915` | Isolated, warm | 10.554 | Pass |
| `shader-cache-ab-return-off-20260915` | Disabled again | 24.581 | Pass |

No GPU fault occurred. First cache run: 7,770 hits / 6 misses. Warm run:
7,776 hits / 0 misses. Turning the cache off again reproduced the original
duration. The shader-heavy case is approximately 2.33x faster with caching.

Additional cache-enabled gates passed:

- `readback-generation-cached-probe-20260915`: 12,000 iterations, 11,988 pixel
  comparisons, zero mismatches/unstable reads, no fault (21.080 seconds).
- `readback-generation-cached-replay-20260915`: all 55 original failure/incomplete
  executions passed, exact original configuration groups, no inventory issues,
  no fault (132.829 seconds). That replay was not faster than its uncached run;
  the benefit is workload-dependent. A transient SSH interruption affected only
  collection. Serial confirmed terminal success; existing results were recovered
  and independently analyzed without repeating GPU work.

## Harness policy

`fast-iteration.py --shader-cache auto` is now the default. Normal iterations
use the isolated cache; shader tracing uses cache-off so compilation evidence
is not hidden by a cache hit. Explicit `off` remains available for cold-compiler
controls. Explicit cache-on plus tracing is rejected.

The cache directory contains both full binary SHA-256 hashes:
`/home/mcruz/.cache/neo-cts-shaders/<mesa-sha256>/<asahi-module-sha256>`.
Any change to either binary selects a different namespace. The receipt records
the effective cache policy and directory. Five host policy/isolation tests pass.
No draw stream, synchronization rule, GLSL version, test list, result threshold
or official CTS executable was changed for this optimization.

The fixed-build full campaign is
`readback-generation-full-cached-gl33-20260915`. The initial cold campaign is
preserved separately as incomplete: 8,285 Pass including setup/no-context work,
47 NotSupported, one interrupted case and 485,983 NotRun. It is not merged into
the new campaign's success count.

## Correct full-run accounting

Collection exposed a preexisting bug: full GL33 analysis selected the GL30
inventory. The harness now chooses and checks inventory by profile before
touching hardware, and full-run success additionally requires all sessions to
be accounted for. The unsupported-profile preflight rejection was exercised.

A fresh official GL33 plan has 50 main sessions x 9,886 cases, one no-context
execution, and 15 configuration results. Both installed case-list hashes match
the host copies. Summary mode's exit 1 reflects unexecuted sessions: this file
is a plan and is not used as passing evidence. The fixed harness is archived in
`cts-harness-generation-cache-20260915.tar.gz`.
