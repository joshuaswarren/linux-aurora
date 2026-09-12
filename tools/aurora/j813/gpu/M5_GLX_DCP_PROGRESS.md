# M5 GPU, DCP and GLX evidence — 2026-09-12

## Proven boundaries

- The combined RAM kernel initializes DCP at 2560x1664 and registers the M5
  render node. A Vulkan render in the same boot produced 65,536/65,536 exact
  pixels. This is coexistence proof, not a desktop rendering test.
- Xorg creates a glamor OpenGL 4.6 context through Zink/Honeykrisp and exposes
  a usable GLX provider. `glxinfo -B` identifies direct rendering on Apple M5.
- A linear GBM buffer registers with DCP using both its direct GEM handle and
  its imported DMA-BUF handle. `m5-gbm-dcp-smoke.c` tests this without GPU
  rendering or changing scanout.
- The real 256x256 `glxgears` frontend creates a Zink context and reaches its
  vertex shader compiler. All these Xorg/GLX probes set
  `HK_M5_ABORT_BEFORE_IOCTL=1`; no moving GPU gears result is claimed.

## Fixes and validation

The desktop archive omitted `gbm/dri_gbm.so` and pointed DRI entry-point
symlinks at libgallium. The runtime builder/packager now includes GBM and
`libdril_dri.so`, which exports Mesa's DRI loader ABI. A matched Gallium build
was installed to avoid mixing internal GBM/Gallium interfaces.

The KMS software winsys initialization was guarded only by `HAVE_SWRAST`.
A Zink build without a software rasterizer fell back to a winsys without
the display fd and returned GEM handle `UINT32_MAX`. Enabling that KMS path
for `HAVE_ZINK` preserves the display namespace. On M5, the same explicit
linear buffer changed from direct AddFB2 `-ENOENT` to success, with direct and
imported handles both equal to 1. Invalid modifiers are not treated as linear.

The M5 shader compiler now handles signed right-shift immediates required by
glamor's vertex shader. Four native-composer byte vectors and rejected operand
cases cover the AGX3c path while preserving AGX3b tests. Xorg's shader
compilation passed after that fix.

The first gears shader failure is a full-width floating compare/select,
`0 < x ? y : 0`. GPU1 uses `fseli32`, rather than AGX3b's `fsel`. The added
encoder preserves register payloads, condition reversal and lifetime hints;
immediate results are limited to zero. Four native-composer vectors cover
comparisons, exchanged alternatives and upper-register/wait fields. Compiler
suites `agx3_alu_core`, `agx3_isel_core`, `agx3_select_core` pass. Hardware
execution of these new shader forms remains unqualified.

## Current failures and remaining phases

After several bounded Xorg probes, the next display run caused repeated DCP
DART write-protection faults and lost the serial console. The submission gate
was enabled. The exact cause is unresolved; successful AddFB2 and an earlier
clean Xorg startup do not prove sustained scanout or buffer teardown. Preserve
the previously working ShadowFB desktop path while investigating imported
buffer ownership and DCP DMA access.

The gears compiler also requested missing `LIBAGX_FILL_UINT4` (helper 32),
including the depth-clear path. It must be implemented or use a valid existing
graphics clear path; silently skipping a clear is not acceptable.

The experimental kernel still admits only one retained application VM,
256x256 single-layer single-sample targets, no depth/stencil or sampler heap,
64 render generations without wrap, and one application compute command.
Compute dispatch retires, but the existing readback test is incorrect. Full
desktop acceleration requires shader/helper correctness, depth and larger
target support, safe reusable queues/contexts, and sustained DCP presentation.

## Workbench evidence

- `logs/m5-fast-loop-20260912-115200/`: combined boot, both preflights, timings.
- `logs/m5-gpu-dcp-render-status-20260912.log`: exact-pixel render.
- `logs/m5-xorg-asr-result-20260912.log`: glamor compiler success.
- `logs/m5-gbm-dcp-smoke-linear-20260912.log`: invalid direct handle, successful
  explicit-linear DMA-BUF import.
- `logs/m5-gbm-dcp-smoke-kms-v2-20260912.log`: both AddFB2 paths succeed.
- `logs/m5-xorg-kms-v2-result-20260912.log`: Xorg framebuffer errors cleared.
- `logs/m5-glx-gears-compiler-result-20260912.log`: GLX renderer and real gears
  shader/helper failures.
- `logs/m5-gears-fsel-tests-20260912.log`: three compiler suites pass.
- `logs/m5-gears-fsel-probe-launch-20260912.log`: DCP fault onset; this run
  does not establish whether the updated gears shader passed on target.

The complete desktop Mesa runtime before the gears compare/select addition is
`m5-mesa-xorg-runtime-v2-20260912.tar.gz`, SHA256
`7d31c4ede6261e94583ba213ab6f993d85bb7514d1c0e3ea98b3a8c72a10031e`.
Keep runtime versions and deployment manifests explicit when testing updates.
