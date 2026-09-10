#!/bin/bash
# Keep the qualified Bluetooth kernel/build and all resident images intact.
set -euo pipefail
export ARCH=arm64 LOCALVERSION=
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j813-builder
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src="$HOME/j813-desktop-kernel-source"
task_build="$HOME/j813-desktop-kernel-build"
task_out="$task_root/artifacts/desktop"
mkdir -p "$task_src" "$task_build" "$task_out"
# git archive preserves case-distinct kernel filenames on the Linux volume.
if [[ ! -f "$task_src/Makefile" ]]; then
    tar -xf "$task_out/kernel-source.tar" -C "$task_src"
fi
cp "$task_root/linux/drivers/bluetooth/btcentauri.c" "$task_src/drivers/bluetooth/btcentauri.c"
sha256sum "$task_src/drivers/bluetooth/btcentauri.c" >"$task_out/bluetooth-source-SHA256"
cp "$task_root/artifacts/bluetooth/kernel.config" "$task_build/.config"
"$task_src/scripts/kconfig/merge_config.sh" -m -O "$task_build" \
    "$task_build/.config" "$task_root/desktop.config"
make -C "$task_src" O="$task_build" olddefconfig
for task_option in CONFIG_FB_DEVICE=y CONFIG_INPUT_EVDEV=y CONFIG_INPUT_UINPUT=y \
    CONFIG_SYSVIPC=y CONFIG_INOTIFY_USER=y CONFIG_CGROUPS=y CONFIG_BT=y; do
    if [[ "$task_option" == CONFIG_BT=y ]]; then
        grep -Eq '^CONFIG_BT=[my]$' "$task_build/.config"
    else
        grep -qx "$task_option" "$task_build/.config"
    fi
done
make -C "$task_src" O="$task_build" W=1 -j10 Image apple/t8142-j813-wifi.dtb modules
make -C "$task_src" O="$task_build" INSTALL_MOD_PATH="$task_out/modules" modules_install
cp "$task_build/arch/arm64/boot/Image" "$task_out/Image"
cp "$task_build/arch/arm64/boot/dts/apple/t8142-j813-wifi.dtb" "$task_out/t8142-j813.dtb"
cp "$task_build/.config" "$task_out/kernel.config"
(cd "$task_out"; sha256sum Image t8142-j813.dtb kernel.config > kernel-SHA256SUMS)
echo J813_DESKTOP_KERNEL_BUILD_COMPLETE
