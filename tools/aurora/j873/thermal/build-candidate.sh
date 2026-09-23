#!/bin/sh
set -eu
test "$(hostname)" = lima-j813-builder
task_src="$HOME/j873-linux-source"
task_build="$HOME/j873-linux-build"
task_out="$HOME/j873-linux-artifacts"
task_tools="$task_src/tools/aurora/j873/thermal"
export ARCH=arm64 LLVM=1 KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j873-builder
mkdir -p "$task_build" "$task_out"
make -C "$task_src" O="$task_build" \
    KCONFIG_ALLCONFIG="$task_tools/j873.config" allnoconfig
for option in CONFIG_MFD_MACSMC=m CONFIG_SENSORS_MACSMC_HWMON=m \
    CONFIG_APPLE_MAILBOX=y CONFIG_APPLE_RTKIT=y CONFIG_SERIAL_CORE=y \
    CONFIG_ARM_APPLE_SOC_CPUFREQ=m CONFIG_CPU_FREQ_THERMAL=y \
    CONFIG_CPU_FREQ_DEFAULT_GOV_POWERSAVE=y CONFIG_THERMAL_OF=y; do
    grep -qx "$option" "$task_build/.config"
done
make -C "$task_src" O="$task_build" -j2 Image modules
cp "$task_build/arch/arm64/boot/Image" "$task_out/Image"
cp "$task_build/.config" "$task_out/kernel.config"
clang -E -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
    -I "$task_src/include" -I "$task_src/arch/arm64/boot/dts/apple" \
    "$task_src/arch/arm64/boot/dts/apple/t8152-j873.dts" \
    -o "$task_out/t8152-j873.pp.dts"
"$task_build/scripts/dtc/dtc" -I dts -O dtb \
    -o "$task_out/t8152-j873.dtb" "$task_out/t8152-j873.pp.dts"
task_ramfs=$(mktemp -d "$HOME/j873-thermal-initramfs.XXXXXX")
trap 'rm -rf "$task_ramfs"' EXIT
mkdir -p "$task_ramfs/bin" "$task_ramfs/sbin" "$task_ramfs/dev" \
    "$task_ramfs/proc" "$task_ramfs/sys" "$task_ramfs/tmp" \
    "$task_ramfs/run" "$task_ramfs/opt/j873"
install -m755 /usr/bin/busybox "$task_ramfs/bin/busybox"
for app in sh mount cat echo sleep ls mkdir modprobe dmesg uname awk sed head \
    tail grep cut sort tr readlink wc find printf poweroff reboot stat date \
    setsid cttyhack timeout; do
    ln -s busybox "$task_ramfs/bin/$app"
done
make -C "$task_src" O="$task_build" INSTALL_MOD_PATH="$task_ramfs" modules_install
install -m755 "$task_tools/init" "$task_ramfs/init"
for script in thermal-status.sh qualify-cooling.sh; do
    install -m755 "$task_tools/$script" "$task_ramfs/opt/j873/$script"
done
(cd "$task_ramfs" && find . -print0 | cpio --null -o -H newc) |
    gzip -1 > "$task_out/initramfs.cpio.gz"
(cd "$task_out" && sha256sum Image kernel.config t8152-j873.dtb initramfs.cpio.gz > SHA256SUMS)
printf '%s\n' 'J873_THERMAL_BUILD_COMPLETE'
