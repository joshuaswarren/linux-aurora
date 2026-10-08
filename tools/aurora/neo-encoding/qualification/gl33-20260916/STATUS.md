# Official desktop OpenGL 3.0 CTS status

The latest full run attempted **879 of 43,901 selected test executions (2.00%)**. These executions span the required configurations; repeated diagnostic runs are not added to coverage.

| Result | Selected executions |
| --- | ---: |
| Pass | 807 |
| Fail | 68 |
| Internal error | 3 |
| Interrupted | 1 |
| Not run | 43,022 |

**Fifteen additional configuration checks passed.** They are excluded from the selected-test totals above. All 52 sessions remain accounted for, but only the configuration session and no-context session are complete. No complete main session or conformance pass is claimed.

This is unmodified Khronos `opengl-cts-4.6.8.1`, profile `gl30`, built for X11/EGL. CTS uses native Asahi on the Apple A18 Pro GPU. The display server uses its separate existing Zink backend. The full run has no case filtering, capability overrides, test edits or waivers.

## Proven changes

- Empty direct draws no longer abort the native draw encoder. Transform-feedback rendering failures remain recorded.
- Corrected the 2D texture compression selector. The original crashing official case now passes, with the before/after descriptors differing only in that bit.
- Added the missing dependency on a separate stencil-plane writer. The isolated depth/stencil case now finishes without the ownership assertion, while its rendering failure remains recorded.
- Corrected the compression selector for extended arrays, 3D and cube descriptors. The faulting official shadow-array case now passes on hardware; its first two descriptors differ only in the compression bit, with identical addresses. Array/3D/cube CPU regressions and captured golden bytes pass.

In the first main configuration, **651/651 shader cases pass** and **126/162 texture-repeat cases pass**. The texture-repeat failures involve depth/shadow sampling. Earlier clip-distance and transform-feedback failures also remain.

The latest stop is an assertion rejecting an unsupported background tile layout during `KHR-GL30.draw_buffers.draw_buffers_1`, the final selected case in the first main configuration. It requires multiple color attachments and a disabled draw-buffer slot. An isolated run reproduces it without a GPU fault.

The multisample color case now completes with a rendering failure after adding native persistent-store descriptors and EOT selection. Diagnostic CPU reads confirm nonzero data in the stored R8 image, while the resolve still returns incorrect pixels. A separate correction accepts the allocator's minimal eight-byte slot for depth-only multisampling, allowing subsequent tests to run. The other 43,022 executions and the failed/error/interrupted cases remain unresolved.

Latest evidence: `fast-iterations/sparse-identity-full/results/sparse-identity-full/analysis.json`, with complete per-case and per-session ledgers beside it. Library SHA256: `c5826185f5ff6c80d176f0836b952addc19525445f1c5c409e737dca693cabfa`. The full unfiltered attempt took29.98seconds including candidate installation/collection/analysis (CTS10.70seconds), with no GPU fault. All43,916 ledger entries exactly match the preceding dense-mrt-full run.

Sparse attachment identity and loader class masks now pass six independent native oracles and18real NIR epilog/clear/reload paths. Complete sparse-vs-compact EOT binaries prove active-order PBE packing with preserved synchronization classes. Sparse BG/EOT runtime resource mapping remains incomplete; no additional official Pass or sparse pixel qualification is claimed. See `SPARSE-MRT.md`.

Dense three-to-eight-target clear/reload/store integration and the eight-target reload register-allocation fix now pass CPU tests. Hardware prepares the eight-target programs and submits the clear batch; the subsequent disabled-slot layout is the next rejection. This is not pixel qualification or an additional official Pass. Final CPU regressions remain264Pass,2ExpectedFail,1unchanged pre-existing golden mismatch. See `DENSE-MRT.md` for native metadata, before/after failure evidence and sparse-layout constraints.

Earlier evidence remains in `hardware-inventory/x11-msaa-depth-full`, `hardware-inventory/draw-buffers-isolated`, `TEXTURE-FAULT.md`, `STENCIL-HAZARD.md`, `ARRAY-TEXTURE-FAULT.md` and `MSAA-FRAMEBUFFER.md`. See `FAST-LOOP.md` for measured4.63s focused and14.61s full-attempt iteration times while reusing a healthy boot and Xorg.
