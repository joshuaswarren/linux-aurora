# M5 HAL300 raster-state reuse — 2026-09-13

Native M5 Gallium now republishes viewport and depth-bias/scissor records
when their inputs change, using the existing Gallium dirty bits. Changing
fragment uniforms no longer reallocates these four payloads or emits their
two PPP pointers. This partially addresses A4 in the fidelity audit.

The raster payload encoding is unchanged. Its dependencies are the rasterizer,
viewport, scissor and framebuffer bounds. The currently admitted M5 path has
one viewport, so shader changes cannot alter viewport count. Switching to a
new or previous batch dirties all state; uploaded payloads remain owned by
the batch pool. Native programs and the nine graphics records still use the
coarser publication path and need further work.

No draw-stream split, flush, barrier, wait or ownership exception is added.
The 164-byte command reservation remains a valid upper bound. M1/M2 use the
existing path without changes.

## Regression evidence

The `asahi-g17g-raster-state` test exercises actual Gallium draws under the
G17G compiler-only DRM shim and decodes the emitted native packets. It checks
viewport transforms, scissor bounds, depth convention, zero bias and that the
referenced allocations belong to the batch. It verifies these transitions:

| Transition | PPP records | Stream bytes |
| --- | ---: | ---: |
| First draw | 11 | 152 |
| Fragment uniform only | 9 | 116 |
| Viewport change | 11 | 132 |
| Scissor change while disabled | 11 | 132 |
| Enable scissoring | 11 | 132 |
| Fragment uniform with scissoring | 9 | 116 |
| Change depth clip convention | 11 | 132 |
| New framebuffer/batch | 11 | 152 |
| Return to previous batch | 11 | 132 |

The first seven draws stay in one batch; returning to it adds its eighth draw.
The test fails on the previous driver because a fragment-uniform change emits
11 records instead of 9. The corrected driver passes all nine transitions.
The reduction is 16 command bytes for each tested fragment-only state update,
plus the avoided payload uploads. It is not a measured GPU performance gain.

The unmodified 2,304-case blend group also completes compiler-only processing
with the exact expected unique case set and no assertion, compiler refusal,
dropped-program or M5 state rejection. All 2,304 submissions are deliberately
rejected by the shim, correctly scoped to their cases by the evidence
validator. All cases are invalid hardware evidence, including 89 raw image
comparison Pass results. None enter the hardware ledger.

Retained workbench logs:

- `logs/m5-raster-state-before-20260913.log`
- `logs/m5-raster-state-lifetime-build-20260913.log`
- `logs/m5-raster-state-lifetime-detail-20260913.log`
- `logs/m5-raster-state-all-blend-compiler-only-20260913.log`

Candidate: `artifacts/gpu/native/libgallium-m5-v27-raster-state.so`, SHA256
`449b37c7381e7e8cf1ff954392125e03ad250cb91e027b872f30fdfc7dbf298b`.

## Physical regression

The candidate includes the preceding command-reservation correction and has
now completed the same unmodified 2,323-case selection on the physical M5:
**2,275 Pass / 48 Fail**, in 307.925 seconds. The exact unique case set and every
status match v25; all previous passes are preserved. The retained failure
images also match v25. Native Apple M5 (G17G C0) renderer and loaded library
provenance are verified. There are no driver errors, missing/unexpected cases,
timeouts or thermal aborts. Sampled hottest-channel readings range from
43.349 to 44.193 C.

Continuous kernel capture retains **10,438 consecutive printk records** with
zero overruns, sequence discontinuities or I/O errors. GPU generations
**1 through 5,207** each report both queues retired exactly once. The 5,207
submissions contain 15,625 draws; 5,185 are multi-draw, with a maximum of 394.
These counts match v25. This establishes preserved batching and complete
retirement evidence for this regression. It does not establish asynchronous
submission or an M1/M2 performance comparison. The submit diagnostic's `bytes`
field is the submission payload size, not the VDM stream size; the 16-byte
state-update reduction above comes from the packet-inspection test.

Published evidence beside this document:

- `m5-native-gles3-state-reuse.qpa.gz`: lossless original CTS QPA.
- `m5-native-gles3-state-reuse-kernel.log.gz`: complete original printk records.
- `m5-native-gles3-state-reuse.json`: runner report and exact case list.

QPA SHA256:
`9086ec5e7bb9cbe0bd30a8ead2baefa397d8fcc4c6a5c4488423d549e6149a59`.
CTS revision and configuration are unchanged from v25: commit
`1942832354c96f0de379f491519734256fbd4f9d`, surfaceless
`rgba8888d0s0ms0`, 64x64, base seed 1. Source for the hardware-tested change is
Mesa `44866172af6d7f7a9136cda2a1adce4badfa8b84`.

A subsequent test-only extension (`bb87fafac92c59795955f19f0b9e569c2db69b7b`)
also verifies destination-only ZERO/ONE blending uses a translucent pass
(`pass=1`, `no_op=0`) in both UNORM and sRGB Gallium framebuffers. All eleven
draw/state transitions pass offline. This narrows one classification hypothesis;
it does not prove the cause of the CTS pixel failures.

The hardware ledger remains **2,275 Pass / 48 Fail / 52,476 NotRun** out of
54,799. The 48 pixel failures, synchronous submission, restricted context
lifetime and remaining GLES coverage are unresolved. This is a regression
qualification of the efficiency corrections, not full GLES 3.0 conformance.
