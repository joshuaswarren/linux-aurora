#!/bin/sh
# Bring-up probe, not a Linux PMGR driver. Only USB1/USB2; no DMA started.
set -eu
tr '\000' '\n' < /proc/device-tree/compatible | grep -qx apple,j873
case "${1:-}" in
    1) aon=0x308290140; bank=0x303a14800; gate=0x308290160;
       dart0=0x40b020000; dart1=0x40b040000; pipe=0x40a414000 ;;
    2) aon=0x308290180; bank=0x303a14900; gate=0x3082901a0;
       dart0=0x383020000; dart1=0x383040000; pipe=0x382414000 ;;
    *) echo 'Only J873 USB1 and USB2 are supported' >&2; exit 2 ;;
esac
case "${2:-inspect}" in inspect|enable) ;; *) exit 2 ;; esac
read32() { busybox devmem "$1" 32; }
write32() { busybox devmem "$1" 32 "$2"; }
wait32() {
    n=0
    while [ "$n" -lt 100 ]; do
        value=$(read32 "$1")
        [ "$((value & $2))" -eq "$(($3))" ] && return 0
        n=$((n + 1)); sleep 0.01
    done
    echo "Timed out at $1: $value" >&2
    return 1
}
old_aon=$(read32 "$aon"); old_psd=$(read32 "$((bank + 0x80))")
old_gate=$(read32 "$gate")
printf 'USB%s_POWER_BEFORE %s %s %s\n' "$1" "$old_aon" "$old_psd" "$old_gate"
if [ "${2:-inspect}" = enable ]; then
    [ "$((old_aon))" -eq 768 ] && [ "$((old_psd & 7))" -eq 6 ] &&
        [ "$((old_gate))" -eq 768 ]
    phase=0
    rollback() {
        [ "$phase" -lt 3 ] || write32 "$gate" "$old_gate"
        if [ "$phase" -ge 2 ]; then
            write32 "$((bank + 0x90))" "$((old_psd & 7))"
            wait32 "$((bank + 0x80))" 7 "$((old_psd & 7))"
        fi
        [ "$phase" -lt 1 ] || write32 "$aon" "$old_aon"
    }
    trap rollback EXIT
    phase=1; write32 "$aon" "$(((old_aon & 0xfffffcf0) | 15))"
    wait32 "$aon" 0xf0 0xf0
    phase=2; write32 "$((bank + 0x90))" 0
    wait32 "$((bank + 0xbc))" 1 1; wait32 "$((bank + 0x80))" 7 0
    phase=3; write32 "$gate" "$(((old_gate & 0xfffffcf0) | 15))"
    wait32 "$gate" 0xf0 0xf0
    trap - EXIT
    echo "USB${1}_POWER_ENABLED"
fi
[ "$(( $(read32 "$aon") & 0xf0))" -eq 240 ] &&
    [ "$(( $(read32 "$((bank + 0x80))") & 7))" -eq 0 ] &&
    [ "$(( $(read32 "$gate") & 0xf0))" -eq 240 ]
for dart in "$dart0" "$dart1"; do
    for off in 0 4 8 12 0x200 0x208 0x210 0xc00 0x1004 0x1404 0x4000 0x4604; do
        address=$((dart + off)); value=$(read32 "$address")
        printf 'DART 0x%x %s\n' "$address" "$value"
    done
done
for off in 0xc 0x1c 0x20; do
    address=$((pipe + off)); value=$(read32 "$address")
    printf 'PIPE 0x%x %s\n' "$address" "$value"
done
echo J873_USB_HOST_POWER_PROBE_COMPLETE
