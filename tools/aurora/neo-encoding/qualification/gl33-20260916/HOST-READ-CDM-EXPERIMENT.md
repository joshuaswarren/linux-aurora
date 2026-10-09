# Native CPU-read cache-command experiment

This is a temporary diagnostic, not a proposed production synchronization
policy. It tests the explicit native CDM cache-maintenance mechanism used by
the upstream Vulkan HOST_READ path against an ordering-only control. The
committed Mesa integration stays at `0fee2805ba2`; the temporary source patch
was removed after both binaries were built.

At a Gallium CPU READ mapping, after the existing producer synchronization,
the diagnostic submits one isolated native CDM record and waits for its fence.
Both variants retain the mapped BO identically, use the same queue path, and
reject a nonempty compute batch instead of mixing unrelated dispatches into
the experiment. There are no shader launches, sleeps, or read retries added by
the patch. This diagnostic adds work to each CPU read and must not be promoted
as an efficient implementation merely because it happens to pass.

| Variant | Native flags | SHA-256 |
| --- | --- | --- |
| Integration baseline | No added CDM record | `f8b2817802afdd2fd8bb5019e8d669548c77a705ac46644c158d15f5fe4e86f5` |
| Weak ordering control | 0 | `a00fdac63575d7e7b604980093b0f569727e9b870ffad4f8bc09061f9a727124` |
| Strong cache-maintenance candidate | 5 | `525f65787b5fdefedb3b247d56010c2d95ac0315ba6bbe8497ef786f953913da` |

Patches: `host-read-cdm-experiment.patch` (weak) and
`host-read-cdm-strong-experiment.patch` (strong). They differ only in the
`AGX_HOST_READ_EXPERIMENT_FLAGS` constant. Builds succeeded; logs are
`host-read-weak-build.log` and `host-read-strong-build.log`.

## Predictions and limits

- Baseline and weak fail, strong passes: evidence for the maintenance semantics,
  but still require repeated full-budget runs and a native same-chip comparison.
- Weak and strong both pass: consistent with added submission/retirement time
  masking the race. Does not demonstrate that flags 5 fixes publication.
- All fail: this maintenance mechanism is insufficient for the failing path.
- Any firmware timeout: stop testing that boot and preserve evidence. A crashed
  GPU cannot supply valid comparisons.

## Current result

The newer kernel plus integration baseline timed out before either diagnostic
was installed. See `KERNEL-VISIBILITY-INTEGRATION.md`. The previous kernel was
staged for rollback; the paired test is pending a clean baseline boot.

Rollback completed: boot `fcc7cfaf-1f57-4d12-84a4-debecd791a6f`, loaded module
`6e8621eb0c4147778d243870907cffccdee5b9b74711516c451fdc9c88937cbe`, Xorg 749.
The integration baseline failed after 133/12,000 iterations (121 comparisons,
one mismatch, one unstable read, second read correct), without a GPU fault.
Evidence: `fast-iterations/host-read-clean-baseline-20260915/`.

The weak ordering control completed all 12,000 iterations: 11,988 comparisons,
zero mismatches, zero unstable reads, no GPU fault. Probe time 33.352 seconds.
Evidence: `fast-iterations/host-read-weak-20260915/`. This shows the added
submission/ordering/retirement path can remove the symptom without flags 5;
it does not distinguish timing masking from those other effects.

The strong cache-maintenance variant also completed all 12,000 iterations:
11,988 comparisons, zero mismatches, zero unstable reads, no GPU fault. Probe
time 33.207 seconds. Evidence: `fast-iterations/host-read-strong-20260915/`.
Therefore this experiment does **not** establish that the strong native flush
is the missing operation. Both records share an extra submission, its firmware
retirement, and a fence wait; those effects must be distinguished next. Neither
temporary variant is suitable for promotion. These are diagnostics, not CTS
passes. A return-to-baseline run is used to check that the failing workload
still reproduces after the two controls.

Return to the identical integration baseline: failure after 153 iterations,
141 comparisons, one mismatch, one unstable read, second read correct; no GPU
fault. Evidence: `fast-iterations/host-read-return-baseline-20260915/`.
The baseline/control/control/baseline sequence therefore reproduced the
failure on both sides of the passing controls. Next investigate the shared
submission/retirement boundary and native same-chip producer completion;
do not convert either diagnostic into an unconditional CPU-read workaround.

After the experiment, normal Mesa `2b7e6eec6ce4` was restored with the guarded
installer and the vendor smoke exited successfully without a GPU fault.
Evidence: `fast-iterations/host-read-restore-normal-20260915/`. The prior kernel
remains loaded, Xorg 749 remains healthy, and no experimental diagnostic patch
is left in the source checkout or installed library. The full CTS campaign
was not restarted.

The approach follows [Cody Ho's debugging account](https://codyho.dev/blog/gpu-driver/):
let the failing driver workload guide the next experiment, minimize hidden
state, and compare against a control. Model-generated explanations are
hypotheses; hardware outcomes determine what survives.
