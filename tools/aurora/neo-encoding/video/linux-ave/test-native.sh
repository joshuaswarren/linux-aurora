#!/bin/sh
# Run in the Neo RAM shell after transferring the matching module to /tmp.
set -eu
module=${1:-/tmp/apple-ave.ko}
ipc_mib=${2:-20}
explicit_pool=${2:+yes}
case "$ipc_mib" in 20|32) ;; *) echo 'IPC pool must be 20 or 32 MiB' >&2; exit 2 ;; esac
ipc_bytes=$((ipc_mib * 1024 * 1024))
ipc_pages=$((ipc_bytes / 16384))
test "$(uname -r)" = 7.1.6-aurora-neo
test ! -d /sys/module/apple_ave

check_status() {
    set -- /sys/kernel/debug/*/ave-status
    test "$#" = 1
    test -f "$1"
    status=$(cat "$1")
    printf '%s\n' "$status"
    for field in state=firmware-mapped firmware_start_requested=0 hardware_dma_tested=0 \
        encoder_registered=0 page_size=16384 powered_domains=5 ipc_bytes="$ipc_bytes" \
        ipc_verified_pages="$ipc_pages" queue_bytes=65536 queue_verified_pages=4 \
        fw0_bytes=1359872 fw0_verified_pages=83 fw1_bytes=1245184 \
        fw1_verified_pages=76 register_snapshot=1; do
        printf '%s\n' "$status" | grep -qx "$field"
    done
}

load_module() {
    if test "$explicit_pool" = yes; then
        insmod "$module" snapshot=1 ipc_mib="$ipc_mib"
    else
        insmod "$module" snapshot=1
    fi
}

load_module
check_status
rmmod apple_ave
test ! -d /sys/module/apple_ave
set -- /sys/kernel/debug/*/ave-status
test ! -e "$1"
printf 'AVE_NATIVE_RELEASE_PASS\n'
load_module
check_status
printf 'AVE_NATIVE_FIRMWARE_PASS dma_pages=%s firmware_pages=159 lifecycle=load-unload-reload firmware_start=0\n' "$((ipc_pages + 4))"
