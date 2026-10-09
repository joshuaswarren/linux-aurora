#!/bin/bash
# Small RAM root for DCP iteration. Full Wi-Fi/input root remains untouched.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_build="$HOME/j813-gpu-kernel-build"
task_full="$task_root/artifacts/display"
task_out="$task_full/probe"
task_release=7.1.6-aurora-j813-dcp-v1
[[ $(cat "$task_build/include/config/kernel.release") == "$task_release" ]]
mkdir -p "$task_out"
cp "$task_full/Image" "$task_full/kernel.config" "$task_out/"
clang -E -P -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
    -I "$task_root/linux/scripts/dtc/include-prefixes" \
    -I "$task_root/linux/arch/arm64/boot/dts/apple" \
    "$task_root/linux/arch/arm64/boot/dts/apple/t8142-j813-display.dts" \
    -o "$task_out/t8142-j813-display.pp.dts"
"$task_build/scripts/dtc/dtc" -I dts -O dtb \
    -o "$task_out/t8142-j813.dtb" "$task_out/t8142-j813-display.pp.dts"
umask 077
task_ramfs=$(mktemp -d "$HOME/j813-dcp-probe.XXXXXX")
trap 'rm -rf "$task_ramfs"' EXIT
(cd "$task_ramfs"; gzip -dc "$task_root/artifacts/base-linux/initramfs.cpio.gz" | cpio -idmu --quiet)
task_modroot="$task_ramfs/lib/modules/$task_release"
mkdir -p "$task_modroot"
for task_module in kernel/drivers/gpu/drm/drm_dma_helper.ko kernel/drivers/gpu/drm/apple/appledrm.ko; do
    install -Dm644 "$task_full/modules/lib/modules/$task_release/$task_module" "$task_modroot/$task_module"
done
for task_file in modules.builtin modules.builtin.modinfo; do
    cp "$task_full/modules/lib/modules/$task_release/$task_file" "$task_modroot/"
done
depmod -b "$task_ramfs" "$task_release"
mkdir -p "$task_ramfs/etc/modprobe.d"
printf 'blacklist appledrm\n' >"$task_ramfs/etc/modprobe.d/m5-display-probe.conf"
(cd "$task_ramfs"; find . -print0 | cpio --null -o --format=newc --owner=0:0 2>/dev/null | gzip -9) \
    >"$task_out/initramfs.cpio.gz"
(cd "$task_out"; sha256sum Image t8142-j813.dtb kernel.config initramfs.cpio.gz > boot-SHA256SUMS)
echo M5_DCP_MINIMAL_PROBE_PAYLOAD_BUILT
