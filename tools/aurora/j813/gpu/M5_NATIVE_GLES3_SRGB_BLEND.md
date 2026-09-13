# M5 native GLES 3.0 sRGB blending

## v25: 2,275 hardware passes, 48 sRGB preservation failures

v24 passes all 1,171 color-clear/default-framebuffer blend cases. The next
unmodified CTS group, `dEQP-GLES3.functional.fragment_ops.blend.fbo_srgb`,
originally failed to compile SRGBA8 tile load/store. Its later "M5 tile shader"
descriptor error was a consequence of the refused shader.

The candidate enables explicit sRGB transfer around native tile access:
decode RGB before blending, encode RGB before normalized packing, leave
alpha linear. Clear and reload use the same tile representation. End-of-tile
uses a linear PBE descriptor to transfer the already encoded bytes once.
Packed R11G11B10 lowering is now a separate capability, preserving the G17P
path without enabling that unqualified format on G17G.

G17G LOG2 and full-register comparison operands now explicitly widen half
inputs. Source abs/neg are preserved; comparison and normalized-pack source
modifiers use exact IEEE sign-bit operations where the native instruction
has no matching operand field. The arithmetic, comparison condition and
clamp/quantization remain intact.

Independent Apple Metal pipelines are retained in
`m5-gles3-srgb-reference.metal` and `m5-gles3-srgb-reference.mtlp-json`.
Original applegpu_g17g binary SHA256:
`a4ee815f1aa82ed83b576b1079a68543c04eb2f97fb3c4402fca6beeca75dc9c`.
Its fragments use normalized pack/unpack around byte-width tile accesses;
the native sRGB conversion modes are distinct for RGB and the pair containing
alpha. This reference establishes native behavior, not a hardware CTS pass.

All 2,304 default-framebuffer and sRGB blend cases compile with no instruction
refusal, dropped-work diagnostic or native-descriptor refusal in the final
compiler-only sweep. That shim rejects all GPU submissions, so none of its
image comparisons count as hardware passes. Log:
`logs/m5-all-blend-v25-legalized-compile-only-20260913.log`.

426 compiler tests and ALU/FALU suites pass. Focused epilog regressions cover
G17P behavior plus G17G full/half inputs and negative subtractive blending.
Gallium and Honeykrisp build. Candidate driver SHA256:
`78d7bfbf112baba498dd0f95c260442618056b3413a7588357de7dbaba1fbd6c`.

The physical M5 completes exactly 2,323 unique cases in 313.970 seconds:
**2,275 Pass / 48 Fail**. All 1,171 previous passes remain passing; 1,104 new
sRGB cases pass. Native Apple M5 (G17G C0) renderer and mapped driver hash
are verified, with no compiler refusals, dropped-work diagnostics, missing
cases, unexpected cases, timeout or thermal abort. Sampled maximum
temperatures range from 43.303 to 44.006 C.

The kernel ring wrapped during the larger run. Its retained completion
suffix covers generations 835 through 5207, each exactly once; it cannot
independently prove the earlier 834 retirements. Capture the kernel stream
continuously on the next run and retain the final snapshot too.

Retained evidence in this directory:
`m5-native-gles3-clear-all-blend.qpa.gz` (lossless original QPA),
`m5-native-gles3-clear-all-blend.json` and
`m5-gles3-clear-all-blend-cases.txt`.
CTS commit `1942832354c96f0de379f491519734256fbd4f9d` is unchanged;
configuration is surfaceless `rgba8888d0s0ms0`, 64x64, base seed 1.
QPA SHA256:
`38fd0c4e83f280d303da39a23bed20b7eadb8d74dd380e6d2ec05712f4ce357d`.
Ledger: **2,275 Pass / 48 Fail / 52,476 NotRun** out of 54,799. Final
driver/configuration qualification is still required for the full goal.

The failures are genuine pixel mismatches, not invalid dropped-work cases:
3 equation cases, 30 destination RGB/alpha combinations and 15 source
RGB/alpha combinations. They concentrate on preserving destination RGB.
For `add_zero_one`, retained GPU output is cyan while the reference retains
the prior gradient, with maximum RGB error 152 against unchanged threshold
7. Alpha error is at most 1 in that case. This points at preservation across
no-op or partial-color writes, not a near-threshold transfer-function error.
The exact cause and fix remain unproven; no tolerances or CTS shaders changed.

The next 872 non-blend fragment-operation cases have also been preflighted.
The diagnostic reaches 835 cases, with 23 cases showing instruction refusals,
then stops on integer-clear tile-vector validation in `scissor.clear_int_buffer`.
The first refusal is `interaction.basic_shader.10`, a half FCMPSel shape.
These are separate future driver gaps and are not counted as hardware results.

The v25 fresh boot uses the corrected reduced runtime archive, with grep
included and its pinned SHA checked. Target monotonic restore timing is
55.90 seconds download-start to SSH-ready: 19.29 download, 5.75 verification,
30.31 extraction and 0.55 SSH setup. Thermal monitoring, CPU0/WFI/1.308 GHz
cap and the runner's 70 C abort remain mandatory for the physical test.
