# M5 GPU development publication, September 18

This publication preserves the accumulated J813 GPU development since the
September 13 AGX3 integration on the existing `j813` branches. It does not
claim new CTS coverage or full OpenGL/GLES/Vulkan qualification.

## Hardware changes

Mesa extends the native G17G compiler's arithmetic, selection, memory and
texture encoders and their instruction-vector regressions. The Gallium and
shared rendering code adds render-target and tile-format handling, an
eight-target RGBA8 end-of-tile reference, depth/stencil dimensions, sampler
and multisample state, and pre-rasterization varying handling. Native
unsupported instruction and rendering profiles continue to be refused.
The optional M5 compute and memory diagnostics are also retained.

The Linux driver extends depth admission and native multisample geometry,
adds retained-resource aliases and context rebinding for application-VM
handoff, and tracks successive compute submission generations. Admission
continues to reject unwitnessed multi-dispatch streams and stencil transfers.
The included multisample test checks the driver's actual geometry functions
against recorded native register values.

The native GL smoke tool and official GL 3.0/session runners retain their
renderer, driver-library, temperature and kernel-log checks. They are test
infrastructure; their publication does not establish passing hardware results.

## Publication checks

- Fetched the remote Mesa, linux-aurora and m1n1-aurora `j813` tips. All matched
  their corresponding local published base; no merge or rebase was needed.
- Rebuilt the current Mesa source with the existing native AArch64 build.
- Asahi host suite: **264 Pass, 2 ExpectedFail, 2 Fail**. The failing
  `asahi-g17p-precompiled-fill` and
  `asahi-g17p-gl-hypr-mixed-draw-shim-submit` also failed in the retained
  September 13 baseline. The complete host suite is therefore not green.
- Five focused kernel checks passed: `test_m5_depth_admission.py`,
  `test_m5_graphics_bundle.py`, `test_m5_multisample_geometry.py`,
  `test_m5_render.py` and `test_mmu_unmap_stack_bound.py`.
- Both added Python GL runners parse successfully, and the GL smoke source
  passes C11 syntax checking with the EGL/GLES headers in the build VM.
- Staged diffs pass whitespace checking and contain no merge-conflict markers.

No new kernel image build, target boot or hardware CTS run was performed for
this source-publication task. Earlier native reports apply to their recorded
driver and kernel hashes, not automatically to these newer sources. In
particular, the September 13 8,734-pass GLES result is not a validation claim
for the accumulated changes published here.

The m1n1 display-mapping and SPTM work was already published on its `j813`
branch and needs no additional commit. Local build outputs, editor metadata,
the saved Rust backup and unrelated case-collision changes are excluded.
