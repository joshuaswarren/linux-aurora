#!/bin/bash
# Full native cross-build using the repository's pinned module toolchain.
set -euo pipefail
SRC=$(cd "$(dirname "$0")/../.." && pwd)
BUILD=${AURORA_LINUX_BUILD:-$SRC/.port-build}
XR=/opt/xtools/debian-rust-1.85
XG=/opt/xtools/debian-gcc14
export PATH=$XG/usr/bin:$PATH
export LD_LIBRARY_PATH=$XR/usr/lib/x86_64-linux-gnu:$XG/usr/lib/x86_64-linux-gnu
export LIBCLANG_PATH=$XR/usr/lib/x86_64-linux-gnu
KMAKE=(make -C "$SRC" O="$BUILD" -j"${JOBS:-8}"
  ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
  KERNELRELEASE=7.1.6-neo-unified HOSTCC=gcc
  RUSTC="$XR/usr/bin/rustc" RUSTDOC="$XR/usr/bin/rustdoc" BINDGEN="$XR/usr/bin/bindgen")
test -f "$BUILD/.config"
"${KMAKE[@]}" olddefconfig
for setting in CONFIG_RUST=y CONFIG_DRM_ASAHI=m CONFIG_DRM_APPLE=m \
  CONFIG_PAGE_SIZE_16KB=y CONFIG_ARCH_APPLE=y CONFIG_MODULES=y \
  CONFIG_BLK_DEV_NVME=y CONFIG_NVME_APPLE=y CONFIG_APPLE_DART=y \
  CONFIG_APPLE_RTKIT=y CONFIG_APPLE_SART=y CONFIG_EXT4_FS=y; do
  grep -qx "$setting" "$BUILD/.config" || { echo "Missing required $setting"; exit 1; }
done
"${KMAKE[@]}" Image modules apple/t8140-j700.dtb
sha256sum "$BUILD/arch/arm64/boot/Image" \
  "$BUILD/arch/arm64/boot/dts/apple/t8140-j700.dtb" \
  "$BUILD/drivers/gpu/drm/asahi/asahi.ko" \
  "$BUILD/drivers/gpu/drm/apple/appledrm.ko"
