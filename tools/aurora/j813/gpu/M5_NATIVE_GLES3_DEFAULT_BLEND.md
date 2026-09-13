# M5 native GLES 3.0 default-framebuffer blending

## v24 hardware result: 1,171 passes

The remaining additive-blend failures used HADD with a negated half-register
source. Independent strict Metal kernels now establish native G17G x-y,
y-x, -x-y and saturate(x-y), with both dead and retained inputs.

Reference source: `m5-gles3-half-negate-reference.metal` and pipeline JSON.
Original binary SHA256:
`0312ac280a0cb6619f76755a8ffe34f1652a9514f0b5a4653dc2d44fe5d741c2`.
The 32 original instruction vectors cover fields 47/30 for source negation,
field 14 for saturation, source lifetimes, low/odd half registers and the
slot-eight dependency. Every vector matches production-encoder bytes.

`agx3_alu_hadd_neg` retains operand lifetimes when canonicalizing a single
negation onto source 1. Both-negative addition uses the native dual-negate
form. The AGX3B path retains its existing restrictions. This is the original
half arithmetic; it does not substitute shaders or alter CTS expectations.

426 compiler tests and ALU/FALU suites pass; native Gallium and Honeykrisp
build. The compiler-only sweep of all 677 default-framebuffer blend equation
cases has zero instruction refusals. Its raw image comparisons do not count
as hardware results because the VM shim rejects all GPU submissions.

Candidate driver SHA256:
`e95c1578fe1c62578bc1882f0f59bb7f02b5e0d58e47d8ffccab6d7c5eb8df80`.
The wider compiler-only sweep completes all 1,152 default-framebuffer blend
cases without an instruction refusal. The first sRGB framebuffer case then
refuses ST_TILE and LD_TILE format 10 and aborts on the unimplemented native
tile descriptor. This is the next distinct failure, not a passing result.

The physical M5 passes all 1,152 default-framebuffer blend cases plus 19
color-clear regressions: 1,171 expected, unique, started, completed and Pass.
This includes the two formerly refused additive-blend cases. No dropped work,
compiler refusals, missing/unexpected cases, GPU faults or timeouts occurred.
All 2,780 generations retire both queues exactly once, in order from 1.
The run took 129.818 seconds, with sampled maximum temperatures ranging from
42.928 to 43.762 C. CPU0 remains capped at 1.308 GHz with WFI, and the runner's
70 C abort guard remained active. Native Apple M5 (G17G C0) and the mapped
driver hash were verified; no Zink, software rendering or test modifications.

Retained raw CTS evidence: `m5-native-gles3-clear-default-blend.qpa`,
`m5-native-gles3-clear-default-blend.json` and
`m5-gles3-clear-default-blend-cases.txt` in this directory.
The unmodified CTS commit is `1942832354c96f0de379f491519734256fbd4f9d`;
configuration is surfaceless `rgba8888d0s0ms0`, 64x64, base seed 1.
QPA SHA256:
`8dda7513cc9cf60b69d1fc683d3c56aa5f46244d5d461adaab1437c1369599bd`.
Campaign ledger: **1,171 Pass, 53,628 NotRun** out of 54,799 cases. This is
progress coverage, not full conformance qualification on the final driver
and required configurations. sRGB tile load/store and descriptors remain
the next known missing implementation.

This boot also validates rendering in the smaller native runtime. It used
the preceding archive plus its original missing `grep` binary, restored from
the verified desktop archive before driver installation. The final archive
includes that binary: `m5-cts-runtime-root.tar.gz`, SHA256
`b516ad9186d06120bc09f3e5d9d29c89314364a055298f25344e934bedf33714`.
Target monotonic timestamps measured 57.59 seconds from download start to
SSH ready: 19.38 download, 5.71 verification, 30.28 extraction and 2.22 setup.
Archive size is 146,726,585 bytes, approximately 60 percent smaller than the
desktop source. No precise speedup ratio is claimed because the earlier
full-root restore was not separately phase-timed.
