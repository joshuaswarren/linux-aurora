# Cached geometry component identity

## Root cause and production fix

The GL33 full campaign `readback-generation-full-cached-gl33-20260915`
aborted in its second configuration at `KHR-GL33.cull_distance.coverage`.
An isolated replay with the exact 113x47, seed 2, EGL configuration 1 settings
also aborted with caching enabled (exit 134, 1.926 s), while caching disabled
passed (1.930 s). Neither run had a kernel fault.

The initial message, `compute texture resources has no proven native descriptor
encoding on G17P`, did not identify the actual defect. The compute image shader
itself ran successfully. A subsequent cached geometry helper was misidentified.

In `agx_compile_variant`, `pre_gs` is compiled with stage `MESA_SHADER_COMPUTE`
and retains a null `so` pointer. `read_shader` instead unconditionally restored
every component with `uncompiled->type` and the parent's `so` pointer. For a
geometry variant, this changed the helper's stage from compute (5) to geometry
(3) and attached API resource bindings it never had.

The helper dispatch uses the compute stage. Descriptor updates using the wrong
restored stage therefore failed to clear old compute image state. The guard
correctly refused that inconsistent state.

The production fix serializes each component's actual stage and the presence of
its API shader owner, restoring both on a cache hit. It changes no shader code,
resource encoding, GPU barriers, draw grouping, or submission counts. The existing
Mesa build-ID cache namespace invalidates entries using the old serialized format.
The test harness also isolates caches by the complete Mesa and kernel hashes.

Isolated worktree: `mesa-j700-gs-cache-20260915`, branch
`j700-gs-cache-20260915`, baseline snapshot `8df91891ab7`, fix `b79a17bbe78`.
The original `mesa-j700` dirty worktree is unchanged.

## Evidence

The logging-only probe `8aa390f46dbee9fcd6e096058b706afaeabfad4b1536cf990de00e1513635455`
recorded:

```
fresh write: stage=5 texflag=0 storage=0 size=698 push=2
cached read: stage=3 texflag=0 storage=0 size=698 push=2
cached dispatch: stage=3 texflag=0 storage=0 tex=0 img=0 base=5fffe1c040 samplers=5fffe1c080
```

The earlier image-writing shader had stage 5, texture/storage flags both 1,
and completed its dispatch in both runs. The pre-GS helper then aborted only
on the warm-cache run. Artifacts:
`fast-iterations/cull-cache-metadata-first-20260915` and
`fast-iterations/cull-cache-metadata-warm-20260915`.

The production library has SHA-256
`362a359bb06f3a1432720bab738f60dd2095f6188cced7d27bcfd8a2f2c0cc56`.
It contains no diagnostic logging. Its build changed only `agx_disk_cache.c`
from the qualified normal Mesa snapshot. The original builder source and binary
were restored and their hashes checked after extracting the candidate.

Real J700 hardware results with the fixed library and generation-fixed kernel:

| Run | Scope | Result |
| --- | --- | --- |
| `cull-cache-identity-first-20260915` | All 218 cull-distance cases, cold cache | 202 Pass, 16 NotSupported, 5.927 s; 440 hits / 279 misses |
| `cull-cache-identity-warm-20260915` | Same 218 cases and settings, warm cache | Identical results, 5.659 s; 719 hits / 0 misses |
| `cache-identity-readback-20260915` | 12,000 iterations, 11,988 pixel comparisons | Pass; zero mismatches or unstable reads, 22.887 s |
| `cache-identity-failure-replay-20260915` | All 55 original failing/incomplete executions, original settings | 55 Pass, zero missing/extra cases, 137.608 s |

Both cull runs include a passing `cull_distance.coverage`. Their full ordered
case/status lists match exactly. The 16 NotSupported cases are the previously
audited item-8 nonpoint combinations, not replacements for failing cases.
No kernel faults occurred. The replay analyzer reports no accounting issues and
`conformance_passed: false`, as appropriate for a diagnostic subset. A new full
campaign, `cache-identity-full-gl33-20260915`, was launched with this fixed Mesa,
the generation-fixed kernel, shader caching enabled, and the strengthened fault
matcher. Its counts must remain separate from the older aborted campaign.
Full conformance remains unproven.

Full-campaign follow-up: strict XML parsing confirms a complete Pass for
`KHR-GL33.cull_distance.coverage` in the second session, where the original
campaign aborted. All 9,886 ordered case/status pairs in the completed first
session exactly match the earlier build's first session. This proves the fix
survives the preceding full-session workload; it does not substitute for the
remaining sessions.

## Fault supervision

`cts_faults.py` now supplies the shared case-insensitive fatal-message matcher
used by the runner, candidate installer, and clean-boot preparation. It includes
the actual `GPU firmware crashed` and `GPU fault nya~!!!!!` driver messages that
the old case-sensitive matcher missed. Eleven fault/cache/collector tests pass.
The fast-iteration upload includes this dependency. Manual clean-boot preparation
must upload `cts_faults.py` alongside `prepare-clean-boot.py`.
The deployed scripts are frozen in `cts-harness-cache-identity-20260915.tar.gz`.
Archive SHA-256: `db8e78cc4b77353b63e18c1dc76a346b948f8d7d3a6d926e2c11aa83f99042b3`.

This changes future supervision, not the evidence produced by previous runs.
The terminal full campaign was independently scanned for the expanded signatures
and had no matches. Collection of a real firmware crash remains unqualified.
