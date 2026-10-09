#!/bin/bash
# Integrate the application GPU kernel with DCP in a separate RAM payload.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
export ARCH=arm64 LLVM=1 LOCALVERSION=
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j813-builder
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src="$HOME/j813-desktop-kernel-source"
task_build="$HOME/j813-gpu-dcp-kernel-build"
task_out="$task_root/artifacts/gpu/native/gpu-dcp-candidate"
task_release=7.1.6-aurora-j813-gpu-v9
if [[ ! -e "$task_build" ]]; then
    cp -a --reflink=auto "$HOME/j813-gpu-application-compute-build" "$task_build"
fi
for task_file in m5_application_compute.rs m5_completion.rs m5_drm.rs m5_queue_probe.rs \
    m5_graphics_job.rs m5_graphics_queues.rs m5_resources.rs m5_live_boot.rs m5_render_test.rs g17_uapi.rs; do
    cmp -s "$task_root/linux/drivers/gpu/drm/asahi/$task_file" \
        "$task_src/drivers/gpu/drm/asahi/$task_file" ||
        cp "$task_root/linux/drivers/gpu/drm/asahi/$task_file" \
            "$task_src/drivers/gpu/drm/asahi/$task_file"
done
for task_file in "$task_root"/linux/drivers/gpu/drm/apple/*.[ch]; do
    task_destination="$task_src/drivers/gpu/drm/apple/$(basename "$task_file")"
    cmp -s "$task_file" "$task_destination" || cp "$task_file" "$task_destination"
done
for task_file in dockchannel-hid.c dockchannel-hid-protocol.h; do
    task_input_dir=drivers/hid/dockchannel-hid
    cmp -s "$task_root/linux/$task_input_dir/$task_file" "$task_src/$task_input_dir/$task_file" ||
        cp "$task_root/linux/$task_input_dir/$task_file" "$task_src/$task_input_dir/$task_file"
done
"$task_src/scripts/config" --file "$task_build/.config" \
    --module DRM_APPLE --enable BACKLIGHT_CLASS_DEVICE --enable DRM_APPLE_DEBUG \
    --enable APPLE_MAILBOX --enable PWM --enable PWM_APPLE \
    --enable LEDS_CLASS --enable LEDS_PWM --enable GPIOLIB
make -C "$task_src" O="$task_build" olddefconfig
for task_option in CONFIG_DRM_APPLE=m CONFIG_DRM_ASAHI=m CONFIG_APPLE_RTKIT=y \
    CONFIG_ARM64_16K_PAGES=y CONFIG_APPLE_MAILBOX=y CONFIG_PWM_APPLE=y \
    CONFIG_LEDS_PWM=y CONFIG_GPIOLIB=y; do
    grep -qx "$task_option" "$task_build/.config"
done
make -C "$task_src" O="$task_build" -j8 Image modules
[[ $(cat "$task_build/include/config/kernel.release") == "$task_release" ]]
mkdir -p "$task_out"
cp "$task_build/arch/arm64/boot/Image" "$task_out/"
cp "$task_build/drivers/gpu/drm/asahi/asahi.ko" "$task_out/"
cp "$task_build/drivers/gpu/drm/apple/appledrm.ko" "$task_out/"
cp "$task_build/.config" "$task_out/kernel.config"
clang -E -P -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
    -I "$task_root/linux/scripts/dtc/include-prefixes" \
    -I "$task_root/linux/arch/arm64/boot/dts/apple" \
    "$task_root/linux/tools/aurora/j813/gpu/t8142-j813-gpu-display.dts" \
    -o "$task_out/t8142-j813-gpu-display.pp.dts"
"$task_build/scripts/dtc/dtc" -I dts -O dtb \
    -o "$task_out/t8142-j813.dtb" "$task_out/t8142-j813-gpu-display.pp.dts"
task_ramfs=$(mktemp -d "$HOME/j813-gpu-dcp-initramfs.XXXXXX")
trap 'rm -rf "$task_ramfs"' EXIT
(cd "$task_ramfs"; gzip -dc "$task_root/artifacts/gpu/native/rtkit-probe/initramfs.cpio.gz" | cpio -idmu --quiet)
make -C "$task_src" O="$task_build" INSTALL_MOD_PATH="$task_ramfs" modules_install
depmod -b "$task_ramfs" "$task_release"
for task_old in "$task_ramfs"/opt/aurora-bluetooth/modules/*.ko; do
    [[ -f "$task_old" ]] || continue
    task_name=$(basename "$task_old")
    task_new=$(find "$task_ramfs/lib/modules/$task_release" -name "$task_name" -type f -print -quit)
    [[ -n "$task_new" ]]
    cp "$task_new" "$task_old"
done
mkdir -p "$task_ramfs/etc/modprobe.d" "$task_ramfs/opt/j813"
printf 'blacklist appledrm\nblacklist asahi\n' >"$task_ramfs/etc/modprobe.d/m5-gpu-dcp.conf"
install -m644 "$task_root/desktop-dcp.conf" "$task_ramfs/opt/j813/desktop-dcp.conf"
install -m644 "$task_root/linux/tools/aurora/j813/input-ready.py" "$task_ramfs/bin/j813-input-ready.py"
(cd "$task_ramfs"; find . -print0 | cpio --null -o --format=newc --owner=0:0 2>/dev/null | gzip -9) \
    >"$task_out/initramfs.cpio.gz"
(cd "$task_out"; sha256sum Image asahi.ko appledrm.ko kernel.config t8142-j813.dtb initramfs.cpio.gz > SHA256SUMS)
echo M5_GPU_DCP_RAM_CANDIDATE_BUILT
