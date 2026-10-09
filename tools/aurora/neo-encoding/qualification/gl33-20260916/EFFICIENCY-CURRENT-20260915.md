# Efficiency evidence for the current GL33 candidate

September 16 update: the original campaign described below **failed**; see
`GL33-TERMINAL-RESULT-20260916.md`. The corrected production kernel's new full
campaign was intentionally stopped on user direction after about 63 minutes,
with no Fail results, to test only the original failures. `HANDOFF.md` records
the unchanged boot and current failure-only replay. Efficiency research is paused.
The separate Mesa candidate remains uninstalled at `67e5e2a1210`.
`PREAMBLE-SCHEDULING-FIELDS-20260916.md` now verifies the native RLD scheduling
field's provenance with 2,464 emulated comparisons. Placement policy, packed
values, and hardware performance are not yet qualified. The older scope and
build results below remain historical evidence, not the current CTS verdict.

Scope: Mesa fix commit `b79a17bbe78` on snapshot `8df91891ab7`, library SHA-256
`362a359bb06f3a1432720bab738f60dd2095f6188cced7d27bcfd8a2f2c0cc56`,
and generation-fixed Asahi module SHA-256
`03abec7b984762e6b32a27951652d4c0445e8300e7447a795e5c45dc5291d762`.
The full campaign was launched on this pair. This review changes neither binary.
See `GL33-PROGRESS-20260915.md` for live campaign checkpoints. The temporary
SSH observation gap recovered at 04:19 UTC on September 16 without a restart.

Separate candidate update: `mesa-j700-agx-sync-20260916` at `cf32dcf7b18` merges
the pinned upstream `33d4d8d9f13` while retaining the preamble work below.
Merge regressions in COPY lifetime and incoming-slot release tracking are fixed;
a production ALU field-array overflow found by the broad suite is also fixed.
The complete candidate and unmodified upstream were built in separate VM trees.
Candidate tests report 277 OK / 18 FAIL / 2 EXPECTEDFAIL, versus upstream
274 / 21 / 2. The multisample follow-up unions coverage kills before one depth
transaction and keeps one colour transaction when zero-coverage sample stores
are eliminated. All 18 nonempty 2x/4x API masks compile and their union passes
4,608 semantic comparisons. Native preamble entry policy and hardware efficiency still require
qualification; no candidate was installed. See `AGX-SYNC-20260916.md`.

## What the two recent fixes establish

The completion-generation fix changes the completion record's generation to
the shared allocator value already used by rendering. The cache fix serializes
and restores component stage and API-owner presence. Neither change adds GPU
barriers, retries, waits, draw splitting, or submissions. The cache remains
enabled; disabling it was a diagnostic control, not the resolution.

These facts establish that the recent fixes do not buy correctness by adding
GPU work. They do not establish efficiency parity for the entire driver.

## Confirmed remaining preamble limitation

Current `src/gallium/drivers/asahi/agx_state.c` still sets `no_preamble` for every
G17P stage. Its graphics builder rejects `has_preamble`/preamble scratch, and
`agx_g17p_append_compute` rejects executable preamble metadata too. Honeykrisp
also disables compiler preambles for G17P. The local M1/M2 design reference,
commit `6dfbc555b4128ee51139c5f78c5aba2594c9701b`, has no corresponding blanket
override at this Gallium call site and enables constant promotion there.
This is a pinned source comparison, not a new upstream fetch or hardware
performance comparison.

Useful implementation infrastructure already exists:

- `agx_ioto_preshader_core` packs Apple's constant-calculation suffix, including
  inline code, external branches, the absent form, and address/size validation.
- `agx_ioto_program_core` composes resource/state loads, shader loading, profile
  state, and that suffix.
- The AGX3 compiler has preamble-specific uniform-store handling, including
  paired half writes. Its existence does not prove every generated program is
  compatible with the state-load execution contract.

After confirming Ninja's targets were current, CPU-only tests returned:

| Test | Result | Artifact |
| --- | --- | --- |
| `asahi-agx-ioto-preshader-core` | 682 checks passed | `preamble-core-cpu-check.log` |
| `asahi-agx-ioto-program-core` | 913 checks passed | `ioto-program-core-cpu-check.log` |

These cover packing, composition, native byte witnesses, and rejection of
invalid inputs. They do not prove shader arithmetic, register handoff, resource
visibility, scratch handling, or execution on the Neo. The helper's input is
compiled Ioto code; arbitrary shader bytes must not simply be wired into it.

The next implementation needs a verified compiler-to-Ioto contract, resource
and register lifetime handling, integration in both graphics and compute paths,
then hardware comparisons with uniform-dependent arithmetic. Keep the current
guard until those requirements are satisfied. Preamble cost and M1/M2-level
efficiency remain unmeasured.

A subsequent CPU compiler comparison is recorded in
`PREAMBLE-COMPILER-PROBE.md`. For one uniform-address workload, enabling G17P
preamble formation reduces main code from 36 to 22 bytes and main GPR count
from 8 to 2, while adding a separate 36-byte preamble and alignment padding.
Both AGX2 and G17P variants compile. No runtime preamble was enabled, and this
does not establish a speedup. General segment bounds and the native handoff
still need verification.

`PREAMBLE-NATIVE-AND-SIZE-AUDIT.md` records subsequent A18/HAL200 static
encoder evidence from the host's macOS 27 driver and metadata commit
`dec426fd7a7e`. Exact preamble lengths are now preserved in full and compact
compiler metadata. The compiler suite and 487 precompiled-kernel checks pass.
This candidate is isolated and not installed; native register/resource handoff
and runtime performance still require qualification.

Follow-up `549b7e7c8a2` integrates preshader suffixes into the shared graphics
and compute program composers, with checked allocation sizes and no new GPU
commands beyond the preamble itself. Graphics and focused compute composition
tests pass. The broader Honeykrisp CPU suite retains an independently reproduced
baseline packed-integer tile-layout failure; it is not counted as passing.
At that commit, Gallium compiler/launch guards remained in place pending upload
and handoff qualification. The full CTS campaign still uses the earlier fixed
binary.

`PREAMBLE-UPLOAD-AUDIT.md` identifies the original compiled BO as the correct
preamble owner, with existing cache-restore and batch-lifetime handling. The
linked main BO must not be used with the original preamble offset. No additional
per-draw allocation or upload is needed for this design.

Regression commit `a3f2a8616a9` adds a 32-input uniform workload that produces
an 834-byte G17P preamble, beyond the inline limit. Its main shrinks from 840
to 36 bytes and from 16 to four 16-bit registers; AGX2's main also becomes
36 bytes/four 16-bit registers. The compiler suite and 487 metadata checks pass
with builder restoration. See `PREAMBLE-EXTERNAL-COMPILER-AUDIT.md`. These are
compiler measurements; runtime input ABI, correctness, and speed remain open.

Subsequent commits add checked original-BO binding (`ba1dc83f997`), reject
incoming invocation-register reads in preambles (`733acb27f15`), and wire that
binding into Gallium graphics/compute construction (`29e2161ec9f`). Full Mesa
builds and focused CPU tests pass; the known broader Honeykrisp failure remains.
No extra GPU upload, allocation, submission, or barrier was introduced for this
wiring. G17P preamble generation remains globally disabled pending hardware
qualification. `2a7391e997e` also makes invalid-descriptor rejection terminate
explicitly in release builds, without changing successful GPU work.

## Test-loop observations

The live full campaign's verbose runner log reached about 391 MB during its
first main session. A 1 MiB sample at offset 40 MiB consisted predominantly of
expected API validation errors from texture-format negative tests. There was
91.21 GiB available on the target filesystem at inspection, so no storage-driven
interruption or deletion was needed.

In the current assertion-enabled Mesa, `src/mesa/main/errors.c:should_output`
accepts `MESA_DEBUG=silent` to suppress stderr duplication of API errors. Error
state and debug callback delivery use separate code paths. In an NDEBUG build
the environment handling differs, so this setting must not be generalized
without checking the actual binary. No environment change was made to the
running campaign, and no speed benefit has been measured. Any future A/B test
must retain QPA results and driver/kernel fault reporting.

`cts-live-progress.py` now caches only completed QPA files whose device, inode,
size, mtime and ctime are unchanged. The cache and latest snapshot are local;
the Neo observer is read-only. Seventeen completed files matched the previous
uncached observer exactly. At 15/50 completed main sessions, a cold scan read
149,465,006 bytes; the next observation reused 17 completed files and read only
8,186,877 bytes from the active file (94.52% fewer bytes). Wall times were
12.882 and 13.300 seconds respectively: no end-to-end latency improvement is
claimed. Final `analyze-run.py` strict XML accounting remains independent of
this progress cache. Use `--no-cache` for a fresh full status scan; observation
failure must never trigger a campaign restart.

## Outstanding acceptance evidence

Latest follow-up: `6743d7ad6c1` removes AGX2 resource counts from G17P vertex
commands and encodes the native IPR/spill fields from compiler metadata. The
conversion matches native ARM arithmetic across 1,064,950 inputs, and 79,622
VDM checks plus the broader relevant CPU suites pass. The full library builds,
remains uninstalled, and does not enable preambles. It must be qualified with
preambles disabled before preamble-on testing. Native input-bearing captures
also reveal a 32-byte thread-invariant spill footprint that needs its entry-ABI
explanation; it must not become an arbitrary minimum reservation.

Native-schema follow-up (01:50 UTC): the launch field is now proven to be
`thread_invariant_largest_ipr_bytes`, separate from temporary GPR count.
All four prior native uniform captures contain real constant programs with zero
IPR, including the long workloads. See `PREAMBLE-NATIVE-AND-SIZE-AUDIT.md`.
Commit `66af913ca3b` preserves separate preamble IPR metadata; the full candidate
build and relevant CPU suites pass, with the documented unrelated Honeykrisp
baseline failure unchanged. The candidate remains uninstalled and generation
remains disabled. Correct launch encoding, nonzero-IPR qualification, input
contract, and runtime efficiency are still open.

Full GL33 execution/accounting, mandatory features on applicable configurations,
and runtime efficiency remain required. Compare batching, commands/barriers,
shader instructions and register use, and representative CPU/GPU timings against
the pinned M1/M2 design, explaining hardware differences. The current focused
passes and CPU packing tests do not close those requirements.

At 2026-09-16 02:43:03 UTC, the unchanged full CTS process was alive at 4:08:35:
263,535 Pass, 7,930 NotSupported, 27/50 main sessions complete, 54.919% of main
executions accounted, and no failures/fault lines or terminal receipt. These
counts are a live checkpoint, not a final XML conformance audit.

The CPU-only input-ABI investigation produced `PREAMBLE-INPUT-ABI-AUDIT.md`:
66 native helper cases, plus strict TPS replay of 33 default copy cases and
86 entry reloads from 17 compiled captures. Native input storage is packed and
grows with input count. The tested native footprint ranges from 0 to 144 bytes;
blindly reserving 32 bytes is incorrect. This provides the mapping needed to
design compiler/loader input handling without per-draw stream splitting or
blanket barriers. Mesa preambles remain disabled and runtime qualification is
still outstanding. The original dirty Mesa/kernel trees and resident were not
changed. Discord delivery remains unavailable: no connector and no configured
progress webhook; no post was claimed.

Candidate `0a7050edd9e` now implements the native default G17P TPS input-copy
primitive in the loader packer. It matches 2,856 native oracle cases, including
partial masks, alignment, boundary registers, and each supported source wait
slot. Full Mesa and the relevant CPU suites pass, with the unchanged known
Honeykrisp packed-integer baseline failure. No runtime call site or preamble
enablement was added. This prepares the actual loader/compiler connection;
that connection, storage accounting, external entry state, stage qualification,
and efficiency measurements still need completion.

The v2 build includes all 24 changed source/build files and nine CPU test
executables, and restores them plus the baseline Mesa library byte-for-byte.
The new candidate's SHA-256 is
`63449d05b801c3e231c7df33b81bf68a820ccd17d6dac69e62b7c8228fc3cf2f`.
The source tree is clean at `0a7050edd9e`; the original dirty trees are unchanged.

New live CTS snapshot at 2026-09-16 03:00:47 UTC: PID 2122 alive at 4:26:17,
281,488 Pass / 8,526 NotSupported, 29/50 sessions complete, session 30 active,
58.672% of executions accounted, no failures/fault lines and no terminal receipt.

Follow-up `d4b97495b74` records actual AGX3 preamble uniform inputs from final
lowered IR, retains known-empty metadata through compact shader info, and
defines the shared packed input layout. The load core passes 3,326 checks;
real compiler fixtures cover sparse 16-/32-/64-bit and dead inputs, including
half 511. Production frame sizes match all 17 pinned native captures. The
existing long shader fixture's instruction sizes remain unchanged.

Full candidate SHA-256:
`24fec79335d18a302c5e7a59cc404676a76b8f637d7fda15b2abf01b80bdbe3c`.
Build ID `70c60f3e0cb6032875d0a3315516be4d215aae58` differs from the prior
candidate; the driver uses it to namespace disk-cache entries, which matters
for the expanded metadata. All 27 overlaid files, ten test executables, and
the baseline Mesa library are restored. The original dirty trees are unchanged.
The metadata/layout is ready for input-read lowering and loader-copy integration;
that integration, producer dependency, storage accounting, implicit entry state,
and hardware/efficiency qualification remain open. Generation remains disabled.

Live CTS revalidated at 2026-09-16 03:18:23 UTC: PID 2122 alive at 4:43:47,
298,229 Pass / 9,103 NotSupported, 31/50 main sessions complete, 32nd active,
62.175% of executions accounted, no failures/fault lines or terminal receipt.

Follow-up `2fed294aa41` now connects packed preamble input copies to the optional
uniform-program body path. It reuses loaded registers, preserves ordinary
uniform uploads, and handles their source lifetimes without new memory loads
or wait instructions. Empty input metadata retains identical body bytes.
CPU tests cover real allocator rollover, unordered ranges, bounds and rollback.
Host UBSan and Linux suites pass (load packer 441, uniform program 484 checks),
as does the full Mesa build, with the unchanged known Honeykrisp baseline issue.
All 30 overlaid files, ten test binaries and baseline Mesa are restored.
Candidate SHA-256 is
`faea8b5f4494a4a80bd17ba4093dcaa8a270c3e91169994c62272f16876227e3`.
It is not installed or enabled; this does not qualify runtime preambles or
efficiency parity. Remaining work is documented in `PREAMBLE-INPUT-ABI-AUDIT.md`.

The last remotely verified CTS checkpoint is 03:20:46 UTC: 302,854 Pass and
9,104 NotSupported, 31/50 main sessions finished, 63.111% accounted, no failures
or fault lines. Later SSH refreshes failed. Host runner session 25708 was still
alive at 03:44 UTC, but remote advancement after the checkpoint is unverified.
No restart or other interference with the campaign was attempted.

Follow-up `8ec7c5b8bee` adds incoming pending producers to the AGX3 scoreboard
ledger. This lets a preamble import the loader's slot-6 TPS results, retire that
generation on its first dependency, and preserve correct later slot reuse. The
1,137-check scoreboard suite passes under host UBSan and in the Linux builder;
full Mesa and relevant CPU suites pass with the same known Honeykrisp issue.
All 33 overlaid files, eleven CPU test binaries and baseline Mesa are restored.
Candidate SHA-256:
`e8dd8480acd1dc5ad05d6244243a2f973e76f96a4541ebd6204b8aa7d98fef23`.
It is not installed and no runtime caller imports this state yet.

Additional offline native captures reject a blanket first-reload structural
flag or fixed slot-2 policy. The incoming ledger reproduces 164 native reload
wait masks across 22 captures given Apple's slot choices; it does not derive
those choices or explain the structural flag. See the expanded input ABI audit.

At 03:59:46 UTC the existing campaign host process was alive at 5:25:40, and its
original Python console process still exclusively owned the present serial
node. The Neo's Wi-Fi address had no ARP entry and did not answer one ping.
Remote CTS progress remains unverified after 03:20:46 UTC. A physical-state
question is pending; no second reader or reboot was attempted.
