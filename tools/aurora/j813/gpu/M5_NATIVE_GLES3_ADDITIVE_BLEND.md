# M5 native GLES additive blend qualification

Physical M5 completes 244 unchanged GLES 3.0 cases: **242 Pass, 2 Fail**.
The passing cases comprise all 19 color-clear regressions and 223 of the
225 default-framebuffer additive blend combinations. This is partial GLES
coverage; full conformance is not established.

## Hardware evidence

- Native renderer: Apple M5 (G17G C0), Asahi Gallium; mapped library verified.
- Mesa j813: `19e6651054b`.
- Driver SHA256: `5dbd520138b9ac6e9f795d6e5fb1f1b5c565c6fecb3e3016a53f087a3c64d82f`.
- Unmodified CTS: `1942832354c96f0de379f491519734256fbd4f9d`.
- Configuration: surfaceless, `rgba8888d0s0ms0`, 64x64, base seed 1.
- Duration: 22.095 seconds; 244 expected, started and completed cases.
- All 503 submitted generations retire both GPU queues exactly once.
- 43 SMC channels monitored; maximum readings range 42.865-43.349 C.
- No GPU timeout, firmware fault, missing result or thermal abort.

The original report is `m5-native-gles3-clear-additive.json`; its `passed`
field is false because the batch contains two failures. The unchanged QPA
is `m5-native-gles3-clear-additive.qpa`, SHA256
`d2470701e5f91abd6824cc6a0f417168cac0fa97bd3c008cf0ceb6fc1a607276`.
The exact case list is `m5-gles3-clear-additive-cases.txt`.

No Zink, software renderer, shader substitution, CTS modifications, relaxed
image thresholds or waivers were used.

## Compiler advancement and remaining failures

The compiler now emits G17G scalar-half immediate addition and multiplication
using independently compiled Apple Metal references. It materializes one
operand when FADD has two constants, retaining original arithmetic, signed
zero and modifiers. Twenty-four native instruction vectors and 425 compiler
tests pass; Gallium and Honeykrisp build.

`m5-gles3-half-immediate-reference.metal` and its pipeline JSON reproduce the
instruction reference using `build-m5-metal-reference.sh`. The original
reference binary SHA256 is
`e05381f7ea76179b2e90e924392f121ab2d5d3b34bc3486c33bdf6454c9c3f13`.

Two cases still require half-register addition with a negated source:

- `add_one_minus_src_color_src_alpha`
- `add_one_minus_src_color_src_color`

Their shaders are refused and their raw CTS results are Fail. The progress
ledger classifies these attempts as Invalid because the driver dropped
required work. Both refusals match the compiler-only preflight.

## Evidence validation

A black-image test can produce a raw Pass after a dropped draw. The runner
now records driver diagnostics by case and refuses to qualify such a run.
`m5_native_cts_validation.py` preserves the original CTS results; it does not
alter test behavior. Its regression checks cover the misleading black Pass,
unscoped errors and successful work. The ledger also rechecks older retained
logs rather than trusting their Pass labels alone.

Campaign inventory: **242 Pass, 2 Invalid, 54,555 NotRun** of 54,799 unique
GLES 3.0 cases. Final qualification still requires complete coverage with
the final driver and required configurations.
