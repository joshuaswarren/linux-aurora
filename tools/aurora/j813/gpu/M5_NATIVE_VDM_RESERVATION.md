# M5 HAL300 command reservation correction — 2026-09-13

The fidelity audit found that native M5 Gallium used the AGX2 command-size
estimate before emitting HAL300 state. The estimate was **72 bytes**, although
the M5 state block alone occupies **116 bytes**. The correction derives the
M5 bound from its actual packet definitions and checks emitted bytes against
that bound in assertion-enabled builds.

Source/build and compiler-path checks below preceded the physical v27
regression, which includes this correction and raster-state reuse. All
2,275 prior passes are preserved, with the same 48 failures and complete
retirement evidence. See [v27 hardware evidence](M5_NATIVE_RASTER_STATE.md).

## Why the old estimate was wrong

`src/gallium/drivers/asahi/agx_state.c` routes M5 state through
`agx_encode_g17g_state`. Its state publication contains:

| In-stream records | Bytes |
| --- | ---: |
| Native vertex state update | 28 |
| Viewport and depth-bias/scissor base PPP pointers | 16 |
| Nine graphics PPP pointers | 72 |
| Total state | 116 |

The bound must also include the largest native Index List (36 bytes for an
indexed indirect draw), the optional restart-state records (8 bytes), and
the conditional transform-feedback barrier packet (4 bytes). This gives a
**164-byte upper bound**, expressed using packet-size constants rather than
an unexplained enlarged buffer size.

The reservation helper separately retains the existing stream-link space and
0x800-byte overread padding. That padding masked the underestimate: this
audit does not establish an observed allocation overflow or link corruption.
The bug allows draw packets to consume bytes intended as overread padding.
The correction preserves the intended padding at command-buffer boundaries.

The draw stream remains linked within its existing batch when capacity is
exhausted. No new per-draw flush, render-pass split, barrier, or hardware wait
was added. M1/M2 and G17P reservation formulas are unchanged.

## Reproduction and validation

1. With the v25 source and only the new emitted-size assertion added, the
   unmodified case
   `dEQP-GLES3.functional.fragment_ops.blend.default_framebuffer.equation_src_func_dst_func.add_one_zero`
   aborts at the assertion. It emits more than the reserved 72 bytes.
2. With the corrected formula, the same case reaches a single submission
   containing **two draws**, with no size assertion failure.
3. The complete **2,304-case** blend group reaches the end of its compiler-only
   run with no size assertion, compiler refusal, dropped-program diagnostic,
   or M5 state-profile rejection. The unique started set matches the expected
   blend inventory.
4. The existing AGX3 packing test completes **99,623 checks** successfully.
5. Gallium builds successfully and `git diff --check` passes. The existing
   `launch_sources` uninitialized-value warning remains; it is not introduced
   or resolved by this change.

All compiler-only runs use the G17G no-op DRM diagnostic in the build VM.
It deliberately returns an error for every submission. In the final ordered
capture, **2,304 submissions are rejected** and the evidence validator marks
**all 2,304 cases invalid**. The raw 89 image-comparison Pass results from that
run are not hardware passes and never enter the campaign ledger.

Standard output and standard error must be merged in the remote process
before transport. The first full diagnostic capture let the transport merge
them, which reordered some messages across case boundaries. The final run
uses remote `2>&1`. The physical CTS runner already opens one shared log with
`stderr=subprocess.STDOUT`.

Retained workbench evidence:

- `logs/m5-vdm-reservation-old-bound-compiler-only-20260913.log`
- `logs/m5-vdm-reservation-corrected-compiler-only-20260913.log`
- `logs/m5-vdm-reservation-all-blend-ordered-compiler-only-20260913.log`
- `logs/m5-vdm-reservation-packing-tests-20260913.log`
- `logs/m5-vdm-reservation-final-build-20260913.log`

Candidate: `artifacts/gpu/native/libgallium-m5-v26-audit.so`, SHA256
`a7b8f900aaff7d4a97abdf3d1039f2c274d5dbe9bf764beac983ea4d8cf89067`.
The v27 combined candidate verifies the actual loaded library, preserves the
2,323-case regression and captures every GPU retirement. The standalone v26
artifact above was not separately installed. This correction does not resolve
the 48 blend pixel failures or the scheduling/lifetime findings in the main audit.
