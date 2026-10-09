#!/bin/bash
# Package standard DRM diagnostics for the otherwise static minimal RAM root.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_tmp=$(mktemp -d "$HOME/j813-drm-tools.XXXXXX")
trap 'rm -rf "$task_tmp"' EXIT
mkdir -p "$task_tmp/lib"
cp /usr/bin/modetest "$task_tmp/"
cp -L /usr/lib/aarch64-linux-gnu/libdrm.so.2 "$task_tmp/lib/"
cp -L /usr/lib/aarch64-linux-gnu/libc.so.6 "$task_tmp/lib/"
cp -L /lib/ld-linux-aarch64.so.1 "$task_tmp/lib/"
(cd "$task_tmp"; sha256sum modetest lib/* > SHA256SUMS)
tar -C "$task_tmp" -czf "$task_root/artifacts/display/drm-tools.tar.gz" .
sha256sum "$task_root/artifacts/display/drm-tools.tar.gz"
