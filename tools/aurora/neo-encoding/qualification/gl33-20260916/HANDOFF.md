# J700 GL 3.3 Conformance — Handoff

**Latest user steering, September 16:** test only the previous failures.
The full rerun was deliberately stopped with SIGTERM after 3,760.8 seconds;
it had no Fail results, and its exit 143/incomplete coverage reflect this
intentional stop. Its evidence was collected and the kernel was healthy.
The same-boot `stamp-wrap-failures-only-20260916` replay **PASSED all 1,312
original failures**, with original settings, in 180.2 seconds including
supervision (176.9 seconds in CTS). Exact accounting has zero issues and all
three saved kernel logs independently rescan clean and complete, with zero
overruns. Exit is 0; binaries and boot are unchanged. Keep Mesa/kernel frozen; the
offline compiler investigation remains paused. Do not resume a broad CTS
campaign automatically or treat a replay pass as full conformance.

**Current checkpoint, 2026-09-16: OPENGL 3.3 CTS DID NOT PASS.**
`cache-identity-full-gl33-20260915` is terminal: all 50 main runs finished,
with **478,322 Pass, 14,666 NotSupported, and 1,312 Fail**, plus 16 separate
setup/no-context passes. The official runner exited 1 and reports
`Conformant="False"`. Complete accounting is not successful conformance.

The old detector missed the firmware rebuild failure. The original receipt's
`kernel_fault: false` is incorrect; preserve it as historical evidence, never
repeat it as the current conclusion. The stream records
`session-crashed reason=pause-submissions-for-rebuild`, a failed RTKit boot,
then rejected submissions. The corrected live observer and final analyzer now
use the shared detector and report missing logs as incomplete evidence.

The old failed boot has been recovered. A short **hardware completion-wrap
diagnostic passed** on boot `a888fccf-9641-467e-84eb-5c9356f53d65`: 768 draws,
4,078,848 correctly read pixels, and QID 3 completion values through zero.
Both preserved kernel logs independently rescan clean, with no lost records.
This tests completion-value wrap; it does not prove a full CTS pass or the
long-run hardware queue lifetime. The original rebuild trigger is unproven.
See `STAMP-WRAP-QUALIFICATION-20260916.md` for hashes, scope and evidence.

Production kernel candidate `378b14ac9d89` preserves 40-bit timestamps and
dependency epochs, uses wrapping completion values and removes the artificial
24-bit restart threshold. Branch `j700-render-stamp-wrap-20260916` lives in
`neo-encoder-linux-j700-stamp-wrap-20260916`. A separate diagnostic branch
`j700-render-event-wrap-probe-20260916` retains the test fixture. The production
module is loaded on fresh boot `94c3f079-27bc-456d-93a2-5db1e7ed2171`;
the loaded build note and module hash match. The first production reboot's
resident preflight timed out before upload; the verified J516cAP/hpm0
macvdmctl recovery and qualified boot retry succeeded. The 1,312-case replay
`stamp-wrap-replay-20260916` PASSED all 1,312 cases in 179.7 seconds with the
original failed settings (configuration 64, 113x47, seed 2, EGL). Exact replay
accounting has no issues; all three preserved kernel logs independently scan
clean and complete. This subset does not clear an order-dependent failure.
The complete official 50-configuration campaign
`stamp-wrap-full-gl33-20260916` was deliberately stopped on user direction
after about 63 minutes, with no Fail results. Exit 143, one incomplete case,
and unrun coverage are cancellation evidence, not a complete CTS verdict.
The failure-only replay is now terminal and the host wrapper has exited.
Do not treat successful replay or a live process as full CTS pass.
At 08:27 UTC on September 16, the observer counted 52,537 Pass and 1,482
NotSupported, with 5/50 main configurations complete and no case failures.
The available kernel stream was complete and clean. This is a timestamped
interim observation; rerun the observer for current state.
Read `GL33-TERMINAL-RESULT-20260916.md` and the corresponding terminal-audit
JSON for the final comparison: all 1,312 failed cases pass in each of the other
49 configurations, and all 14,666 unsupported executions are explained.

Installed Mesa SHA-256:
`362a359bb06f3a1432720bab738f60dd2095f6188cced7d27bcfd8a2f2c0cc56`.
Kernel module loaded and verified for production qualification:
`51541904ae00503eb3c44d6bf5356e206ce7085296d047d489452247977b1594`.
The original full-run module `03abec7b9847...` and diagnostic `fc3c73675550...`
are backed up. The diagnostic one-shot service has been disabled and removed.
Only J700/T8140 is in scope; do not touch the M5/J813 Air.

Separate candidate `mesa-j700-agx-sync-20260916` is clean at `67e5e2a1210`,
merging pinned upstream `33d4d8d9f13` with the earlier preamble infrastructure.
Its full production build through `cf32dcf7b18` reports 277 OK / 18 FAIL /
2 EXPECTEDFAIL, versus upstream 274 / 21 / 2. A subsequent test-only precompiled
ABI correction passes all 584 checks; that full suite has not been rerun.
The production Mesa SHA is
`5a0b1411802d988042aa0d09fa2ad6e8fe4aef781e80838c6fe1873c57735ad0`.
It is not installed or hardware-qualified. Runtime preambles remain disabled.
Host-only follow-up `PREAMBLE-SCHEDULING-FIELDS-20260916.md` identifies the
previously unexplained RLD field as native scheduling metadata, separate from
dependency waits. A bounded ARM64 oracle passes 2,464 comparisons between
compiler and named driver field mappers. Packed values, placement policy,
and hardware efficiency still need qualification; no runtime change was made.
See `AGX-SYNC-20260916.md`, `PRECOMPILED-ABI-AUDIT-20260916.md`, and
`GL33-PROGRESS-20260915.md` for current evidence and remaining qualification.
The final host analyzer now verifies session boundaries and the exact official
inventory; thirteen synthetic regressions and one preserved real archive pass the
accounting audit. See `FINAL-ACCOUNTING-AUDIT-20260916.md`. Completeness remains
separate from the official verdict and hardware qualification.
The new desktop GL sparse-mask/depth diagnostic passes 65,536 software sample
observations at 4x; software rounds 2x to 4x and it is explicitly untested.
Its compiled executable is staged locally for baseline/candidate hardware A/B
after CTS. See `GL33-SPARSE-DEPTH-QUALIFICATION.md`. The shared coverage-hoist
helper currently has only a Vulkan caller; do not credit its CPU tests as
Gallium or GL 3.3 hardware coverage.

`audit-gl33-applicability.py` now checks completed main QPAs against the pinned
CTS skip predicates and recorded context capabilities. The current campaign's
first window/pbuffer pair contains 19,176 Pass and 596 explained NotSupported,
with no applicability issues. All 1,390 swizzle predicate checks agree; the 18
depth tests pass on the depth-bearing pbuffer. Seven positive/negative controls
pass. Run it over the entire collected final directory after the campaign
ends; two samples do not qualify the remaining configurations. See
`GL33-APPLICABILITY-AUDIT.md` and `gl33-applicability-current.json`.
The candidate/kernel UAPI and limited synchronization source review are in
`AGX-KERNEL-INTERFACE-AUDIT-20260916.md`; neither is runtime qualification.

**Earlier 2026-09-15 cached full campaign (superseded):**
`readback-generation-full-cached-gl33-20260915`, host tool session **16278**,
native runner/runuser PID **1534**,
log `readback-generation-full-cached-gl33-host.log`. Same boot
`9f1434bf-725b-4ff1-ae51-babd4df25cb4`, kernel `03abec7b9847`, Mesa `2b7e6eec6ce4`,
Xorg 717. Do not restart; first inspect the existing host handle and remote
`process.json` / actual PID. The prior full uncached run is terminal/partial.
Latest verified live snapshot: 8,253 main Pass, 43 NotSupported, no failures;
16 separate configuration/no-context passes. PID 1534 was live at elapsed
4:59 and the host handle remained running. These are partial counts only.

Shader-cache qualification: identical swizzle case A/B/B/A =
24.549 / 10.554 / 10.554 / 24.581 seconds, all Pass, no faults. Cache first run:
7,770 hits / 6 misses; warm: 7,776 hits / 0 misses. Cached readback also passed
12,000 iterations / 11,988 comparisons with zero stale reads. Cached original
failure replay: 55/55 Pass, no missing/duplicate cases or faults. A transient
SSH failure interrupted collection only; serial proved the job had completed,
and the saved results were recovered without rerunning. Cache defaults now
`auto`: isolated for normal runs, off for shader tracing. Five cache-policy
unit checks pass; the exact Mesa+kernel hashes form the cache namespace.

Fresh `hardware-inventory/x11-gl33-inventory-auto` is captured and validated:
50 main sessions x 9,886 cases + one no-context execution, plus 15 configuration
results. Both installed case-list hashes match the host. Summary mode returns
1 because this is a plan, not executed conformance; no pass claim is inferred.
Full-run analysis now routes to the correct profile inventory and requires
all sessions accounted for. Wrong-profile preflight rejection was tested.

Cold partial accounting: 8,285 Pass (includes 16 setup/no-context results),
47 NotSupported, 1 deliberately interrupted case, 485,983 NotRun. This is not
a completed campaign. `GL33-APPLICABILITY-AUDIT.md` records verified skip
conditions. Actual CTS info reports GL 4.6 Core and advertised integer-mix and
stencil-texturing; source audit resolved the apparent NS discrepancy: these
particular cases gate on GL33/GLSL330 test metadata rather than extension
presence. Integer-mix extension/functional variants passed. See
`SHADER-CACHE-ITERATION.md` for the complete speed experiment and harness gates.
Runtime Discord webhook is unset; pending payload saved,
not sent. Do not recover webhook credentials from files.

**Earlier iteration-speed correction checkpoint:** The first full fixed-build
run was deliberately interrupted via the owned CTS PID after over 8,000 main
passes with no failures. Its unmodified official QPA evidence is preserved;
it is a partial uncached run, never a full pass. The runner forced
`MESA_SHADER_CACHE_DISABLE=true`; swizzle cases took roughly 23-24 seconds each.
The harness now has an explicit `--shader-cache isolated` option, with a cache
namespace containing both exact Mesa and kernel SHA-256 hashes and receipt
fields recording the policy. Default remains off pending an identical-case
A/B comparison. Cache isolation unit checks: 4 pass. The first A/B setup was
rejected for QPA logging flags in the strict session parser, before CTS started;
the session now contains only the accepted original rendering/config settings.
Current A/B label: `shader-cache-ab-off2-20260915`.

Collection also exposed an existing harness bug: full GL33 analysis was routed
to the GL30 inventory. The runner now selects per-profile inventory and checks
its type before starting. A fresh `x11-gl33-inventory-auto` still needs to be
captured before restarting the full campaign. Current kernel/Mesa/Xorg remain
the qualified generation-fix combination below. No reboot or resident change.

**Earlier full fixed-build campaign start (now deliberately interrupted):**
`fast-iteration.py readback-generation-full-gl33-20260915 --profile gl33 --full --console-timeout 86400`
is live (host tool session 75298, native runner/runuser PID 952). It owns the
J700 console; do not open another serial owner or restart on an observation
timeout. Inspect the existing process and terminal receipt instead.
Boot `9f1434bf-725b-4ff1-ae51-babd4df25cb4`, Xorg 717, candidate kernel
`03abec7b984762e6b32a27951652d4c0445e8300e7447a795e5c45dc5291d762`, unchanged Mesa
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
Host log: `readback-generation-full-gl33-host.log`.
Remote output: `/home/mcruz/gl30-cts-20260913/9f1434bf-725b-4ff1-ae51-babd4df25cb4/readback-generation-full-gl33-20260915`.
Early live QPA snapshot: 4,480 Pass, 1 NotSupported, no failures; this includes
setup/configuration results and is not a final coverage denominator.
No conformance claim until the entire pinned inventory is accounted for.

**2026-09-15 generation-fix hardware checkpoint:** Candidate module `03abec7b9847`
is loaded and verified on boot `9f1434bf-725b-4ff1-ae51-babd4df25cb4`, Xorg 717,
normal Mesa `2b7e6eec6ce4`. Two unchanged readback probes passed 24,000 total
iterations / 23,976 comparisons, zero mismatches/unstable reads, no faults.
Immediately preceding baseline failed at iteration 130. The original
55-execution failure/incomplete replay passed 55/55 in all six original
configuration groups, no missing/duplicate/NotSupported substitutions and no
kernel fault. Receipts are in `fast-iterations/readback-generation-{after,repeat,replay}-20260915`.
The two-file fix is committed in the isolated readback worktree as
`4863a3c90a3b`; original dirty trees remain preserved. No pushes. No hardware
job remains running; Xorg is healthy and reusable. Resident remains unchanged.
Broader reuse/concurrency regression and a full fixed-build official campaign
remain required; these focused results are not full OpenGL 3.3 qualification.
See `READBACK-GENERATION-AUDIT.md` for native provenance and exact source hashes.

**Earlier 2026-09-15 generation-identity candidate checkpoint:** Native analysis established that
render work tags and completion cache-maintenance consume the same allocated
generation. Linux's completion record instead uses a captured cold value or an
unrelated physical submission ordinal. The narrow correction is in
`neo-encoder-linux-j700-readback-20260915` on base `8b31b4760ee2` (two dirty
files); see `READBACK-GENERATION-AUDIT.md`. All 28 pinned native instruction
witnesses pass. The new regression passes; the full descriptor suite retains
the exact base's three preexisting failures (candidate 18 pass / 3 fail; base
17 pass / 3 fail). All 77 Rust source hashes match the isolated builder copy.
The initial build found a separate bootstrap caller, so the API refactor was
removed and the empty bootstrap's existing tag/record values are preserved.
Rebuild succeeded in `linux-j700-readback-20260915`; log
`readback-publication-kernel-build-2.log`. Candidate module SHA-256
`03abec7b984762e6b32a27951652d4c0445e8300e7447a795e5c45dc5291d762`, build ID
`81b0fb722ffe014f1a72eb53436c290c7ad52f39`. Fresh baseline failed at iteration
130 with one unstable/mismatched first read and correct second read, no fault.
The guarded helper staged the candidate after Xorg actually exited (the first
attempt correctly rejected D-state display teardown). Qualified RAM boot is
in progress in `boot-readback-generation-20260915`; host log
`readback-generation-boot-host.log`. Resident unchanged. Candidate runtime
result pending; the normal module backup is retained and the readback staging
helper supports `--restore` with exact hash/boot/process guards.

**2026-09-15 current state after paired experiment:** Normal Mesa
`2b7e6eec6ce4` and prior kernel `6e8621eb0c41` are restored, boot
`fcc7cfaf-1f57-4d12-84a4-debecd791a6f`, dedicated Xorg 749; final vendor smoke
exited successfully with no GPU fault. The readback bug is unresolved.
With integration Mesa on this prior kernel, the baseline failed at iteration
133, both weak and strong CDM controls passed 12,000 iterations, and the same
baseline failed again at 153. This does not prove that strong cache maintenance
is missing: extra submission/retirement/wait/time are shared effects. Do not
ship either diagnostic patch. See `HOST-READ-CDM-EXPERIMENT.md` and
`VISIBILITY-DEBUG-PROTOCOL.md` for evidence and the next discriminating work.
Both isolated source checkouts are at their committed integration heads, and
the original dirty source trees remain byte-identical to their saved patches.
No running hardware job remains; no pushes or resident changes were made.

**2026-09-15 rollback and diagnostic checkpoint:** The new kernel later hit a
firmware unmapped-read fault before the host-read experiment ran. The previous
kernel is restored and loaded (`6e8621eb0c41`), boot
`fcc7cfaf-1f57-4d12-84a4-debecd791a6f`, Xorg 749. The isolated Mesa integration
baseline (`f8b2817802af`) reproduces the readback defect at iteration 133 on this
clean boot, without a GPU fault. Weak/strong native CDM controls are built;
see `HOST-READ-CDM-EXPERIMENT.md`. Both temporary patches are preserved as
artifacts and removed from the source checkout. The full official campaign
remains paused while the diagnostic fails. The supervisor now captures bounded
J700 Asahi devcoredumps on future faults; four host checks pass.

**2026-09-15 latest hardware checkpoint:** The isolated kernel was built and
booted, but the late-readback bug remains. Read
`KERNEL-VISIBILITY-INTEGRATION.md` and `VISIBILITY-DEBUG-PROTOCOL.md` first.
Current J700 boot is `0117abac-6cb4-4194-9d9b-9d69f80e26e5`, dedicated Xorg 750.
Mesa remains `2b7e6eec6ce4`; loaded Asahi module is `f8d887c8c952`, ELF build ID
`c4f4f52075e024517a367781e33ed26a00349c4e`. The identical diagnostic failed at
261/12,000 iterations (one mismatch, one unstable read, second read correct),
without a GPU fault. Do not infer improvement from a later failure iteration.
The original dirty source trees and resident boot image remain unchanged.
Kernel integration HEAD is `c4478d9d130b`; all eight targeted host scripts pass
after the scheduler mock repair. Mesa integration is committed as
`0fee2805ba2`, with 294 host passes and two expected failures. No pushes.
Older checkpoints below describe superseded intermediate states.

**2026-09-15 latest checkpoint:** Mesa's complete Asahi host suite now passes:
294 OK, two intentional expected failures, zero unexpected failures
(`native-visibility-asahi-tests6.log`). Missing indirect-launch tail markers
caused duplicate barriers; the fix upgrades the existing tail. Conditional
compute expects two strong tails because it has both a predicate dependency
and a final explicit HOST_READ dependency. No additional launch or barrier.

The late-readback bug remains unresolved. The user's current priority is fixing
it and applying the debugging lessons from Cody Ho's GPU-driver article.
Read `VISIBILITY-DEBUG-PROTOCOL.md`. The published Gravity kernel is M4 code;
do not transplant offsets or infer an A18 cache fix from its GLES claim.

Kernel candidate: `neo-encoder-linux-j700-visibility-20260915`, branch
`j700-gl33-visibility-20260915`, HEAD `931890d89de7`. It preserves the original
four-file MSAA/stamp snapshot and integrates the eleven GPU commits through
`7296ee3c3169`. The initial broad merge was aborted; only those GPU commits were
applied. Seven of eight targeted host scripts pass. `test_g17_owned_scheduler.py`
fails compiling its mock kernel (missing Mutex/new_mutex/pin_init/Error), so
its harness needs updating before claiming a pass. Log:
`kernel-visibility-host-tests.log`. No kernel build or hardware installation yet.
Original kernel and Mesa worktrees and the Neo's running stack are preserved.

**2026-09-15 integration built and tested:** Read
`NATIVE-VISIBILITY-INTEGRATION.md` first for the current source and hardware
state. The isolated `agx3` merge is resolved/staged but uncommitted and builds.
The candidate passes 202 cull-distance cases (16 NotSupported) and the official
primitive-restart case on the Neo, but the fast readback probe still fails after
75 iterations. The Neo has been restored to normal Mesa `2b7e6eec6ce4`, same boot
and Xorg PID 762, without a GPU fault. Nine Vulkan shim failures remain after
the latest integration fixes and focused fixture retest. The original dirty
`mesa-j700` tree is byte-identical to its saved patch. No pushes or new kernel.

**2026-09-15 new upstream visibility fix found; isolated integration started:**
Live remote checks found Mesa `agx3` now at
`e3573059fdf50172a234e4db881315d4565482ad` and Linux `drm-agx3` at
`7296ee3c3169a766b25160e108463552b9de6ca1`. Both remote-tracking refs were
fetched. Mesa `agx3-gallium` is unchanged at `cc761b39c026`. Relative to our
current Mesa HEAD there are 77 upstream-only and 32 local-only commits;
the Linux GPU branch has 11 commits since the previously fetched tip.

DJ commit **`6aae441547b`** is directly relevant: Vulkan barrier-only command
buffers did not emit GPU work, so shader-write → HOST_READ dependencies could
finish without flushing fragment stores. It now emits the native strong CDM
cache-maintenance record only at that explicit host-read boundary. See
`hk_CmdPipelineBarrier2` and `hk_cdm_cache_flush` (native flags 5, with an existing
weak trailing barrier strengthened in place). Our current Gallium
`agx_memory_barrier` only flushes active incoherent batches, and CPU transfer
mapping waits the writer without issuing this explicit host-visibility record.
Investigate the corresponding OpenGL implicit CPU-read boundary. This is a
strong lead, **not a verified Gallium fix**. Do not add per-draw/global barriers.
Also review `b41cee4ed03` (pixel transactions before fragment side effects)
and `170feeac625` (image writes before texture reads).

The main Mesa worktree and its 13 dirty files are preserved. Its full tracked
diff is saved as `mesa-before-native-visibility.patch`. An isolated worktree
at `/Users/mcruz/Developer/aurora-silicon/mesa-j700-gl33-native-visibility-20260915`
on branch `j700-gl33-native-visibility-20260915` contains snapshot commit
`1bb888071a2` of the tested depth/storage/tess candidate. A no-commit merge of
`origin/agx3` is **in progress**, with conflicts in these seven files:
`asahi_clc.c`, `agx3_pack_binary.c`, `agx3_subgroup_core.c`, `agx_compile.c`,
`test-agx3-pack-binary.cpp`, `test-g17p-precompiled-fill.c`, and
`test-tilebuffer.cpp`. Resolve semantically, preserving the local depth,
storage-image, tessellation scratch and ballot fixes. No merged candidate has
been built or installed. The original builder and Neo still run the normal
`2b7e6e...` Mesa and `6e8621...` stamp-gated kernel; no GPU test is active.
The new Linux commits have only been fetched, not merged. No changes pushed.

**2026-09-15 producer correlation completed:** Read
`READBACK-PRODUCER-AUDIT.md`. A temporary in-memory Mesa journal caught **21
late pixel changes in 6,000 mappings**. All six captured histories show the
correct mapped BO in the render attachment list, a successful submission and
a successful wait on its writer sync object before the bytes changed. This
makes a missing Mesa writer wait less likely; it does not prove fence timing.
Normal Mesa was restored on both builder and Neo, SHA `2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
Restoration smoke passed, with no GPU fault. Xorg is still PID 762 on the same
boot and the stamp-gated module remains loaded.

Native 25G83 comparison confirms that the current Linux MCache range packing
matches the concrete native encoder. The firmware's final flush uses
EventControl `+0x24` (copied from descriptor `+0x0c`) and six bits from
EventControl `+0x08`. Investigate those native ownership/visibility semantics
next; no speculative selector change was made. The in-memory observer is a
diagnostic and must not be used for conformance qualification.

**2026-09-15 faster iteration loop is hardware tested:** Read
`FAST-TEST-LOOP.md`. `fast-iteration.py --readback-probe --profile gl33`
now rejects on the first observed readback error, with a 12,000-iteration
budget required for success. It rejected the current candidate in **3.77 s
end to end**, after 93 iterations (one mismatch, one unstable read, second
read correct). This is a custom diagnostic, not CTS coverage.

`--replay-failures partial-depth-storage-tess-full2-20260915 --profile gl33`
batch-replays the 54 failures plus one interrupted case in their six original
sessions. The hardware run finished in **75.13 s: 27 Pass, 28 Fail**, with all
55 cases accounted for. The failures covered 14 unique packed-pixel cases;
the query passed this time. The earlier 570-case wildcard pass did not clear
the race. Both new tests retained the normal Mesa and stamp kernel, reused
Xorg PID 762, and reported no GPU fault or kernel-log overrun.

Replay receipts preserve source QPA hashes and original settings. Missing,
duplicate or incomplete results, changed seeds and newly unsupported cases
cannot be counted as passes. `--repeat N` repeats the original sessions.
Verbose runner stdout now stays on the Neo by default; QPAs and fault evidence
are collected. `--collect-full-log` restores the previous collection behavior.
Seven evidence-accounting tests passed. No driver or CTS implementation was
changed by this iteration-speed work. Next use the seconds-long probe to
reject producing-batch/cache-visibility candidates before broader validation.
The legacy official single-case smoke passed. A 24-iteration harness smoke
also completed its full budget, verifying the positive receipt path only.
Final live checks show no CTS/probe process, Xorg 762 healthy, and unchanged
Mesa/module digests on boot `5a46d969-07c9-4cb1-b307-8adae3456a74`.
Discord progress webhook remains unavailable (`AGENT_PROGRESS_WEBHOOK_URL`
unset); these progress results are saved locally, not posted to Discord.

**2026-09-15 completion-stamp candidate results:** The failing official run
was intentionally stopped after five complete configurations and part of a
sixth, to evaluate the firmware-derived fence fix. The preserved
`partial-depth-storage-tess-full2-20260915/partial-campaign-summary.json`
records 54,683 Pass, 1,484 NotSupported, 54 Fail, and one unfinished case.
There are 23 unique failed cases (22 compressed packed-pixel cases and one
fragment-invocation query case). This campaign is partial and did not pass.
No GPU fault or kernel-log overrun was reported before the stop.

The Neo has completed a qualified RAM boot into
`5a46d969-07c9-4cb1-b307-8adae3456a74`. The candidate module was staged on
the installed root, then verified by file digest **and loaded build note**
by `prepare-clean-boot.py`. DCP framebuffer verification passed and the
dedicated Xorg is PID 762. Module SHA is
`6e8621eb0c4147778d243870907cffccdee5b9b74711516c451fdc9c88937cbe`.
The running Mesa remains the depth/storage/tess candidate below. Root console
login and Wi-Fi succeeded; the resident, Stage2 and kernel Image were retained.
The prior module is backed up on the Neo at
`/home/mcruz/gl30-cts-20260913/asahi-backup-aa5b21014671517849cfa12c7415cd5b39f565283c7140f3b3ebaa367e96fb4b.ko`.
The checked readback probes completed with **5/2,988** and **277/11,988**
mismatches; every mismatched second read was correct. There was no GL error.
The stamp candidate has **not fixed the race**. A focused official run passed
570/570 compressed packed-pixel cases, and the fragment-invocation query
passed when replayed with the original failing FBO configuration and seed.
These focused passes do not override the failing stress results.

An existing same-mapping observer was rebuilt against the current Mesa:
`candidates/libgallium-render-stamp-observe.so`, SHA
`665613998868b71c9359cf1bba9f4a758f31bdcec7d2248bd2ec12684e152edc`.
The observer found **12/6,000 CPU mappings changing pixel bytes after map/sync
returned**, without another GPU command during each observation. The late
visibility/producer-ordering problem therefore persists despite stamp gating.
The observer adds a diagnostic 200-us interval only when explicitly enabled;
it is not a product fix.

**Current restored state:** Both builder and Neo are back on the normal
depth/storage/tess Mesa SHA `2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
The stamp-gated kernel SHA `6e8621eb0c4147778d243870907cffccdee5b9b74711516c451fdc9c88937cbe`
remains loaded. The restoration smoke test passed; Xorg PID 762 is healthy;
no CTS/probe is running. See `RENDER-COMPLETION-STAMP-AUDIT.md` for exact
results and limitations. Next investigate producing-batch identity and the
CPU visibility/cache-maintenance path. Do not claim full conformance or a
reliable coverage gain from the focused passes while the race still reproduces.

## Historical updates (superseded by the results above)

**2026-09-15 16:54 UTC live-run update:** The installed candidate is
`libgallium-depth-storage-tess-spill.so`, SHA-256
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
The full official `--type=gl33` run `depth-storage-tess-full2-20260915` is
active on boot `783909de-db08-4557-afd9-5cc36d421f1e`. Four complete
9,886-case executions report respectively **9569/307/10, 9569/307/10,
9591/289/6, 9587/289/10 (Pass/NotSupported/Fail)**. The fifth execution
is underway. Of the 36 failures in the four completed executions, 35 are
compressed packed-pixel readback and one is
`pipeline_statistics_query_tests_ARB.functional_fragment_shader_invocations`
(zero where at least one invocation is required). These are repeated
executions across configurations, not counts of unique failed cases.
Do not present a per-configuration percentage as full conformance.

Depth replication, the supported native scalar storage-image write path,
and precompiled tessellation scratch support are now in the installed Mesa
candidate. The cull-distance group passed 202 cases with 16 NotSupported
before the full run. Mesa edits remain uncommitted; preserve the dirty tree.
Read `RENDER-COMPLETION-STAMP-AUDIT.md` for new independent firmware evidence:
EventControl is unlinked **before** cache maintenance and the final fragment
stamp write. Linux currently does not gate its render fence on that stamp.
This is a candidate cause, not a verified hardware fix. Do not interrupt the
full CTS run with competing GPU jobs or a module reload.

The older baseline and next-step sections below are historical; this update
supersedes their current-state claims.

**Completion candidate now built:** `asahi-render-stamp.ko` SHA-256
`6e8621eb0c4147778d243870907cffccdee5b9b74711516c451fdc9c88937cbe`
requires the exact physical fragment queue's post-maintenance stamp before
releasing its fence. Native firmware checks passed 17/17; existing pure
completion tests passed 35/35; the kernel module build succeeded. It is
**not installed or hardware validated**. New uncommitted kernel edits are in
`g17_resources.rs` and `g17_manager.rs`, alongside the previous MSAA edit.
The pinned official runner's source and generated mustpass header confirm
**50 GL33 main executions** for the 24 valid configs: four for the first
config and two for each of the other 23, totaling 494,300 case executions.
This excludes the preliminary no-context/discovery work.

**2026-09-15 restart fix:** Read `RESTART-BALLOT-FIX.md`. Mesa commit
`8326b87458c` fixes the adjacency-restart failure by preserving a live ballot
input register. The official restart case, 160 cull-distance functional cases,
21 transform-feedback cases, and 34 probe rows matching llvmpipe pass on the
Neo. No new instructions, barriers, waits, or draw calls were added. This
commit is local and has not been pushed. The normal driver is installed;
all diagnostic source replacements were restored. One of the 22 recorded
non-passes is now fixed, but there is no new full-sweep percentage.

**Earlier 2026-09-15 completion audit:** Read `FENCE-VISIBILITY-AUDIT.md` too.
The same mapped readback changed after the synchronization/map return in
52/6,000 observations; pinned-CPU continuous polling reproduced actual pixel
changes in 18/6,000 observations without another GPU command. The exact missing
completion condition is still unresolved. The original driver was restored and
its digest verified on the device and builder before the restart investigation.
That audit claimed no CTS gain or conformance completion. The installer preserves backups by
previous content digest to support repeated candidate rollbacks safely.

**Device:** J700 "Neo", Apple A18 Pro (G17P / AGX3B)
**Tree:** `mesa-j700` worktree, branch `j700`, head `8326b87458c`, local commit not pushed
**Latest full-sweep baseline:** 22 non-pass of 9,886 = **99.78%**, measured at `4aad4e2ea25` before the restart fix

Measured by a full resumable sweep on a clean boot, zero `EOVERFLOW`, generation 94 of
~256 — so the number is measured, not projected.

---

## 1. The goal and the constraint

Full GL 3.3 conformance, **done properly**. The standing constraint is explicit:

> avoid any hacks or shortcuts. it must do things properly and efficiently. avoid hacks
> like splitting draw streams into one draw per stream, putting barriers everywhere, etc.
> m1/m2 driver is the baseline for efficiency

A fix that trades correctness for throughput, or that papers over an ordering problem with
a blanket flush or a delay, is worse than no fix. Two candidate "fixes" were rejected on
these grounds this session (see §5).

## 2. Two habits that actually produced results

**Measure, don't read.** Every root cause below was pinned by a probe on hardware with a
software control — llvmpipe running the *same* CTS binary — not by reasoning about source.
Three hypotheses looked airtight in the code and were wrong; each cost one rebuild to
disprove because there was a probe to disprove it with.

**Check for a packer refusal first.** When a G17P draw produces nothing, run the client
under the offline drm-shim with `AGX_MESA_DEBUG=refuse` *before* suspecting the feature
under test. A refusal makes the shader unlinkable and the driver then silently drops the
whole draw — it looks exactly like a rendering bug in whatever the shader happened to use.
This check would have saved hours twice. Use `stdbuf -oL -eL`, or the disassembly printed
above the message belongs to the *previous* target.

## 3. What landed this session

Four commits, all pushed. Together: 76 non-pass → 22.

| SHA | What |
|---|---|
| `0f56362edb9` | **Compare with abs on one source, neg on the other.** The byte-constant compare-select refused this `fcmpsel` shape, so every fragment program emulating exactly one cull distance failed to link and the driver dropped every draw using it. `nir_opt_algebraic` folds `ball(feq(cf,0))` into precisely that form. Apple encodes it as one instruction and the general compare-select already reproduced Apple's bytes, so the specialization just had to *decline* instead of *refuse*. |
| `296ad42bb88` | **Keep a broadcast's source alive when it is read again.** The biggest find. `src_live` was never published on the immediate-lane broadcast descriptor, so it defaulted to "final read" and the hardware *destroyed the source register*. That read ends every workgroup scan, so `poly_work_group_scan_inclusive_add` lost its partial sum, `libagx_prefix_sum_tess` turned `counts={2}` into a total of 0, and isoline tessellation rendered nothing at all. `cull_distance.functional*` went 144/16 → **160/160**. |
| `c320e303ff4` | **Twiddle compressed textures at non-block-aligned sizes.** Coverage only. Every block-compressed twiddling case said "Block alignment assumed" and used 32×32; `packed_pixels.varied_rectangle` uses 10×10 and 15×15. Round trip is correct — this pins it so it isn't re-derived. |
| `4aad4e2ea25` | **Expand an indirect dispatch into the grid the hardware reads.** `glDispatchComputeIndirect` passed the API's three *workgroup* counts as an `INDIRECT_GLOBAL` grid, which G17P has no grounded record for; the refusal asserts and kills the process. The hardware wants a six-word grid — three global *thread* counts then three local sizes — which is `INDIRECT_LOCAL`, already live because the tessellator's setup kernel writes that shape. |

> **Do not** try to ground `agx3_cdm_indirect_launch_36_pack` for indirect dispatch. It
> exists in genxml with unit tests, but its only caller is a d3d12 *non-live validation
> wrapper* — it has never been submitted to hardware. The `INDIRECT_LOCAL` route is the
> grounded one.

## 4. The remaining 22

Grouped by blocker, because blocker — not test family — decides who can pick each up.
Counts fluctuate slightly between sweeps; the visibility bug is nondeterministic.

### 9 — Fence signals before GPU writes land · `packed_pixels.*compressed*`
**Not a Mesa bug.** A compressed `glGetTexImage` goes through a **reused** linear
`r8_unorm` staging resource that a GPU blit decompresses into. The blit is hazard-tracked
and the fence is waited to completion, and the CPU still reads the previous texture's
bytes. Every failure is a *first-read* failure; an immediate second read is always correct
(`mismatches=45 unstable_read=45 second_read_ok=45`).

Timing settles it:

| | mismatches |
|---|---|
| no delay | 8 / 788 |
| 200 µs delay before the CPU read | **0** |
| 2 ms delay | **0** |
| `dc civac` over the whole BO | 60 → 40 (only halves it) |

A delay fixing it completely while a cache invalidate only halves it means the fence
signals before the GPU's write-back has landed. `g17_completion.rs` signals off the
firmware EP0x20 type-1 event stamp.

**Next:** kernel/firmware — flush or wait for write-back before signalling.
Reproducer: `probes/compressed-readback-race.c` (seconds, no CTS needed).
**Do not** fix this in Mesa with a delay or a blanket flush.

### 8 — Internal shaders are unspillable · `cull_distance…item_6…triangles_*`
`libagx_tess_tri` needs 223 halves against a 192-half operand domain and asserts
`(remat || ctx->key->has_scratch) && "internal shaders are unspillable"`. **Re-tested on
the current tree — still fails**, so this is not a stale note to retry. The absent kernel
makes item 6 triangles *abort*, which is why these are Aborted rather than Fail.

**Next:** give precompiled kernels scratch on G17P. Note `agx_g17p_append_compute`
explicitly rejects non-zero `scratch_size` today, and G17P spills have no helper program —
both need solving together. Rematerialization alone was measured and is still over the
ceiling.

### 2 — Packed depth/stencil blit, not root-caused · `packed_depth_stencil.blit.*`
The only diagnostic is "Color comparison failed". An oracle that blits `DEPTH|STENCIL`
between packed depth-stencil FBOs and reads back through a stencil test passes on hardware
in **every shape it can validly check**, matching llvmpipe exactly:

- stencil, 2- and 4-sample source → single-sample dest: 1024/1024
- stencil, single-sample source → MSAA dest + downsample colour blit: 1024/1024
- depth, single-sample source, D24S8 and D32F_S8: 1024/1024

**Next:** replicate what the oracle doesn't — the CTS wraps the blit in
`setDrawReadBuffer(GL_NONE, GL_NONE)`, draws a `DEPTH_SPAN1` quad, and verifies a colour
*gradient* rather than a flat fill.

### 1 — Storage-image PBE descriptor · `cull_distance.coverage`
Rejects with `storage PBE has no proven native descriptor encoding on G17P` and aborts.
**Next:** metal-nt witness for a storage-image write, then ground the descriptor.

### 1 — Flaky, same visibility race · `pipeline_statistics…fragment_shader_invocations`
Passes in isolation and passed two consecutive family runs, then failed in the sweep.
Query results are read back by the CPU, so this is very likely the same fence-visibility
defect rather than an independent bug. **Next:** re-check after the kernel fence fix.

### Resolved since the sweep — Restart with adjacency primitives · `primitive_restart.restart_mode`
Commit `8326b87458c` fixes the remaining `LINES_ADJACENCY` and
`TRIANGLES_ADJACENCY` sub-cases. The register ballot incorrectly killed an input
needed by the next broadcast. The corrected encoding matches native Apple
witnesses and passes the official case. The tessellation half still requires
GL 4.4 and is skipped here. See `RESTART-BALLOT-FIX.md` for exact receipts.

## 5. Do not redo these

All measured and disproven this session. They all look plausible in the source.

**On the isoline / prefix-sum failure**
- Strong post-dispatch barrier on user compute dispatches (`..._append_with_barrier`, flags `0x5`) — measured, no change.
- Rodata publication — args occupy halves 12–23 and rodata 24–31 for that kernel. Adjacent, never overlapping.
- The `num_slots` disagreement in `agx_nir_lower_cull_distance.c` (VS store hardcodes 2, FS reads `DIV_ROUND_UP(n,4)`). A real inconsistency, and *not* the bug.
- The TCS — with no TCS bound and driver-supplied default tess levels, the failure is identical.

**On the compressed-readback failure**
- Format selection — byte-identical to llvmpipe for all ten `GL_COMPRESSED_*` enums.
- The compressed blocks themselves — byte-identical to llvmpipe at 10×10, 15×15, 16×16.
- Non-block-aligned twiddling — round-trips correctly, now pinned by a test.
- Decompression — differs from llvmpipe by ±1 LSB, far inside the test's 0.1 epsilon.
- BO recycling — keeping every texture alive still reproduces it.
- `glFinish` after upload — no effect; the blit is issued *inside* `glGetTexImage`.

**Two traps that cost real time**
- `counts[]` is prefix-summed **in place**, so `counts[0]` read after the sum is always 0. Reading it post-hoc proves nothing. (This produced a wrong "the kernel reads zeros" conclusion.)
- A depth check with a multisample source is **not** a valid test — llvmpipe fails it too, because the source's depth sits at sample positions while the verify draw is at pixel centres.

## 6. The witness method

Both ISA fixes were grounded against Apple's own compiler, not inferred. Recipe:
`src/asahi/isa/README-metal-witness.md`. The pattern that works is to compile **two kernels
that differ in exactly one property** and diff the bytes:

```
uint x = lane+1; out[tid] = simd_broadcast(x, 31);
   47 04 54 00 02 00 3e 2c 04 00      source dies

uint x = lane+1; uint b = simd_broadcast(x, 31); out[tid] = b + x;
   47 04 54 04 03 00 3e 2c 02 00      source stays live
```

Byte 8 is the whole difference. Our encoder now reproduces both forms exactly and a unit
test pins them. A *single* witness would not have settled it — an earlier attempt to read
one binary by hand produced a wrong conclusion and cost a rebuild.

## 7. Operating the rig

### Build and iterate
- Mesa is a git **worktree** at `mesa-j700` on branch `j700`. Run everything from there; never `cd` to the original repo root.
- Builds happen on the `neo-builder` Lima VM. `limactl copy` **preserves mtimes** — `touch` every file you sync or ninja will skip it.
- Offline triage uses the drm-shim oracle (`src/asahi/gl-tests/run-gl-shim.py`) — no device, no render generations.
- Hardware runs: `fast-iteration.py LABEL --candidate <libgallium-*.so> --case '<CASE or GROUP*>' --profile gl33`. Fresh label each time.
- Whole suite: `~/fullsweep.py LABEL` on the device — resumable, steps past any case that aborts the process (several still do).

### Budget and hygiene
- **Render generations:** ~256 GL contexts per boot. Past that, every submit fails `EOVERFLOW` and results become silent zeros that look exactly like a driver regression. Check `generation=` and `EOVERFLOW` in the kernel log before trusting any measurement. A full sweep costs ~45.
- **Never run GL clients during a full CTS run.**
- **Wi-Fi:** the link can go associated-but-dead. An interface down/up plus re-running the bringup script restores it over the console **without a reboot** — the boot and its generation budget survive.

### Reboot sequence
1. Stop the campaign X server on the device.
2. `systemctl reboot` over the console.
3. `boot-qualified.sh <fresh campaign dir>` from the Mac (chainloads Linux).
4. `post-boot-prepare.sh LABEL` with `MESA_SHA` set to the digest of whatever driver the previous boot left installed — the clean-boot check asserts on it.

### Secrets
The console credential is supplied in chat only. It is passed to `post-boot-prepare.sh` on
**stdin** and must never be written to a file, script, artifact, log or commit. Same rule
for any webhook URL, token, network name or address.

### Git
- Never bare `git stash` / `git stash pop` — the stash stack is shared across worktrees and other sessions may be using it. Prefer a temporary WIP commit.
- No attribution lines in commit messages.

## 8. Known-red tests

Compiler suite is clean: **476 pass**, including the two new encoding tests.
The asahi suite is 289 Ok / 2 expected fail / **3 fail**. All three are pre-existing —
verified by building at the previous head.

| Test | Status |
|---|---|
| `asahi-g17p-precompiled-fill` | `push_count` drifted for every precompiled kernel (COPY_IMAGE 42→54), binary size unchanged, hashes changed. Goldens stale. **Not attributed to a commit** — attributing it needs a bisect with a Mesa rebuild per step. Deliberately **not** re-pinned, because writing down numbers that can't be explained defeats the test. |
| `asahi-g17p-mrt-mixed` | Pre-existing, untriaged. |
| `hk_agx3_ioto_program_core` | Pre-existing, untriaged. |

## 9. Where the detail lives

Long-form notes are in the session memory directory, indexed from `MEMORY.md`:

| Note | Carries |
|---|---|
| `gl33-full-failure-map-20260915` | Every sweep measurement, what each remaining family needs, the packed_depth_stencil elimination list |
| `g17p-fence-signals-before-writes-land` | The kernel visibility bug, its reproducer, the full elimination list |
| `g17p-threadgroup-memory-is-dead` | Three G17P shared-memory/broadcast defects, all now solved, with witness bytes |
| `g17p-packer-refusal-drops-the-whole-draw` | Why a refusal looks like a rendering bug, and the `AGX_MESA_DEBUG=refuse` check |
| `j700-g17p-render-generation-budget-per-boot` | The 256-context ceiling and how exhaustion presents |
| `g17p-drm-shim-is-the-driver-level-oracle` | Offline triage without hardware |

Probes kept on disk:
`neo-linux/artifacts/j700-gl30-cts-20260913/probes/compressed-readback-race.c`

---

## Where to start

**10 of the 22 sit behind a single kernel fence-visibility defect, not Mesa.** Fix that
first and the Mesa-side remainder is 12 — of which 8 are the one tessellator spilling
problem. The work is far more concentrated than the case count suggests.
