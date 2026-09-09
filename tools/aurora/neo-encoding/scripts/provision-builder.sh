#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-neo-builder ]] || { echo 'Run inside neo-builder' >&2; exit 1; }
sudo apt-get update
sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y \
  build-essential git rsync bc bison flex libssl-dev libelf-dev cpio kmod \
  clang lld llvm libclang-dev rustc cargo rust-src bindgen \
  pkg-config meson ninja-build python3-mako python3-yaml python3-ply \
  python3-packaging python3-dev swig device-tree-compiler libgnutls28-dev \
  libdrm-dev libudev-dev libexpat1-dev libzstd-dev zlib1g-dev \
  libvulkan-dev glslang-tools spirv-tools libwayland-dev wayland-protocols \
  libx11-dev libxrandr-dev libxshmfence-dev libxxf86vm-dev libxcb-dri3-dev \
  libxcb-present-dev libxcb-randr0-dev libxcb-shm0-dev libxcb-sync-dev \
  libxext-dev libxfixes-dev libxrender-dev libx11-xcb-dev libxcb-glx0-dev \
  busybox-static openssh-client openssh-server ffmpeg v4l-utils vulkan-tools \
  ripgrep libclc-21-dev libclc-21 llvm-21-dev libclang-cpp21-dev \
  libllvmspirvlib-21-dev libdisplay-info-dev libglvnd-dev \
  cmake spirv-tools-dev spirv-headers libwayland-egl-backend-dev \
  libxcb-dri2-0-dev libxcb-xfixes0-dev libxcb-shape0-dev libunwind-dev

task_shared=/Users/mcruz/Developer/aurora-silicon/neo-linux
mkdir -p "$HOME/neo/build"
python3 "$task_shared/scripts/prepare-sources.py"
printf 'NEO_BUILDER_READY\n'
uname -m
clang --version | head -1
rustc --version
bindgen --version
df -h /
