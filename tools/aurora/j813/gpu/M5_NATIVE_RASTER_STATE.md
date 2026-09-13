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

The candidate includes the preceding command-reservation correction. It has
not yet been tested on physical hardware. The last hardware qualification
remains v25: 2,275 Pass / 48 Fail from 2,323 cases. The 48 pixel failures,
synchronous submission, restricted context lifetime and remaining GLES
coverage are unresolved.
