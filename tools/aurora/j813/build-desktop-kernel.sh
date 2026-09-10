#!/bin/bash
# Keep the qualified Bluetooth kernel/build and all resident images intact.
set -euo pipefail
export ARCH=arm64 LOCALVERSION=
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j813-builder
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src="$HOME/j813-desktop-kernel-source"
task_build="$HOME/j813-desktop-kernel-build"
task_out="$task_root/artifacts/desktop"
task_dtb=t8142-j813-wifi.dtb
task_fragments=("$task_root/desktop.config")
case "${1-}" in
    '') ;;
    input)
        task_dtb=t8142-j813-input.dtb
        task_fragments+=("$task_root/desktop-input.config")
        ;;
    *) echo 'Expected no argument or input' >&2; exit 2 ;;
esac
mkdir -p "$task_src" "$task_build" "$task_out"
# git archive preserves case-distinct kernel filenames on the Linux volume.
if [[ ! -f "$task_src/Makefile" ]]; then
    tar -xf "$task_out/kernel-source.tar" -C "$task_src"
fi
if [[ "$task_dtb" == t8142-j813-input.dtb ]]; then
    cp "$task_root/linux/arch/arm64/boot/dts/apple/t8142-j813-input.dts" \
        "$task_src/arch/arm64/boot/dts/apple/"
    for task_file in drivers/soc/apple/rtkit.c drivers/soc/apple/rtkit-helper.c \
        drivers/hid/dockchannel-hid/dockchannel-hid.c \
        include/linux/soc/apple/rtkit.h; do
        cp "$task_root/linux/$task_file" "$task_src/$task_file"
    done
fi
cp "$task_root/linux/drivers/bluetooth/btcentauri.c" "$task_src/drivers/bluetooth/btcentauri.c"
sha256sum "$task_src/drivers/bluetooth/btcentauri.c" >"$task_out/bluetooth-source-SHA256"
cp "$task_root/artifacts/bluetooth/kernel.config" "$task_build/.config"
"$task_src/scripts/kconfig/merge_config.sh" -m -O "$task_build" \
    "$task_build/.config" "${task_fragments[@]}"
make -C "$task_src" O="$task_build" olddefconfig
for task_option in CONFIG_FB_DEVICE=y CONFIG_INPUT_EVDEV=y CONFIG_INPUT_UINPUT=y \
    CONFIG_SYSVIPC=y CONFIG_INOTIFY_USER=y CONFIG_CGROUPS=y CONFIG_BT=y; do
    if [[ "$task_option" == CONFIG_BT=y ]]; then
        grep -Eq '^CONFIG_BT=[my]$' "$task_build/.config"
    else
        grep -qx "$task_option" "$task_build/.config"
    fi
done
if [[ "$task_dtb" == t8142-j813-input.dtb ]]; then
    for task_option in CONFIG_PM=y CONFIG_APPLE_MAILBOX=m CONFIG_APPLE_RTKIT=m \
        CONFIG_APPLE_RTKIT_HELPER=m CONFIG_APPLE_DOCKCHANNEL=m CONFIG_HID_DOCKCHANNEL=m \
        CONFIG_HID_APPLE=m CONFIG_HID_MAGICMOUSE=m CONFIG_LEDS_CLASS=m; do
        grep -qx "$task_option" "$task_build/.config"
    done
fi
make -C "$task_src" O="$task_build" W=1 -j10 Image "apple/$task_dtb" modules
make -C "$task_src" O="$task_build" INSTALL_MOD_PATH="$task_out/modules" modules_install
cp "$task_build/arch/arm64/boot/Image" "$task_out/Image"
cp "$task_build/arch/arm64/boot/dts/apple/$task_dtb" "$task_out/t8142-j813.dtb"
cp "$task_build/.config" "$task_out/kernel.config"
(cd "$task_out"; sha256sum Image t8142-j813.dtb kernel.config > kernel-SHA256SUMS)
echo J813_DESKTOP_KERNEL_BUILD_COMPLETE
