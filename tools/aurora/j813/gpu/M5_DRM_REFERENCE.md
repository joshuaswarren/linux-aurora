# M5 application DRM interface

The experimental J813/T8142 adapter keeps both M5 firmware processors running
and exposes an Asahi render node. It uses the M5 runtime and SKSM graphics
submission path, with ordinary DRM VM, GEM, queue and syncobj interfaces.

GLX gears now renders repeatedly on the M5 GPU through Zink and Honeykrisp
in a DCP-backed XFCE session. X11 presentation uses Mesa's software WSI and
linear render targets; XFCE itself still uses CPU rendering. This qualifies
the gears workload, not general Vulkan applications or an accelerated compositor.

## Resizable GLX gears, 2026-09-13

The adapter accepts render dimensions from 1 through 16384 on either axis,
matching Mesa's advertised image limit. D32 storage validation now handles
small Morton layouts with 128-byte alignment as well as 64x64 tiled images.
Single-layer depth attachments may use zero for the unused array stride;
nonzero strides must match the computed layout. Compression remains disabled
when either dimension is below 16. Mapping coverage and the existing render
profile checks remain enforced.

The hardware resize test passed 15 cases, including 1x1, 16x16, 1027x813,
1920x1080, 3840x2160, 16384x1 and 1x16384, followed by 80 rapid resize requests
and a return to 512x384. Each case retired GPU work; ordinary windows also
had changing XGetImage pixels. One-pixel and very thin windows can legitimately
contain only background, so those use completion checks rather than a motion
assertion. This does not qualify every dimension pair or a 16384x16384 allocation.

XFCE maximize (2560x1613), fullscreen (2560x1664), and restoration to maximized
also passed with changing full-client captures and paired TA/fragment
completions. The tests are `test-m5-gears-resize.py` and
`test-m5-gears-window-states.py`; run with the desktop Python, DISPLAY=:0,
the current gears PID file and a new output directory. They change the real
window. The window-state test leaves it maximized.

Private evidence archive SHA-256 values:

- Resize: `77314841023810ffbe61bd1af8783062d5be7b496036e5a6644efd1763839956`
- Window states: `f771df704d5021b5ef8ad6bc7cb5f350a5fcfab3fca3478c81d35f90d7a13a28`

## Initial runtime and limits (historical)

On the RAM candidate with the measured M5 firmware profile and providers,
`modprobe asahi m5_initdata_test=14 m5_fw_trace=0` selects the adapter. Mode13
starts a retained firmware session without registering DRM. Modes1 through12
remain diagnostics with their original stop-request checks.

The first application render is connected to the M5 job constructor and
paired queue submission. It accepts one render command, 256x256, one layer,
one sample and one cluster, without depth/stencil, samplers or timestamps.
The remaining dispatch/resource controls still use the native reference
profile. Repeated application jobs and general render-state derivation remain
unfinished. Recovery-only firmware tracing (`m5_fw_trace=512`) is supported;
continuous trace producers remain rejected for this synchronous adapter.

Input fences are waited outside the runtime lock. Once an output fence is
allocated, the submission guard publishes binary and timeline outputs on
both success and validation/submission failure. A published hardware failure
marks the VM, quarantines its mappings and retains firmware/job ownership.
Stop-request success is not treated as proof that hardware has halted.
This experimental session is retained until a fresh boot; do not hot-reload it.

## Userspace ABI check

From the kernel checkout, build the standalone ARM64 test with:

```sh
cc -static -Os -s -Wall -Wextra -Werror -D__user= -Iinclude/uapi/drm \
  tools/aurora/j813/m5-drm-smoke.c -o m5-drm-smoke
```

Run `m5-drm-smoke /dev/dri/renderD128` on the M5 after loading mode14. It checks
the hardware identity, VM/queue lifecycle, GEM mappings in Mesa's M5 address
windows, rejection of driver-reservation mappings, and error-fence publication
for malformed commands. It checks VM status after validation errors and
destroys the objects before repeating. It publishes no GPU command.

For timeline syncobjs, `SYNC_IOC_FILE_INFO` with zero requested fence details
reports the chain wrapper's retirement status. Requesting its contained fence
details is necessary to inspect the job's error. The test verifies the actual
job error rather than treating a successfully retired wrapper as lost error
propagation.

The local `run-m5-drm-smoke.py` helper verifies the J813/kernel/session evidence
on the M5 KIS console. Its explicit `--existing-session` option repeats only
the ABI check; it never reloads the retained module.

## Hardware evidence, 2026-09-11

The RAM-booted J813 kernel registered `renderD128` and reported T8142, G17G,
HAL300, USC3 and ten enabled GPU cores. The complete test passed three VM and
queue lifecycles, nine GEM bindings, three rejected driver-reservation binds,
nine rejected submissions and eighteen correctly signalled error fences.
CPU buffer contents survived the binding checks, and validation failures did
not poison VM status. No GPU command was published by this test, and no panic
or firmware restart appeared in its log.

## First application triangle, 2026-09-12

The userspace `m5-drm-render` test created a VM, GEM mappings, a queue and a
syncobj, then submitted one TA/fragment draw through the DRM render interface.
With `--native-va --all-reference-pages`, all 334 captured pages were mapped
at their original addresses with their captured read/write permissions.
The submit and completion-fence wait succeeded. All 65,536 pixels matched:
18,432 foreground, 47,104 background, zero unexpected or incorrect pixels.
No GPU restart or panic was reported. The 262,144-byte RGBA output SHA-256 is
`ce4e527e102b5e2f3f0585a6c045bc3eca14e5d225da0a8f3dc24b53f8c60a13`, identical
to the earlier kernel-driven reference triangle.

The smaller 203-page native-address control had failed without changing any
output pixels. The kernel and firmware were unchanged for the successful
334-page control. This establishes missing resources in the reduced set as
the blocker for that test; the exact necessary subset is not isolated yet.
This does not qualify relocated commands or Mesa-generated shaders.

A client may reserve the first 4 MiB of the compact address aperture for its
own command buffers. The driver allocates its private resources after that
prefix, and command validation still rejects overlap with the driver's arena.
The test requires a fresh session: this adapter currently admits one draw.

The boot helper now checks all six prepared GPU memory reservations against
the live ADT, including the GPU's phandle bindings, before entering Linux.
Its qualified carrier is identified by image hash; the older carrier that
exports empty UAT reservations is unsuitable for this GPU profile.
Native program bundles and hardware memory captures are private test inputs
and are not distributed in this repository.

## Mesa-generated fragment, 2026-09-12

A fresh application session also rendered a different fragment colour
compiled from NIR by Mesa's AGX3c backend. The exporter
`agx3_graphics_compile_test --m5-export-flat-variant FILE` generates a
60-byte shader for RGB (0.75, 0.25, 0.5), with two GPR halves, zero staging,
and no push data, scratch or preamble. It uses no captured instruction bytes.
Shader SHA-256:
`fcde505b78e36ad58809d27fda50461615f45642afc92a338772b4264e9b9755`.

The renderer's `--generated-fragment=FILE` option installs it at the existing
fragment entry, retaining the qualified native-VA layout and all 334 pages.
The GPU produced exactly 18,432 pixels of RGBA (191,64,128,255), 47,104 of
(16,16,16,255), and zero incorrect pixels. Submission and fence wait succeeded
without restart or panic. RGBA output SHA-256:
`5e74e188b0e82cc3961282f8c7fd8e7223c345e780069b2f600b05466dbd0d08`.

This qualifies a newly compiled fragment through the application interface.
Vertex instructions, runtime programs, command state and resources still
come from the reference bundle. Complete Mesa command generation, repeated
application draws and OpenGL/Vulkan applications remain unqualified.

Tested `asahi.ko` SHA-256:
`757542f8f41ed668680b58e855dfe5ba2fc0606c9ce4bc51e36ebb2f490880ad`.
The local evidence is `logs/m5-drm-smoke-mappings-20260911.log`. Successful
application rendering remains a separate qualification step.

A fresh RAM boot of the same module also passed the mode12 three-frame
native-program regression in context4. Both queues retired, the scheduler
unlinked, stamps advanced through100/200/300, and each output had18432
foreground and47104 background pixels with no wrong or unexpected pixels.
Output was reset before frames2 and3. Log:
`logs/m5-drm-regression-graphics-20260911.log`.
