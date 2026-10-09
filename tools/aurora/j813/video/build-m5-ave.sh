#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]] || { echo 'Run inside j813-builder' >&2; exit 1; }
export ARCH=arm64
unset LLVM LLVM_IAS
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j813-builder
task_shared=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src=/home/mcruz.linux/j813-desktop-kernel-source
task_kernel=/home/mcruz.linux/j813-desktop-kernel-build
task_build=/home/mcruz.linux/j813-ave-module
task_bundle="$task_shared/artifacts/video/native"
mkdir -p "$task_build" "$task_bundle"
cp "$task_shared/video/linux-ave/apple-ave.c" "$task_shared/video/linux-ave/apple-ave-ipc.h" \
   "$task_shared/video/linux-ave/Makefile" "$task_build/"
make -C "$task_src" O="$task_kernel" M="$task_build" CC=gcc -j8 W=1 modules
cp "$task_build/apple-ave.ko" "$task_bundle/apple-ave-unstripped.ko"
llvm-strip --strip-debug -o "$task_bundle/apple-ave.ko" "$task_build/apple-ave.ko"
printf '#include "t8142-j813-input.dts"\n#include "%s/video/linux-ave/t8142-ave.dtsi"\n' \
  "$task_shared" > "$task_build/t8142-j813-ave.dts"
clang -E -P -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
  -I "$task_shared/linux/scripts/dtc/include-prefixes" \
  -I "$task_shared/linux/arch/arm64/boot/dts/apple" \
  "$task_build/t8142-j813-ave.dts" -o "$task_build/t8142-j813-ave.pp.dts"
"$task_kernel/scripts/dtc/dtc" -I dts -O dtb \
  -o "$task_bundle/t8142-j813.dtb" "$task_build/t8142-j813-ave.pp.dts"
cp "$task_shared/artifacts/desktop/Image" "$task_shared/artifacts/desktop/initramfs.cpio.gz" "$task_bundle/"
printf 'M5_AVE_MODULE_AND_DTB_BUILT\n'
