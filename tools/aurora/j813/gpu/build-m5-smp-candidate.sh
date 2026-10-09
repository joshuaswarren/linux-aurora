#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
# Run after populating the J813 AGX3 kernel source/build tree in j813-builder.
# Preserve the graphics baseline and package the M5 CPU fixes separately.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src=/home/mcruz.guest/j813-desktop-kernel-source
task_build=/home/mcruz.guest/j813-smp-diagnostic-build
if [[ ! -e "$task_build" ]]; then
    cp -a --reflink=auto /home/mcruz.guest/j813-gpu-dcp-kernel-build "$task_build"
fi
for task_file in Makefile idle.c idle-apple.S; do
    cp "$task_root/linux/arch/arm64/kernel/$task_file" "$task_src/arch/arm64/kernel/$task_file"
done
cp "$task_root/linux/arch/arm64/include/asm/apple-idle.h" \
    "$task_src/arch/arm64/include/asm/apple-idle.h"
cp "$task_root/linux/arch/arm64/lib/delay.c" "$task_src/arch/arm64/lib/delay.c"
"$task_src/scripts/config" --file "$task_build/.config" \
    --enable HOTPLUG_CPU --enable SOFTLOCKUP_DETECTOR \
    --enable DETECT_HUNG_TASK --enable WQ_WATCHDOG --enable STACKTRACE \
    --enable DEBUG_LIST --enable DEBUG_SPINLOCK
task_script=$(mktemp)
trap 'rm -f "$task_script"' EXIT
sed -e 's|$HOME/j813-gpu-dcp-kernel-build|/home/mcruz.guest/j813-smp-diagnostic-build|' \
    -e 's|artifacts/gpu/native/gpu-dcp-candidate|artifacts/gpu/native/gpu-dcp-smp-diagnostic|' \
    "$task_root/linux/tools/aurora/j813/gpu/build-m5-gpu-dcp-candidate.sh" >"$task_script"
bash "$task_script"
mkdir -p "$task_root/artifacts/cpu"
cp "$task_root/artifacts/gpu/native/gpu-dcp-smp-diagnostic/Image" "$task_root/artifacts/cpu/Image"
cp "$task_root/artifacts/initramfs.cpio.gz" "$task_root/artifacts/cpu/initramfs.cpio.gz"
cp "$task_root/artifacts/t8142-j813.dtb" "$task_root/artifacts/cpu/t8142-j813.dtb"
