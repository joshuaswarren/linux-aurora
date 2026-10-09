# J700 late-readback investigation: evidence and next experiments

Priority: make the existing reproducer and the original official CTS failures
pass by correcting synchronization or visibility at its actual boundary.
Preserve batching and the M1/M2 efficiency baseline. A delayed successful read,
observer-induced pass, or different test configuration does not establish a fix.

## Evidence held fixed

- The producer journal captures six instances with the correct BO, render
  attachment, successful submit and successful writer wait before its CPU map.
- Across 6,000 mappings, 21 changed bytes after synchronization. The 200-us
  observer masks the application-level race, so its application results are
  not pass evidence. See `READBACK-PRODUCER-AUDIT.md`.
- Requiring the fragment completion stamp did not fix the defect.
- The newer Mesa integration still failed at iteration 75. Its host tests now
  pass, but that is not GPU correctness evidence.
- The newer kernel also failed the identical probe after 261 iterations, with
  one unstable read and the second read correct. No GPU fault was logged.
  See `KERNEL-VISIBILITY-INTEGRATION.md` for identities and before/after evidence.
- The newer kernel later hit an unmapped-read firmware fault and was rolled
  back. The Neo is on boot `fcc7cfaf-1f57-4d12-84a4-debecd791a6f`, Xorg 749,
  baseline Mesa SHA `2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
  Revalidate this before the next hardware operation.
- The paired native CPU-read diagnostic did not isolate strong cache
  maintenance as the fix. Baseline failed at iteration 133; both weak flags-0
  and strong flags-5 controls passed all 12,000 iterations; the identical
  baseline then failed at iteration 153. No GPU faults occurred in this
  four-run sequence. See `HOST-READ-CDM-EXPERIMENT.md`.
  Extra submission, firmware retirement, fence wait, and elapsed time are
  common to both controls. Do not promote either diagnostic as a fix.

## Lessons applied from the article

[Cody Ho's account](https://codyho.dev/blog/gpu-driver/) reports faster progress
when Mesa failures drove RE, and when a large, contaminated compute capture was
replaced by a tiny isolated workload. Round-trip assembly alone did not establish
instruction semantics. His model preferences are an experience report, not a
controlled model comparison.

Our application: choose the next experiment for its ability to distinguish
causes of the specific readback failure. Do not expand the task into exhaustive
hardware documentation. Keep capture data and reasoning separate: summarize
only the relevant state, while preserving the raw evidence for reanalysis.

## Competing explanations

| Explanation | Evidence so far | Discriminating next check |
|---|---|---|
| Wrong producer or wait generation | Correct producer chains observed; lower-level generation error still possible | Correlate exact submission generation, both stamp destinations, and fence signal for the failing BO |
| Completion signalled before required work | Fragment stamp check alone fails to cure it | Compare the native retirement sequence against Linux at the same generation; distinguish work completion from storage reuse |
| Incomplete cache publication | Pixels become visible after the successful wait; explicit Vulkan HOST_READ fix is relevant | Compare cache range, context selector, HWDSID, and actual maintenance commands for the mapped BO against a minimal same-chip native operation |
| Premature resource reuse | Texture churn reproduces the failure | Track allocation and retained descriptor ownership through the existing failing case; change only the ownership condition under test |

An experiment must state its predicted result under each competing explanation.
Do not replace an unknown hardware field with a plausible constant. Do not
accept a pass caused only by extra elapsed time or unrelated GPU work.

## Independent source comparison

Pinned Gravity Linux source: `9807df349e5c`, branch `gravity-m4`.
Downloaded comparison files are under `reference-gravity-20260915/`.
Its `g16_runtime.rs` separately checks firmware stamps, driver stamps, notifier
membership, and queue/channel retirement before releasing storage. Its later
driver-stamp checks explicitly protect JobMeta/cache-line lifetime. This is a
useful checklist; its M4 offsets and sequencing are not automatically A18 facts.

Pinned RE documentation: niklassheth/agx-re
`e25433b1d348a809ab86484ce149ff7d459e8cdf`. EXP-0093's fence/barrier results are
explicitly M4-only; the kernel-interface document is a synthesis, not proof of
Linux cache-operation semantics. No published A18 fix for our exact bug has
been identified.

## Validation order

1. Completed: isolated kernel candidate host checks, build, verified load and
   the identical readback probe with unchanged Mesa. The defect still occurs.
2. Use the hypothesis table to narrow the next experiment or capture. The
   paired CDM experiment leaves the shared submission/retirement/timing effects
   unresolved; correlate native same-chip producer completion with publication
   before choosing a production change. The new-kernel firmware fault also
   needs allocation ownership at FAR `0xfffffc20018c0000` established before
   changing lifetime rules. Do not run the long campaign while the diagnostic fails.
3. For a successful candidate, run repeated full-budget readback diagnostics,
   the exact original CTS failure replay, and affected feature groups.
4. Finally run the complete official GL3.3 configuration/seed matrix on one
   fixed driver stack. Keep diagnostic results separate from CTS accounting.

Only the J700 may be used. Preserve the resident boot image and the other Mac's
state. A new native capture should be the smallest workload that isolates the
needed behavior, using the already established RAM-only tracing procedure.
