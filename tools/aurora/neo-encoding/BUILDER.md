# Dedicated Neo builder

Lima instance: `neo-builder`, Ubuntu ARM64 with Apple Virtualization.framework,
12 vCPUs, 24 GiB RAM and a 96 GiB sparse disk.

```sh
limactl start neo-builder
limactl shell neo-builder
```

Only `/Users/mcruz/Developer/aurora-silicon/neo-linux` is shared writable from
the host. Linux source/build trees live on the VM's case-sensitive filesystem
under `$HOME/neo/sources` and `$HOME/neo/build`, avoiding the Linux filename
collisions in the existing macOS checkout. Use this VM for all new Neo builds.
The separate `j813-builder` instance remains the M5 builder.

Provision with:

```sh
limactl shell neo-builder bash \
  /Users/mcruz/Developer/aurora-silicon/neo-linux/scripts/provision-builder.sh
```

Provisioning is complete. Verified inside the guest: 12 CPUs, approximately
24 GiB RAM, `aarch64`, Clang/LLVM 21.1.8, Rust 1.93.1, and bindgen 0.72.1.
The `xt_CONNMARK.h` and `xt_connmark.h` source files have distinct inodes.
The only writable host filesystem mount is the Neo work directory above.
The provisioning script verifies `artifacts/source-manifest.json` and preserves
previously unpacked source trees, including local edits.

The initial all-Asahi Mesa build exposed three existing Gallium/OpenGL API
errors in `agx_state.c`: `agx_ppp_fini` now takes a chip argument, while those
call sites still pass two arguments. Evidence is saved in
`logs/build-mesa-gallium-failed.log`. The active Mesa build profile selects
Asahi Vulkan and its G17P tests, matching the initial compute bring-up path.
OpenGL has not been built or validated; no source workaround was applied.

Run builds from the host:

```sh
limactl shell neo-builder bash /Users/mcruz/Developer/aurora-silicon/neo-linux/scripts/build-kernel.sh
limactl shell neo-builder bash /Users/mcruz/Developer/aurora-silicon/neo-linux/scripts/build-uboot.sh
limactl shell neo-builder bash /Users/mcruz/Developer/aurora-silicon/neo-linux/scripts/build-mesa.sh
```

U-Boot has built successfully; its binary, matching ARM64 ELF and configuration
are exported under `artifacts/u-boot/`. Mesa's Vulkan build and staged install
also completed successfully; its G17P test executables are exported under
`artifacts/mesa-tests/`. The kernel build completed successfully with exit code
0: `7.1.6-aurora-neo`, a 58,264,064-byte ARM64 Image with 16 KiB pages, the
J700 device tree, and a 3,441,424-byte `asahi.ko`.
Build output is in `logs/build-kernel.log` and `logs/build-mesa.log`.
Completion markers are `NEO_KERNEL_BUILD_COMPLETE` and `NEO_MESA_BUILD_COMPLETE`;
absence of those markers is not a successful build. Do not start duplicate
builds while their current processes are active.

The Mesa Asahi suite ran in the ARM64 VM: **99 passed, 9 failed** out of 108.
This is CPU-side testing, not a Neo GPU hardware pass. The failures are
`asahi-agx3-packing`, `agx3_compile_fail_closed`, `agx3_multi_store`,
`agx3_event_wait`, `agx_tests`, `agx3_opcode_mapping`, `agx3_graphics_compile`,
`asahi-g17p-advanced-blend`, and `asahi-agx-ioto-program-core`.
See `logs/test-mesa-asahi.log` and `logs/test-mesa-asahi-detail.txt`.
These failures remain unresolved; the original pinned sources are unchanged.

The kernel overlay is `neo.config`, applied to the pinned tree's arm64
`defconfig` and `asahi.config`. It requires the 16 KiB page configuration,
Rust, AGX as a module, and the Apple DockChannel console. Matching full kernel
symbols are retained in `~/neo/build/linux/vmlinux` and exported as
`artifacts/linux/vmlinux`. Installed kernel modules live in
`~/neo/build/modules`; Mesa's staged installation lives in
`~/neo/build/mesa-install/opt/neo-mesa`. Nothing is installed onto the Neo by
these build scripts.

`scripts/build-initramfs.py` packages a 13,243,399-byte RAM initramfs with
BusyBox, kmod, the exact AGX module, the Vulkan loader/driver and selected G17P test
executables. The module dependency lookup was verified inside the packaged
root with chroot. `artifacts/linux/manifest.json` records file hashes.

Native Linux uses its own RAM-only Stage 2 in `artifacts/m1n1-linux/`, built
inside the VM by `scripts/build-m1n1-linux.sh`. It applies the saved KIS source
patch plus `artifacts/m1n1-linux-kis.patch` to the pinned m1n1 commit. That patch
includes the build configuration and skips normal USB initialization during
T8140 KIS native handoff. The compiled ELF is checked for the guard's marker.
The VM's Linux GNU compiler failed to link this freestanding image; the
supported Clang/LLD path with `BUILDSTD=1` built successfully. The working
macOS HV Stage 2 in `neo-update/` is unchanged. Never install this candidate
as persistent Stage 1.

`artifacts/m1n1-linux-gpu.patch` adds a scoped T8140 fixup for the four UAT
reserved-memory regions, using live ADT addresses and sizes. The first Linux
boot proved that the pinned generic GPU fixup skipped T8140 and left all four
regions zero. The original shell-verified Stage 2 is retained separately in
`artifacts/m1n1-linux-shell-verified/`; the GPU fixup is hardware verified.

The native launch uses the existing guarded `neo-update/kis-chainload.py`,
with its 1024-byte KIS writes and original gzip length. `boot-linux.py` then
verifies the image manifests, target identity, Stage 2 base, and Linux page
size before loading the kernel, initramfs and J700 DTB. The verified initial
path uses `--keep-boot-cpu --direct`, starts all six CPUs, retains the original
boot CPU, and bypasses U-Boot. It uses the Neo's DockChannel address `0x30812c000`.
The current run is recorded by `logs/latest-native-run.txt`.

Stop and restart the builder with `limactl stop neo-builder` and
`limactl start neo-builder`. Stop only after builds have finished or been
intentionally interrupted.

Source archives were made directly from verified Git objects, without changing
the existing Linux, Mesa, or J813 worktrees:

| Component | Repository / branch | Commit |
|---|---|---|
| Linux | aurora-silicon/linux-aurora / drm-agx3 | 4ac2664258f370cf7b03327da742b5885f91ca0b |
| Mesa | aurora-silicon/mesa / agx3 | e58d46899eb1f351863bccab4b6e8c87ed32b95e |
| U-Boot | aurora-silicon/u-boot / J700 | b0f6d36e1c3ff4b4eceec663ded023830c237a30 |

Latest-branch checks were performed on 2026-09-08. The kernel checkpoint file
reports proven Neo Vulkan compute and incomplete fragment rendering. Hardware
video encoding is separate from AGX: the target's macOS IORegistry identifies
AppleAVE2Driver, while this kernel tree currently contains AVD decoding and ISP
drivers, but no Apple video encoder driver.

The working macOS/KIS recipe remains in `../neo-update/WORKING_KIS_BOOT.md`.
Do not replace installed Stage 1 or apply the staged macOS update. First Linux
bring-up uses RAM-loaded artifacts. Native Linux shell, advancing uptime,
kernel identity and RAM mounts were verified on the Neo in
`logs/20260908T175648Z-native-vulkan-smp/console.log`. Vulkan compute then
passed 1,000 consecutive submissions with 32,000 exact values. This does not
qualify fragment rendering or hardware video encoding. The full recipe is
`WORKING_NATIVE_BOOT.md`.
