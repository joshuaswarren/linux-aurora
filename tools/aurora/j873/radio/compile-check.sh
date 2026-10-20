#!/bin/sh
# compile-check.sh [BUILD_DIR]: compile (no link) the drivers the radio configuration enables, after build-config.sh has resolved it.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../../../.." && pwd)
build=${1:-/dev/shm/j873-radio-kbuild}
cc=${CROSS_COMPILE:-aarch64-linux-gnu-}
make -s -C "$src" ARCH=arm64 CROSS_COMPILE="$cc" O="$build" -j"$(nproc)" modules_prepare
make -s -C "$src" ARCH=arm64 CROSS_COMPILE="$cc" O="$build" -j"$(nproc)" \
    drivers/gpio/gpio-macsmc.o \
    drivers/pci/controller/pcie-apple.o \
    drivers/net/wireless/apple/centauri/ \
    drivers/bluetooth/btcentauri.o \
    drivers/net/ethernet/broadcom/tg3.o \
    drivers/net/ethernet/aquantia/atlantic/ \
    drivers/net/ethernet/realtek/r8169_main.o
echo RADIO_COMPILE_CHECK_PASS
