# M5 native GLES source-alpha blending

Physical M5 passes all 15 source-alpha additive blend cases and all 19
color-clear regressions through native Asahi Gallium. The unchanged Khronos
pixel comparisons pass for all 34 cases on one driver. This is a focused
hardware qualification, not GLES 3.0 conformance.

## Implementation

Mesa j813 adds G17G float-to-half narrowing, half multiply/add/FMA,
saturation, inverse alpha, and half min/max with the original NaN semantics.
The compiler preserves exact widening before normalized fragment stores
and accepts either operand order for inverse expressions. HAL300 U8 index
draws widen through Gallium's existing index utility, preserving every index,
base vertex and restart value before U16 command construction.

Commits: `cf7fa22deac` (mediump blending), `4f7b5f07a41` (U8 indices).
423 compiler regressions and the ALU/FALU suites pass. Native Gallium and
Honeykrisp libraries build. The new encoder tests compare 62 original Apple
instructions, including compact odd-half-register forms and slot-8 waits.

No CTS source, shader source, expected image, comparison threshold, waiver,
advertised version or renderer substitution was changed.

## Physical evidence

- CTS commit: `1942832354c96f0de379f491519734256fbd4f9d`.
- Renderer: `Apple M5 (G17G C0)`, provenance verified by the runner.
- Driver SHA256: `cec0c663594d0cc38a5c5314824245e13d413fc01e36e36e3b7cc8156e6f65c1`.
- QPA SHA256: `9f3041c776784217acc2f931254198305271b2eae0ae4f5d9a6e8119a81307e7`.
- Run: 34 expected, 34 started, 34 completed, 34 Pass; 9.408 seconds.
- GPU: all 293 unique application generations completed with both queues retired.
- No compiler refusal, GPU failure, timeout or thermal abort.
- Sampled maximum temperature: 43.037-43.303 C; real-temperature stop guard 70 C.
- Headless thermal kernel, CPU0 only with WFI and requested/max 1.308 GHz;
  43 SMC temperature channels and cooling zones enabled.

Retained files: `m5-native-gles3-clear-src-alpha.qpa`,
`m5-native-gles3-clear-src-alpha.json`, and
`m5-gles3-clear-src-alpha-regression-cases.txt`. Run the list with
`run-m5-native-cts.py`, `--binary deqp-gles3 --platform surfaceless`,
`--config rgba8888d0s0ms0 --base-seed 1 --seconds 90` on a fresh qualified
thermal boot. The current experimental kernel retains its first application
VM; do not bypass that guard to run a new GPU client after the process exits.

## Independent instruction references

The adjacent `m5-gles3-*-reference.metal` files are independent native compiler
witnesses, never substituted into CTS. `build-m5-metal-reference.sh` compiles
with the installed Apple Metal toolchain, macOS 26.0, Metal 3.2,
`-fno-fast-math`, and `metal-nt -arch applegpu_g17g`. It refuses to overwrite
retained text output. Original GPU binary SHA256 values:

| Reference | SHA256 |
| --- | --- |
| float-narrow | `7925a2590619a1427149bcf1912807c3ebb390edf02c3f871b89b73632cadc5e` |
| half-saturate | `2cdd49c5673e0aa2f8208a62cd859684763d2f31dc347bf0505d4dd105d79cc2` |
| half-inverse | `28ebd6fafaa219c2b602afa03325f2f7b7d8bee197d0c5ec47cbe19cd0d78c64` |
| half-arithmetic | `b38f10486906364ff0a58531e6aa50be36739e65558d64c472f496a82a03826e` |
| half-minmax | `30bba4870c8f1a19d906bd82af0858e0c3e6d59e993839eaaf6aedbd68f4206d` |
| half-select | `ca75171c7a35f5cbc9a24b66d708ac705025839c521b98fbf0f5792634b032e8` |

The half-select reference independently distinguishes ordered comparisons
from min/max NaN behavior. `decode-m5-native-fields.py` is a table inspection
aid; its candidates still require exact production-encoder round trips.

## Remaining work

The campaign ledger is 34 Pass / 54,765 NotRun across 54,799 GLES3 cases.
The next target is the complete 225-case additive blend group. Offline
preflight finds 15 shader refusals involving positive half immediate add,
half immediate multiply, and an all-immediate full-width add. Offline results
are not counted as physical CTS execution. Full qualification still requires
all cases on the final driver and required configurations.
