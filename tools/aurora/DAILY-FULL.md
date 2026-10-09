# J700 daily-full accelerator and alarm integration

This is a local integration candidate based on `hybrid/daily-next` at
429d8f17a42160d3690080b5b86376d084f0c3ca. Source qualification on another
kernel is not hardware qualification of this combined tree. No firmware,
model captures, or proprietary binaries are included.

## Kernel configuration and boot DT

Merge `arch/arm64/configs/j700-full.config` after the normal daily config and
run olddefconfig. Required additions are:

```
CONFIG_MODULES=y
CONFIG_FW_LOADER=y
CONFIG_DEBUG_FS=y
CONFIG_OF_OVERLAY=y
CONFIG_APPLE_ANE_BRINGUP=m
CONFIG_APPLE_ANE_FW_TEST=m
CONFIG_VIDEO_APPLE_AVD=m
CONFIG_RTC_DRV_APPLE_PMU=m
CONFIG_CRYPTO_LIB_SHA256=y
CONFIG_CRC32=y
```

Retain the daily 16 KiB ARM64 page size, IOMMU, SPMI/NVMEM and existing
media-controller/V4L2 options. ANE/AVD/RTC are optional modules. ProRes,
JPEG and AVE remain external modules with their own Kbuild files.

The normal `apple/t8140-j700.dtb` now includes AVD and its DART, ProRes,
JPEG, and the PMU RTC child. Missing AVD firmware returns a probe error.
ProRes/JPEG submit nothing until explicitly commanded. RTC probe requires
`experimental=1`; its default rejection performs no PMU I/O. The existing
Baku NVMEM provider remains bound and publishes its RTC child using the
same regmap, preserving the reboot and clock-offset cells.

The opt-in `apple/t8140-j700-encoding.dtb` additionally includes AVE. Do not
use it for the first boot: union m1n1 checks the canonical AVE node's
existence even if disabled and can abort before Linux. Its firmware
reservation and checksum preflight must pass first. A module setting
cannot protect against that pre-Linux failure. ANE remains an ordered
runtime overlay experiment because its DARTs must not probe before the
power sequencer has raised the child rails and CE.

## Object checks and external modules

Use a fresh output directory copied from the daily .config. Run only
`olddefconfig prepare`, then explicit object targets. Check `df -h /` before
each invocation; do not compile below 15 GiB free. Example on a Linux
builder (set KOUT and KSRC to absolute paths):

```sh
make -C "$KSRC" O="$KOUT" ARCH=arm64 LLVM=1 olddefconfig prepare
make -C "$KSRC" O="$KOUT" ARCH=arm64 LLVM=1 drivers/soc/apple/apple-ane-fw-test.o
make -C "$KSRC" O="$KOUT" ARCH=arm64 LLVM=1 drivers/media/platform/apple/avd/avd-drv.o
make -C "$KSRC" O="$KOUT" ARCH=arm64 LLVM=1 \
  M="$KSRC/tools/aurora/neo-encoding/video/linux-ave" MO="$KOUT/external-ave" apple-ave.o
make -C "$KSRC" O="$KOUT" ARCH=arm64 LLVM=1 \
  M="$KSRC/tools/aurora/neo-encoding/video/linux-prores" MO="$KOUT/external-prores" apple-prores-test.o
make -C "$KSRC" O="$KOUT" ARCH=arm64 LLVM=1 \
  M="$KSRC/tools/aurora/neo-encoding/video/linux-jpeg" MO="$KOUT/external-jpeg" apple-jpeg-test.o
```

For deployable external `.ko` files, the target builder must supply the
completed build and Module.symvers for the exact running kernel. With that
existing output directory, replace the individual external object target
above with `modules`. `prepare` alone does not establish exported symbol
compatibility or produce deployable modules. This sweep does not run a
full kernel build or module link/install. Do not use the historical
neo-encoding build-kernel/initramfs scripts for this check.

## ANE

Source: Acelogic 5e558f75. Firmware is SHA-pinned
`/lib/firmware/apple/ane/j700-25G83.flat`; prepare it with
`tools/aurora/ane/prepare-linux-firmware.py` from the matching local image.
The regular in-tree FW_TEST module supports bootstrap only. Inference
requires a separately prepared `apple-ane-case-data.h` and matching case
package, compiled by the external builder with ANE_HAVE_CASE. The
stories260K case additionally exposes `/dev/aurora-ane0`. Model compilation
and native reference capture still require macOS.

`build-fw-test.py --allow-unqualified-kernel` now permits an explicitly
unqualified build against this prepared tree and checks the resulting
vermagic against its actual kernel.release. Firmware/model hashes remain
mandatory. Supply `--source-root`, `--kernel-build`, `--base-dtb`, `--output`
and `--case-dir` as documented in `ane/README.md` and `ane/RUNTIME.md`.
Build/load either the in-tree bootstrap modules or the external model
modules, never both copies of a module. The builder needs dtc, fdtget,
fdtput, fdtoverlay, make, modinfo and the exact kernel's Module.symvers.

Load the power module with `enable_engine_power=1` before the firmware
backend, then the generated overlay with `enable_firmware_test=1`. It
holds power before publishing any DART. Bootstrap is a separate write of
`25G83` to `start_bootstrap`; model LOAD can bootstrap automatically.
Use the supplied ane-client --run/--qualify flow to prove computation.
After firmware starts, DMA/power and module references remain pinned until
reset; suspend is vetoed. A failed or shut-down session must not be treated
as safely unloadable. Missing model fixtures mean inference is unavailable,
even if probe and firmware startup succeed.

## AVD

Source: DjDeveloperr 99933382, including bounded job/request handling also
present in the later public work. Firmware:
`/lib/firmware/apple/avd-fw-v5-t0.bin`. Install the matching V4L2-request
userspace stack (the source qualification used its custom decoder and
VA-API integration). A stock FFmpeg build is not guaranteed to support
this stateless driver. Use v4l2-ctl --list-devices and media-ctl -p to
identify AVD separately from the ISP, then decode known H.264 and HEVC
streams, comparing frame hashes with software decode. Extend to VP9/AV1,
10-bit, 4:2:2, seek/stop/restart and malformed input. The source commit
reports 34 decoder cases / 1932 exact frames; rerun on this combined tree.

## AVE, ProRes and JPEG

Use `neo-encoding/video/WORKING_LINUX_ENCODING.md`, `OTHER_CODECS.md`,
`ENCODING_COVERAGE.md` and `../ENCODING_EXPANSION_RESULTS.md` for the latest
source evidence; some older README paragraphs describe earlier milestones.
These are private debugfs diagnostic interfaces, not V4L2 encode drivers.
The Python encode tools require local reference packets/captures and use
FFmpeg for validation/muxing, not as a standard hardware-encode backend.

AVE requires two no-map apple,asc-mem firmware reservations from union
m1n1, iommu-addresses at the 1 TiB alias, and chosen string properties
`aurora,ave-text-crc32` and `aurora,ave-data-crc32`. Run
`video/ave-boot-preflight.py:prepare_ave_firmware()` through the existing
boot proxy before handoff; it is a helper, not a standalone boot command.
It checks 25G76 TEXT SHA and the 9003.78.0 firmware version. Compatibility
with today's 25G83 AVE bytes/layout is not established here. Do not relax
those pins simply to make the candidate boot.

ProRes and JPEG use the existing ps_prores and ps_jpg domains, Linux-owned
DMA and their DARTs; no separate firmware file is requested. Use
encode-prores-linux.py and encode-jpeg-linux.py with prepared packets and
verify the output by independent decode. AVE uses encode-linux-frame.py /
encode-linux-video.py for H.264/HEVC. Begin with one known reference frame
before sequences. Once started, these diagnostics can retain power/DMA
until reset; module unloading and system suspend are not qualified.

## RTC and audio

After confirming the expected RTC device, load rtc-apple-pmu with
experimental=1, test a short awake alarm, cancellation and rearming before
attempting retained s2idle. Never load the old tools/power/neo/overlay RTC
module on this DT: the existing SPMI PMIC already owns slave 14. Use
rtc-alarm-test.py and sleep-test.py with their --help instructions and a
fresh output directory. Historical tools can require the old PMP-v2 sysfs
interface; an admission failure on the clean PMP driver is a failed
preflight, not permission to bypass it. Lid/power wake remains the normal
daily path. The port is untested on hardware.

Speaker User Playback Volume is a new linear 0-100 control, default 100.
It multiplies the existing protected volume; it cannot raise the safety
ceiling. Keep speakersafetyd, the J700 UCM/WirePlumber routing and DSP
profile. Test mute, intermediate gain and restore with the protection
service running. The newest daily audio power-up/teardown fixes are kept.

## Boot and failure qualification

First boot the normal DT with the new accelerator modules withheld; check
serial heartbeat, storage, input, display, GPU, networking and audio.
Load one new driver at a time. Missing firmware must leave the shell and
existing devices usable. Keep the ordinary DT and previous daily kernel
as the recovery selection. Do not infer system stability from a successful
object compile. No combined-tree hardware success is claimed here.
