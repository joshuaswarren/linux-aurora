# M5 native GLES 3.0 after the AGX3 update

The updated native M5 driver completes the same 8,878-case GLES 3.0 CTS batch
with **8,734 Pass and 144 Fail**. Every individual result matches the prior
FMA candidate: **zero regressions and zero additional passes**. This is a
development coverage comparison, not full GLES conformance qualification.

## Integrated source

Mesa `j813` merge `35b32f975546e61e4521af0ac11a99c53321b155` integrates 20 commits
from dj's `agx3` branch through `5f07e8677e39b7617457e8869dae662c98074f61`.
`agx3-gallium` through `cc761b39c0266564a23b0882b4c3bf7de3df69e4` was already
included. The update brings G17P dependency, conversion, trigonometry,
half-precision and texture work. Integration preserves the independently
qualified G17G arithmetic, array sampling and eight-slot dependency forms.

Linux remains `de8feb0a0f671dcd9cf8e0cf9f2ee26881357ad5` for this comparison.
The native thermal kernel image SHA256 is
`90769960880b5f563aa00638914efed27918654d1008606e9da83808532c588e`.
The CPU0 cap is 1,308,000 kHz, with WFI idle, WFxT disabled and the existing
SMC thermal monitoring and 70-degree stop threshold. The DCP desktop is not
part of this thermal CTS image.

## Native test conditions and result

CTS commit: `1942832354c96f0de379f491519734256fbd4f9d`.
Binary: `deqp-gles3`, native Asahi Gallium, renderer `Apple M5 (G17G C0)`.
Seed 1, 64x64 pbuffer, `rgba8888d0s0ms0`, same exact ordered case list.
No CTS source, tolerance, software fallback, Zink or draw-splitting changes
were introduced. Case-list SHA256:
`6df90b90b3ce2543e868ec7ce0ef72d424a742b2d65c5cc0c84d1311a870f57f`.

| Measurement | Previous FMA candidate | Updated AGX3 candidate |
| --- | ---: | ---: |
| Completed GLES 3.0 cases | 8,878 | 8,878 |
| Pass | 8,734 | 8,734 |
| Fail | 144 | 144 |
| Retired submission generations | 11,837 | 11,837 |
| Draws | 24,319 | 24,319 |
| Submissions with multiple draws | 9,532 | 9,532 |
| Runtime, seconds | 646.157 | 640.139 |
| Maximum sampled temperature, Celsius | 44.974 | 45.303 |

All 8,878 per-case submission draw-count sequences match the prior run.
Both queues retired in all 11,837 consecutive generations. The kernel capture
contains 23,700 consecutive records, sequence 1024 through 24723, with no
gaps, overruns or driver errors. There were no missing/unexpected cases,
invalid results, timeouts or thermal aborts. Loaded-library provenance matches
the candidate hash below. Timing differences between these two runs alone
do not establish a performance improvement.

The full enumerated catalog remains **8,734 Pass, 144 Fail, 45,921 NotRun**
out of **54,799 cases** (48,300 dEQP-GLES3 plus 6,499 KHR-GLES3 cases).
All 144 remaining image mismatches are unchanged: 36 cases each for scalar-first
integer/vector addition, AND, OR and XOR. Operand reuse/lifetime remains an
investigation target, not an established cause or a fixed issue.

## Retained evidence

- `m5-native-gles3-agx3-coverage.json`: complete native runner report.
- `m5-native-gles3-agx3-coverage.qpa.gz`: losslessly compressed original CTS
  results, including every failure and its images.
- `m5-native-gles3-agx3-coverage-kernel.log.gz`: complete continuous kernel capture.
- `m5-native-gles3-agx3-coverage-stdout.log.gz`: original submission/result log.
- `m5-native-gles3-agx3-comparison.json`: independent per-case QPA comparison
  against the prior run, including all remaining failure names.

Candidate library SHA256:
`a0cba337a5053f9de30f699c74622e71d77edacd469372b3cef3c26c746b9890`.
Uncompressed original QPA SHA256:
`fd5735ff9c1ec6e207f21d30210ed61409181a6a7614f604bbf6bde2cf481d27`.
Uncompressed kernel-stream SHA256:
`edc69d09c304d232cb8f20e999248ca6aac87244ebc33362a1e854f7c108bfaa`.

## Supporting host checks

The updated native Gallium build completes. All 8,878 compiler-only preflight
cases complete without opcode refusals; these preflights receive no hardware
pass credit. The Asahi host suite reports 263 passes, two expected failures
and two G17P failures:

- The precompiled-fill robust-index size/hash checks also failed in the retained
  upstream baseline from the previous integration.
- The Hyprland border shader fails on an unsupported mixed-width FADD. A control
  with the previous v30 Gallium binary reproduces the same refusal.

Neither host failure is omitted or counted as a native M5 CTS pass. Native
G17P execution was not part of this M5 regression campaign.
