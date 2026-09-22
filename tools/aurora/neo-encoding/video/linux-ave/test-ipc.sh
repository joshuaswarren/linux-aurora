#!/bin/sh
# Run once after test-native.sh has loaded the cold candidate.
set -u
root=/sys/kernel/debug/285100000.video-codec
grep -qx 'firmware_start_requested=0' "$root/ave-status" || exit 1
printf 'AVE_IPC_TEST_BEGIN\n'
printf 1 > "$root/ave-start-once"
result=$?
cat "$root/ave-status"
dmesg | tail -100
test "$result" = 0 || exit "$result"
grep -qx 'state=ipc-ready' "$root/ave-status" || exit 1
grep -qx 'ipc_ready=1' "$root/ave-status" || exit 1
grep -qx 'channel0_name=IO' "$root/ave-status" || exit 1
grep -qx 'channel1_name=IO_T2H' "$root/ave-status" || exit 1
printf 'AVE_NATIVE_IPC_PASS\n'
