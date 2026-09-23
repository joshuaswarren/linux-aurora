#!/bin/sh
# Bounded USB2 host PHY probe. Does not enable DART or start xHCI DMA.
set -eu
tr '\000' '\n' < /proc/device-tree/compatible | grep -qx apple,j873
case "${1:-}" in
    1) aon=0x308290140; bank=0x303a14800; gate=0x308290160;
       dart0=0x40b020000; dart1=0x40b040000; pipe=0x40a414000;
       phy=0x40a418000; evt=0x40a448000; dwc=0x40a430000 ;;
    2) aon=0x308290180; bank=0x303a14900; gate=0x3082901a0;
       dart0=0x383020000; dart1=0x383040000; pipe=0x382414000;
       phy=0x382418000; evt=0x382448000; dwc=0x382430000 ;;
    *) exit 2 ;;
esac
r() { busybox devmem "$1" 32; }
w() { busybox devmem "$1" 32 "$2"; }
mask() { old=$(r "$1"); w "$1" "$(((old & ~$2) | $3))"; }
[ "$(( $(r "$aon") ))" -eq 1023 ]
[ "$(( $(r "$((bank + 0x80))") & 7 ))" -eq 0 ]
[ "$(( $(r "$gate") ))" -eq 1023 ]
[ "$(( $(r "$((dart0 + 0xc00))") ))" -eq 0 ]
[ "$(( $(r "$((dart1 + 0xc00))") ))" -eq 0 ]
[ "$(( $(r "$((pipe + 0xc))") ))" -eq 66 ]
[ "$(( $(r "$((pipe + 0x1c))") ))" -eq 0 ]
[ "$(( $(r "$phy") ))" -eq 4 ]
[ "$(( $(r "$((phy + 4))") ))" -eq 11 ]
# USB2-only dummy PIPE path, already selected by firmware. Keep USB3 mux.
mask "$((dwc + 0xc200))" 0x40 0
mask "$((dwc + 0xc2c0))" 0x20000 0
w "$((pipe + 0x1c))" 1
id=$(r "$((dwc + 0xc120))")
[ "$((id))" -eq "$((0x33313130))" ]
[ "$(( $(r "$((dwc + 0xc704))") & 0x80000000))" -eq 0 ]
w "$((pipe + 0x1c))" 0
# Matching 26A428 AppleT8152TypeCPhy::eusb2phy_init(false), with exact
# USB1/USB2 ADT DFLT and HOST tunables. The DEV-only VBUS force bits are omitted.
mask "$((phy + 8))" 0x7003 3
mask "$((phy + 0x10))" 3 1
mask "$((phy + 0x14))" 0x7f000000 0x23000000
mask "$((phy + 0x18))" 0x703 0x103
mask "$((phy + 8))" 0x7003 0x7003
sleep 0.01
mask "$((phy + 4))" 8 0
sleep 0.001
mask "$((phy + 4))" 1 0
mask "$((phy + 4))" 2 0
mask "$evt" 0 9
mask "$((phy + 4))" 0 4
mask "$((phy + 0x1c))" 0x20000000 0
mask "$((phy + 0x1c))" 0x40000000 0
sleep 0.001
count=$(r "$((evt + 0x20))")
printf 'USB%s_UTMI_CLOCK_EVENTS %s\n' "$1" "$count"
[ "$((count))" -gt 0 ]
sleep 0.005
mask "$phy" 7 2
w "$((pipe + 0x1c))" 1
mask "$((pipe + 0x20))" 0 0x8000
# Select host personality while both DART instances remain disabled.
mask "$((dwc + 0xc110))" 0x3000 0x1000
for off in 0 4 8 0x10 0x14 0x18 0xc110 0xc120 0xc200 0xc2c0; do
    address=$((dwc + off)); value=$(r "$address")
    printf 'USB%s_HOST 0x%x %s\n' "$1" "$address" "$value"
done
echo J873_USB2_HOST_PHY_READY_NO_DMA
