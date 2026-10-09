#!/bin/sh
set -eu
test "$(hostname)" = lima-j813-builder
task_src="$HOME/j873-linux-source"
task_build="$HOME/j873-desktop-build"
task_out="$HOME/j873-desktop-artifacts"
task_tools="$task_src/tools/aurora/j873"
export ARCH=arm64 LLVM=1 KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j873-builder
mkdir -p "$task_build" "$task_out"
sed '/^CONFIG_LOCALVERSION=/d' "$task_tools/thermal/j873.config" > "$task_build/requested.config"
cat "$task_tools/desktop/desktop.config" >> "$task_build/requested.config"
make -C "$task_src" O="$task_build" \
    KCONFIG_ALLCONFIG="$task_build/requested.config" allnoconfig
for option in CONFIG_FB_DEVICE=y CONFIG_UNIX=y CONFIG_SYSVIPC=y \
    CONFIG_PROC_SYSCTL=y CONFIG_USER_NS=y CONFIG_PID_NS=y CONFIG_NET_NS=y \
    CONFIG_SECCOMP_FILTER=y \
    CONFIG_MFD_MACSMC=m CONFIG_SENSORS_MACSMC_HWMON=m \
    CONFIG_APPLE_MAILBOX=y CONFIG_SERIAL_CORE=y CONFIG_ARM_APPLE_SOC_CPUFREQ=m \
    CONFIG_CPU_FREQ_THERMAL=y CONFIG_CPU_FREQ_DEFAULT_GOV_POWERSAVE=y \
    CONFIG_INPUT_EVDEV=y CONFIG_HID_MAGICMOUSE=y; do
    grep -qx "$option" "$task_build/.config"
done
make -C "$task_src" O="$task_build" -j4 Image modules
cp "$task_build/arch/arm64/boot/Image" "$task_out/Image"
cp "$task_build/.config" "$task_out/kernel.config"
clang -E -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
    -I "$task_src/include" -I "$task_src/arch/arm64/boot/dts/apple" \
    "$task_src/arch/arm64/boot/dts/apple/t8152-j873.dts" \
    -o "$task_out/t8152-j873.pp.dts"
"$task_build/scripts/dtc/dtc" -I dts -O dtb \
    -o "$task_out/t8152-j873.dtb" "$task_out/t8152-j873.pp.dts"
echo J873_DESKTOP_KERNEL_BUILT
