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
- The real 256x256 `glxgears` frontend creates a Zink context and now gets past
  its geometry arithmetic encoding failures. Fragment compilation and graphics
  pipeline state remain blocked. All these Xorg/GLX probes set
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

Subsequent native-backed compiler changes on Mesa `j813` cover saturation,
halfword EQ-zero selection, population counts, full-width address addition
lowered into carry arithmetic, and floating operations with native dependency
waits and operand lifetimes. The latest change handles constant floating-point
scaling (`1d1be0a58ab`). Five compiler core suites pass. The complete backend
suite passes 296/298 tests; the two failures were reproduced on the unchanged
baseline (U16 conversion domains and texture export retirement).

The latest real gears probe has no geometry or fragment instruction-encoding
refusals. The EQ-one compare is implemented in `a9db953fdcc` and its 167 isel
checks pass. `83dcceeeaa5` admits the native color vertex layouts (UVS8/9), with
2815 VDM checks passing, and stops advertising unsupported M5 geometry and
tessellation shaders. Zink then uses Gallium's triangle conversion for legacy
quads instead of introducing an unsupported geometry pipeline.

Gears now reaches ordinary indexed triangles. Its current PPP refusal is
color1/depth1/stencil0, samples1, UVS9, coefficient bindings1/count4, and a full
256x256 viewport/scissor. Native color/depth reference capture is next. The
existing no-varying/no-depth PPP profile cannot represent this state.

## Current failures and remaining phases

A later ShadowFB/XFCE boot also lost its physical display after a display power
cycle. The first DCP power-on succeeded; shutdown then reported
`setPowerState(0) timeout 1000 ms`, and the next power-on reported
`wait for power timed out, connector will be broken`. The user confirmed a black
panel even while Xorg screenshots contained a desktop and DRM reported an active
CRTC. No application GPU command had been submitted in that boot. This is a
separate observed failure from the earlier glamor DART fault below; a shared
cause has not been established.

Recovery starts Xorg with `-s 0 -dpms` and disables XFCE blanking. It also binds
devpts into the desktop root after mounting it, so terminal PTYs are available.
Disabling power saving avoids the failing path during bring-up; it does not
qualify DCP suspend/resume. A RAM reboot is currently required after the DCP
power command channel stops responding. After the 49.781-second recovery boot,
the user confirmed the desktop was visible and the pointer moved. The terminal
also opened successfully, including a PTY allocation as the desktop user.

`start-m5-shadow-glx.sh` and `start-m5-shadow-xfce.sh` preserve this recovery
configuration. They expect the existing J813 RAM workbench, `/desktop` with its
shared `/dev`, `/proc`, `/sys` and `/run` mounts, and the matched desktop Mesa
runtime. Start the X server once, then XFCE; the scripts reject duplicate
instances. The X server keeps `HK_M5_ABORT_BEFORE_IOCTL=1`, and XFCE remains
software rendered. They do not recover a DCP command channel already wedged.

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
- `logs/m5-gears-scale-unique-boundaries-20260912.log`: fragment and pipeline
  failures after the geometry arithmetic fixes.
- `logs/m5-gears-scale-final-backend-tests-20260912.log`: 296/298 backend tests.
- `logs/m5-dcp-live-power-state-20260912.log`: DCP power timeouts despite active
  DRM state and disabled DPMS after the failure.
- `logs/m5-fast-loop-20260912-134643/`: recovery RAM boot completed in 49.781 s.
- `logs/m5-gears-half-eq-one-result-20260912.log`: fragment compilation passes.
- `logs/m5-gears-ordinary-result-20260912.log`: ordinary triangle draw reaches
  the color/depth PPP state boundary, without transformed-draw errors.

The complete desktop Mesa runtime before the gears compare/select addition is
`m5-mesa-xorg-runtime-v2-20260912.tar.gz`, SHA256
`7d31c4ede6261e94583ba213ab6f993d85bb7514d1c0e3ea98b3a8c72a10031e`.
Keep runtime versions and deployment manifests explicit when testing updates.
