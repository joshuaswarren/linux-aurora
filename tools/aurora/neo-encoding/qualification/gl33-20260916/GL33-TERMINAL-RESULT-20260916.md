# OpenGL 3.3 CTS: failed

Campaign `cache-identity-full-gl33-20260915` completed all 50 main configuration
runs. The official summary reports `Conformant="False"`; the runner exited 1.
Main results: **478,322 Pass, 14,666 NotSupported, 1,312 Fail**. There are 16
additional setup/no-context passes. Complete execution does not establish a
successful conformance result.

All 1,312 failures occur in the final configuration:
`config-gl33-main-cfg-64-run-17-width-113-height-47-seed-2-egl.qpa`.
Every one of these case names passed in each of the other 49 configurations,
using the same driver. This supports a long-run stability failure; it does not
establish a regression against a previous driver version. The configurations
vary in dimensions, seeds, and framebuffer properties, so these are not 49
identical-setting control runs.

The unsupported counts follow the existing applicability rules: 289 in every
configuration, plus 18 depth-dependent skips in each of 12 depthless
configurations (`50 * 289 + 12 * 18 = 14,666`). The applicability audit covers
all 50 files and explains the unsupported results. Its overall audit result
still fails because required cases returned `Fail` in the final configuration.
Do not describe the overall applicability audit as passing.

## Firmware failure and reporting correction

The preserved kernel stream first records a session rebuild at line 2,692,242,
kernel sequence 964203. The replacement GFX RTKit boot times out, then one
submission returns ETIME and 6,203 subsequent submissions return ENOTSUPP.
The first failed CTS case is
`KHR-GL33.texture_swizzle.smoke_access_idx_5_channel_idx_2`.

The original supervisor receipt says `kernel_fault: false`. That field is
wrong: its detector missed these message formats. The receipt, official XML,
and QPAs remain unmodified. The separate terminal audit records the corrected
fault finding and its disagreement with the original receipt.

The live observer now executes the shared fault detector, and the final
analyzer independently rescans saved logs instead of trusting that receipt.
Missing logs cannot establish valid kernel evidence. Host validation passes:
five fault-detector tests, two remote-observer tests, and 13 accounting tests,
including a falsely clean receipt, missing evidence, and log overrun.
The attempted SSH progress refresh timed out; the corrected observer's
transport was not verified live. The complete preserved campaign was rescanned
successfully with the same detector.

## Remaining work

The last successful read-only device check found J700/T8140 on boot
`9f1434bf-725b-4ff1-ae51-babd4df25cb4`, Xorg PID 717, and no CTS process.
No driver replacement, reboot, or new GPU workload was performed in this
continuation. The GPU session needs recovery before replaying failures.

The retained-render 24-bit ordinal limit is a possible rebuild trigger, but
the saved log does not establish its exact value at the failing submission.
Do not change the counter width or wrap its firmware stamp without proving
the native protocol. First establish the rebuild reason, then qualify the
fix with the exact failed settings and a new full official run.

Machine-readable comparison and corrected fault evidence:
[GL33-TERMINAL-AUDIT-20260916.json](GL33-TERMINAL-AUDIT-20260916.json).
Reproduce with `audit-terminal-cts.py RUN --output NEW-AUDIT.json`.
