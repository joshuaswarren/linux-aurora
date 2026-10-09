#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
# Build a separate thermal image from the established J813 SMP kernel tree.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
export ARCH=arm64 LLVM=1 LOCALVERSION=
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j813-builder
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src="$HOME/j813-desktop-kernel-source"
task_build="$HOME/j813-thermal-build"
task_tools="$task_root/linux/tools/aurora/j813/thermal"
case "${1:-thermal}" in
    thermal)
        task_out="$task_root/artifacts/thermal"
        task_dts=t8142-j813-thermal.dts
        task_base="$task_root/artifacts/cpu/initramfs.cpio.gz" ;;
    gpu)
        task_out="$task_root/artifacts/gpu/native/gpu-thermal-candidate"
        task_dts=t8142-j813-gpu-thermal.dts
        task_base="$task_root/artifacts/gpu/native/rtkit-probe/initramfs.cpio.gz" ;;
    *) echo 'Usage: build-candidate.sh [thermal|gpu]' >&2; exit 1 ;;
esac
if [[ ! -e "$task_build" ]]; then
    cp -a --reflink=auto "$HOME/j813-smp-diagnostic-build" "$task_build"
fi
cp "$task_root/linux/drivers/cpufreq/apple-soc-cpufreq.c" \
    "$task_src/drivers/cpufreq/apple-soc-cpufreq.c"
KCONFIG_CONFIG="$task_build/.config" \
    "$task_src/scripts/kconfig/merge_config.sh" -m \
    "$task_build/.config" "$task_tools/../thermal.config"
make -C "$task_src" O="$task_build" olddefconfig
for option in CONFIG_CPU_FREQ_DEFAULT_GOV_USERSPACE=y CONFIG_THERMAL=y \
    CONFIG_CPU_FREQ_THERMAL=y CONFIG_SENSORS_MACSMC_HWMON=m CONFIG_MFD_MACSMC=m \
    CONFIG_ARM_APPLE_SOC_CPUFREQ=m CONFIG_CPU_IDLE=y; do
    grep -qx "$option" "$task_build/.config"
done
make -C "$task_src" O="$task_build" -j2 Image modules
task_release=$(cat "$task_build/include/config/kernel.release")
mkdir -p "$task_out"
cp "$task_build/arch/arm64/boot/Image" "$task_out/"
cp "$task_build/.config" "$task_out/kernel.config"
clang -E -P -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
    -I "$task_root/linux/scripts/dtc/include-prefixes" \
    -I "$task_root/linux/arch/arm64/boot/dts/apple" \
    "$task_tools/$task_dts" -o "$task_out/t8142-j813-thermal.pp.dts"
"$task_build/scripts/dtc/dtc" -I dts -O dtb -o "$task_out/t8142-j813.dtb" \
    "$task_out/t8142-j813-thermal.pp.dts"
task_ramfs=$(mktemp -d "$HOME/j813-thermal-initramfs.XXXXXX")
trap 'rm -rf "$task_ramfs"' EXIT
(cd "$task_ramfs"; gzip -dc "$task_base" | cpio -idmu --quiet)
make -C "$task_src" O="$task_build" INSTALL_MOD_PATH="$task_ramfs" modules_install
depmod -b "$task_ramfs" "$task_release"
mkdir -p "$task_ramfs/opt/j813"
for script in enable-cooling.sh thermal-status.sh qualify-cooling.sh; do
    install -m755 "$task_tools/$script" "$task_ramfs/opt/j813/$script"
done
install -m755 "$task_tools/init" "$task_ramfs/init"
(cd "$task_ramfs"; find . -print0 | cpio --null -o --format=newc --owner=0:0 2>/dev/null | gzip -9) \
    > "$task_out/initramfs.cpio.gz"
(cd "$task_out"; sha256sum Image kernel.config t8142-j813.dtb initramfs.cpio.gz > SHA256SUMS)
echo M5_THERMAL_CANDIDATE_BUILD_COMPLETE
