#!/bin/sh
# Retain checksummed private firmware snapshots in RAM across WPA reconnect.
# No packet/key data is printed. The host retrieves each snapshot afterward.
set -eu
umask 077
label=${1:?capture label required}
case "$label" in *[!A-Za-z0-9_-]*|'') exit 2;; esac
debug=/sys/kernel/debug/apple_centauri/0000:01:00.0
directory=/run/n1-connect-$label
mkdir "$directory"
wpa_cli -p/run/wpa_supplicant -iwlan0 disable_network 0 > "$directory/wpa-control.log"
sleep 1
capture() {
    deadline=$(( $(date +%s) + 25 ))
    index=0
    while [ "$(date +%s)" -lt "$deadline" ] && [ "$index" -lt 128 ]; do
        status=$(cat "$debug/fwlog_status")
        consumer=$(printf '%s\n' "$status" | sed -n 's/.* consumer=\([0-9]*\) .*/\1/p')
        producer=$(printf '%s\n' "$status" | sed -n 's/.* producer=\([0-9]*\) .*/\1/p')
        test -n "$consumer" && test -n "$producer" || return 3
        pending=$((producer - consumer))
        test "$pending" -ge 0 && test "$pending" -le 32 || return 3
        if [ "$pending" -eq 32 ]; then
            snapshot=$directory/snapshot-$index.bin
            cat "$debug/fwlog_buffer" > "$snapshot"
            test "$(wc -c < "$snapshot")" -eq 4194304 || return 5
            printf '%s\n' "$status" > "$directory/snapshot-$index.meta"
            sha256sum "$snapshot" > "$directory/snapshot-$index.sha256"
            sha256sum -c "$directory/snapshot-$index.sha256" > /dev/null
            # Verify the retained copy, as linux-download.py does. Firmware
            # may still modify the mapping even at pending=32; comparing the
            # entire live mapping prevented recycling in probe 36a. Preserve
            # three outstanding DMA entries and reject malformed blocks when
            # decoding instead of claiming the live ring is immutable.
            echo "$((producer - 3))" > "$debug/fwlog_ack"
            index=$((index + 1))
            : > "$directory/ready"
        else
            # An idle device may produce no logs until the first scan. Free
            # space is sufficient readiness; do not wait for an initial fill.
            : > "$directory/ready"
        fi
        busybox usleep 10000
    done
    printf 'FWLOG_SNAPSHOTS=%s\n' "$index"
}
capture > "$directory/capture.log" 2>&1 &
capture_pid=$!
trap 'kill "$capture_pid" 2>/dev/null || true' EXIT HUP INT TERM
for attempt in 1 2 3 4 5; do
    test ! -f "$directory/ready" || break
    kill -0 "$capture_pid" 2>/dev/null || break
    sleep 1
done
test -f "$directory/ready" || exit 4
wpa_cli -p/run/wpa_supplicant -iwlan0 enable_network 0 >> "$directory/wpa-control.log"
wait "$capture_pid"
trap - EXIT HUP INT TERM
# The last partial batch can contain the connection result. Retain it too,
# but do not acknowledge a buffer that is still being filled by firmware.
cat "$debug/fwlog_buffer" > "$directory/snapshot-final.bin"
sha256sum "$directory/snapshot-final.bin" > "$directory/snapshot-final.meta"
cat "$directory/capture.log"
wpa_cli -p/run/wpa_supplicant -iwlan0 status | sed -n '/wpa_state=/p'
head -n 1 "$debug/data_status"
cat "$debug/fwlog_status"
