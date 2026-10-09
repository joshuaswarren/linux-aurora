#!/bin/sh
# Restore the small Vulkan test root into RAM after a candidate kernel boot.
set -eu
[ "$#" = 3 ] || { echo 'Usage: prepare-m5-vulkan-test-root.sh SERVER ARCHIVE SHA256'; exit 2; }
server=${1%/}
archive=$2
digest=$3
case "$archive" in ''|*[!a-zA-Z0-9._-]*) exit 2;; esac
case "$digest" in *[!a-f0-9]*) exit 2;; esac
[ "${#digest}" = 64 ]
grep -aq apple,j813 /proc/device-tree/compatible
[ -c /dev/dri/renderD128 ]
if grep -q ' /gpu-test ' /proc/mounts; then
    echo 'Existing GPU test root: preserve it.'
    exit 1
fi
start=$(date +%s)
mkdir -p /gpu-test
mount -t tmpfs -o size=256M tmpfs /gpu-test
wget -q "$server/$archive" -O /gpu-test/.runtime.tar.gz
printf '%s  %s\n' "$digest" /gpu-test/.runtime.tar.gz | sha256sum -c -
tar -xzf /gpu-test/.runtime.tar.gz -C /gpu-test
rm /gpu-test/.runtime.tar.gz
for directory in dev proc sys run; do
    mount --rbind "/$directory" "/gpu-test/$directory"
    mount --make-rslave "/gpu-test/$directory"
done
mkdir -p /run/user/0
chmod 700 /run/user/0
for binary in /opt/m5-mesa/bin/m5-vulkan-readback-render \
              /opt/m5-mesa/lib/libvulkan_asahi.so /opt/m5-mesa/lib/libvulkan.so.1; do
    chroot /gpu-test /lib/ld-linux-aarch64.so.1 --library-path /opt/m5-mesa/lib --list "$binary"
done
env ASAHI_M5_BRINGUP=1 MESA_SHADER_CACHE_DISABLE=true \
    LD_LIBRARY_PATH=/opt/m5-mesa/lib VK_DRIVER_FILES=/opt/m5-mesa/asahi_icd.json \
    XDG_RUNTIME_DIR=/run/user/0 AGX_RENDER_RECORD_ONLY=1 AGX_RENDER_READBACK=1 \
    chroot /gpu-test /opt/m5-mesa/bin/m5-vulkan-readback-render
echo M5_FAST_RUNTIME_READY_SECONDS=$(( $(date +%s) - start ))
