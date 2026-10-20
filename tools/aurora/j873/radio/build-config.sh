#!/bin/sh
# build-config.sh [BUILD_DIR]: resolve the J873 radio kernel configuration the way tools/aurora/j873/desktop/build-kernel.sh does,
# with radio.config appended (PCIe, Apple N1 Wi-Fi and Bluetooth, the Ethernet candidates). Prints the radio-relevant symbols.
# Needs only a cross gcc (CROSS_COMPILE, default aarch64-linux-gnu-); it does not build the kernel.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../../../.." && pwd)
tools=$src/tools/aurora/j873
build=${1:-/dev/shm/j873-radio-kbuild}
mkdir -p "$build"
sed '/^CONFIG_LOCALVERSION=/d' "$tools/thermal/j873.config" > "$build/requested.config"
cat "$tools/desktop/desktop.config" "$tools/radio/radio.config" >> "$build/requested.config"
echo 'CONFIG_LOCALVERSION="-aurora-j873-radio"' >> "$build/requested.config"
make -s -C "$src" ARCH=arm64 CROSS_COMPILE="${CROSS_COMPILE:-aarch64-linux-gnu-}" O="$build" \
    KCONFIG_ALLCONFIG="$build/requested.config" allnoconfig
for option in $(sed -n 's/^\(CONFIG_[A-Z0-9_]*\)=.*/\1/p' "$tools/radio/radio.config"); do
    want=$(grep "^$option=" "$tools/radio/radio.config")
    if grep -qx "$want" "$build/.config"; then
        echo "ok      $want"
    else
        echo "MISSING $want (resolved: $(grep -E "^(# )?${option}[= ]" "$build/.config" | head -1))"
    fi
done
