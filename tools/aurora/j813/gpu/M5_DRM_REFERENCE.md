# M5 application DRM interface

The experimental J813/T8142 adapter keeps both M5 firmware processors running
and exposes an Asahi render node. It uses the M5 runtime and SKSM graphics
submission path, with ordinary DRM VM, GEM, queue and syncobj interfaces.

The first application triangle is hardware-qualified with a captured native
program. General Mesa rendering, Vulkan, glxgears and accelerated XFCE remain
unfinished. Native DCP presentation and software XFCE were verified separately;
the GPU and DCP still need integration in a single accelerated desktop.

## Runtime and current limits

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
