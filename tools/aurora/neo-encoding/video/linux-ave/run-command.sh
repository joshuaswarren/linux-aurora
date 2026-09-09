#!/bin/sh
# Exact one-write packet publication plus a bounded, reproducible evidence dump.
set -u
packet=$1
mode=${2:-single}
case "$mode" in single) node=ave-command;; pair) node=ave-command-pair;; *) exit 1;; esac
root=/sys/kernel/debug/285100000.video-codec
bytes=$(wc -c < "$packet") || exit 1
if [ "$bytes" -lt 72 ] || [ "$bytes" -gt 131072 ]; then
    echo 'AVE packet size is outside the supported range' >&2
    exit 1
fi
dd if="$packet" of="$root/$node" bs=131072 count=1
result=$?
printf 'AVE_PACKET_RESULT=%s\nAVE_REPLY_BEGIN\n' "$result"
od -An -v -tx1 "$root/$node"
printf 'AVE_REPLY_END\n'
cat "$root/ave-status"
strings "$root/ave-firmware-log" | tail -25
dmesg | tail -8
exit "$result"
