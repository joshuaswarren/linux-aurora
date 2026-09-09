#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-neo-builder ]] || { echo 'Run inside neo-builder' >&2; exit 1; }
export ARCH=arm64 LLVM=1
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=neo-builder
task_shared=/Users/mcruz/Developer/aurora-silicon/neo-linux
task_src="$HOME/neo/sources/linux"
task_build="$HOME/neo/build/linux"
mkdir -p "$task_build" "$task_shared/artifacts/linux"
make -C "$task_src" O="$task_build" defconfig
KCONFIG_CONFIG="$task_build/.config" "$task_src/scripts/kconfig/merge_config.sh" -m \
  "$task_build/.config" "$task_src/arch/arm64/configs/asahi.config" "$task_shared/neo.config"
make -C "$task_src" O="$task_build" olddefconfig
for task_option in CONFIG_ARM64_16K_PAGES=y CONFIG_RUST=y CONFIG_DRM_ASAHI=m \
  CONFIG_SERIAL_APPLE_DOCKCHANNEL=y CONFIG_APPLE_DART=y; do
  grep -qx "$task_option" "$task_build/.config" || { echo "Missing $task_option" >&2; exit 1; }
done
make -C "$task_src" O="$task_build" rustavailable
make -C "$task_src" O="$task_build" -j12 Image apple/t8140-j700.dtb modules
make -C "$task_src" O="$task_build" INSTALL_MOD_PATH="$HOME/neo/build/modules" \
  INSTALL_MOD_STRIP=1 modules_install
cp "$task_build/arch/arm64/boot/Image" "$task_shared/artifacts/linux/"
cp "$task_build/arch/arm64/boot/dts/apple/t8140-j700.dtb" "$task_shared/artifacts/linux/"
cp "$task_build/.config" "$task_shared/artifacts/linux/kernel.config"
cp "$task_build/System.map" "$task_shared/artifacts/linux/"
# Matching full symbols stay on the VM; don't discard them after deployment.
printf 'NEO_KERNEL_BUILD_COMPLETE\n'
make -s -C "$task_src" O="$task_build" kernelrelease
