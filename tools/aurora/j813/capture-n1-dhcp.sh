#!/bin/sh
# Run as a child of the RAM console. Private capture bytes never reach stdout.
set -u
label=${1:?capture label required}
case "$label" in
    ''|*[!A-Za-z0-9_-]*) echo "Invalid capture label"; exit 2 ;;
esac
umask 077
prefix=/run/n1-dhcp-$label
for suffix in pcap capture-log client-log; do
    if [ -e "$prefix.$suffix" ]; then
        echo "Capture label already exists"
        exit 2
    fi
done
timeout -s INT 20 /bin/tcpdump -i wlan0 -p -n -s 0 -U -Z root \
    -w "$prefix.pcap" 'udp port 67 or udp port 68' \
    > "$prefix.capture-log" 2>&1 &
capture_pid=$!
cleanup() {
    kill "$capture_pid" 2>/dev/null || :
    wait "$capture_pid" 2>/dev/null || :
}
trap cleanup EXIT HUP INT TERM
ready=0
for attempt in 1 2 3 4 5; do
    if grep -q 'listening on wlan0' "$prefix.capture-log"; then
        ready=1
        break
    fi
    if ! kill -0 "$capture_pid" 2>/dev/null; then
        break
    fi
    sleep 1
done
if [ "$ready" != 1 ]; then
    cat "$prefix.capture-log"
    exit 1
fi
echo N1_DHCP_CAPTURE_READY
timeout 15 udhcpc -B -q -f -n -i wlan0 -s /bin/n1-udhcpc \
    -p /run/n1-udhcpc.pid -t 3 -T 3 > "$prefix.client-log" 2>&1
client_rc=$?
wait "$capture_pid" || :
trap - EXIT HUP INT TERM
echo N1_DHCP_CAPTURE_COMPLETE
grep -E 'packets (captured|received by filter|dropped by kernel)' "$prefix.capture-log" || :
printf 'DHCP_RESULT rc=%s\n' "$client_rc"
head -n 1 /sys/kernel/debug/apple_centauri/0000:01:00.0/data_status
exit "$client_rc"
