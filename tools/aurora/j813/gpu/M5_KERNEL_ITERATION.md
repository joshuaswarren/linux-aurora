# M5 kernel iteration over normal USB

The qualified RAM carrier keeps normal m1n1 USB available for payload upload.
`boot-linux.py --j813-source-usb=PORT` uploads and reads back each payload over
that connection, checks its SHA256, then switches only the M5 to KIS for the
U-Boot/Linux console. Resident Stage 1 and internal disks are unchanged.

The source must be m1n1 USB vendor/product `1209:316d` at host location
`0x01100000`. Both loaders check chip `0x8142` and source transport. The VDM
helper is scoped to RID1; it must never fall back to the Neo's RID0.

## One-command reboot and runtime restore

Run from the existing J813 workbench with its pyserial-enabled Python. Build a
candidate first into `artifacts/gpu/native/NAME`, containing `Image`,
`initramfs.cpio.gz`, and `t8142-j813.dtb`. `--gpu-candidate=NAME` keeps it separate
from the qualified `rtkit-probe` payload.

```sh
python linux/tools/aurora/j813/gpu/run-m5-kernel-loop.py \
  --workbench "$PWD" --carrier /path/to/qualified/m1n1.bin \
  --candidate NAME \
  --server http://HOST:PORT \
  --runtime RUNTIME.tar.gz --runtime-sha256 SHA256
```

The HTTP server must serve the prepared artifact directory containing the
archive and `prepare-m5-vulkan-test-root.sh`. The existing `configure-wpa.py`
uses the workbench's protected Wi-Fi credentials. They are not command arguments.
The host admin credential is supplied by the existing scoped VDM helper.

The default starting state is the M5 Linux serial shell. If already at the
resident m1n1 proxy, add `--from-m1n1`. Omit all three runtime arguments for a
kernel/DRM-only boot. Each stage gets a separate log and elapsed time under
`logs/m5-fast-loop-TIMESTAMP/`; failures stop without an automatic reboot/retry.
The runner locks against another instance of itself. Other tools must still
respect exclusive ownership of the M5 proxy/console while it runs.

## Small Vulkan runtime

For combined GPU/DCP work, build `build-m5-gpu-dcp-candidate.sh` in the isolated
J813 builder and use `--candidate gpu-dcp-candidate --with-dcp`. This selects
`t8142-j813-gpu-display.dts`, enables both GPU ASC providers, and loads
`appledrm live_discovery=1` before Asahi. The runner requires the native
2560x1664 connector mode before proceeding.

For offscreen GPU fault isolation, add `--defer-dcp` together with
`--with-dcp`. The combined carrier, DT and DMA preflights still apply, but
the runner defers loading `appledrm`. It prints `M5_DCP_LOAD_DEFERRED` and
does not claim display readiness. This avoids an intermittent DCP startup
stall blocking tests that only inspect GPU buffers. A hardware run restored
the GPU and small Vulkan runtime in 45.038 seconds with DCP loading deferred.
Remove this option for desktop and presentation qualification.

This path requires the combined carrier built from m1n1 `j813` commit
`c7a06d01ccb5ff35de1bb6fc3b327f78f69ac5f3`, binary SHA256
`3eb341d34e923abe578de7e670ed948c84fcb6f50084a23dd132898fd3b4b409`.
Both GPU and display preflights verify the combined carrier, physical
reservations and DMA mappings. The qualified GPU-only carrier remains a
separate artifact. The combined boot reached DCP, GPU and Vulkan recording
readiness in 41.251 seconds, starting from resident m1n1; it excludes reboot
time. A 65,536/65,536 exact-pixel render then passed with DCP initialized.

For Xorg/GLX, `package-m5-mesa-runtime.py --output NEW_DIRECTORY` packages the
full userspace build. It includes the DRI GBM backend and DRIL loader shim;
the DRI entry-point symlinks must target `libdril_dri.so`, not libgallium.
Set `GBM_BACKENDS_PATH=/opt/m5-mesa/lib/gbm` with the other Mesa overrides.
The small Vulkan root below is still useful for submission tests and does not
replace the full desktop root. See `M5_GLX_DCP_PROGRESS.md` for current limits.

Package ABI-compatible inputs from the qualified Mesa and desktop archives:

```sh
python linux/tools/aurora/j813/gpu/build-m5-vulkan-test-root.py \
  --mesa-runtime /path/to/mesa-runtime.tar.gz \
  --desktop-root /path/to/desktop-root.tar.gz \
  --driver /path/to/stripped/libvulkan_asahi.so.gz \
  --readback /path/to/m5-vulkan-readback-render \
  --render /path/to/m5-vulkan-render-debug \
  --output /path/to/transfer/RUNTIME.tar.gz
```

This produces a Vulkan-only root, including the ELF loader, Vulkan loader,
Honeykrisp, its dependencies and the fixtures. Gallium/Zink, LLVM, Xorg and
XFCE are excluded. The installer verifies the archive, restores `/gpu-test` in
RAM, checks all three dynamic-loader paths, and records a Vulkan draw without
submitting GPU work. It refuses to replace an existing `/gpu-test` mount.

Hardware tests use the host BusyBox `env` outside the chroot:

```sh
env ASAHI_M5_BRINGUP=1 MESA_SHADER_CACHE_DISABLE=true \
  LD_LIBRARY_PATH=/opt/m5-mesa/lib \
  VK_DRIVER_FILES=/opt/m5-mesa/asahi_icd.json \
  XDG_RUNTIME_DIR=/run/user/0 AGX_RENDER_UNCACHED=1 \
  AGX_RENDER_READBACK=1 AGX_RENDER_HOLD=1 \
  AGX_RENDER_OUTPUT=/run/readback.rgba \
  chroot /gpu-test /opt/m5-mesa/bin/m5-vulkan-readback-render
```

The current experimental kernel retains the first submitted application's VM
and admits one application compute command per boot. Recording-only validation
does not consume this submission. A new kernel or first-application test still
requires a fresh boot; this workflow makes that boot cheap.

## Hardware measurements, 2026-09-12

| Operation | Previous path | Fast path |
|---|---:|---:|
| Candidate upload and Linux boot | 167 s over KIS | 12.075 s over normal USB, KIS console |
| Payload upload including SHA256 readback | Included above | 4.817 s |
| Vulkan runtime archives transferred | 410,789,174 bytes (392 MiB) | 9,208,546 bytes (8.8 MiB) |
| Runtime restore and recording validation | About 2 min for the full root | 2.424 s |
| Linux reboot through Vulkan recording readiness | Not separately timed | 41.490 s |

The total includes USB enumeration, carrier, GPU module loading, Wi-Fi setup,
DHCP and runtime restoration. It excludes kernel compilation and subsequent
hardware work. The last incremental kernel build took 43 s, so build plus this
measured loop is approximately 85 s, not yet a single measured combined run.

Evidence: `logs/m5-fast-loop-20260912-113017/timings.json`, its per-stage logs,
`logs/m5-fast-usb-boot-20260912.log`, and
`logs/m5-fast-runtime-hardware-summary-v2-20260912.log` in the workbench.
Both normal USB boots reached the native shell. Four payloads passed SHA256
readback. The small runtime's hardware draw produced 65,536/65,536 exact pixels.
Its compute readback reproduced the existing failure: 0 exact pixels, 65,535
sentinels, first pixel `ff000000`, both guards intact. Correct compute readback,
sustained GLX gears and a complete Zink desktop remain unproven.
