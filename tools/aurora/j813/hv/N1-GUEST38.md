# Guest38: working native Wi-Fi and DHCP under m1n1

Guest38 repeats the carrier32/native boot-object path/matched resident
LP-iBoot handoff from guest37. It retains bootstrap's existing
`hv_set_time_stealing(False, True)` setting throughout startup. There are
no synchronous TX traps or long interactive pauses in this run.

The exact native target was verified through SSH: Mac17,3, build 25G76.
`ipconfig getpacket en0` returned **DHCP ACK**, XID 0x4ae51160, leased address
192.168.68.56, server/router 192.168.68.1, lease time 0x1c20 (7200 seconds).
Three native pings to the router received three replies with zero loss.
This is a working native DHCP/network baseline under the hypervisor.

Private receipts are `logs/guest-38/native-network.json` and
`logs/guest-38/native-dhcp-packet.txt`, mode 0600. A temporary one-hour
`caffeinate -dimsu` assertion is verified by `pmset -g assertions`; it
prevents the idle-sleep path that caused guest37's SEP DART panic. No
permanent power-management setting was changed.

The first SSH monitor used the IP literal, which lacks a trusted host-key
entry and was rejected. Switching to the already trusted
`Admins-MacBook-Air.local` name resolved verification without changing SSH
trust settings or using new credentials. The old monitor was terminated.

## Linux comparison and resulting fix

`data_tx_native_prepare` began as an opt-in diagnostic in `centauri_wifi.c`.
It sets only metadata bits 17:15 to native preparation value
4, preserving the current queue, expiry, packet bytes, and policy settings.
It initially defaulted to zero for an A/B comparison. The exact native setter is
25G76 AirshipDataPacket::prepare at 0x18090faf8; guest37 captured 0x20000
on real DHCP and IPv6 descriptors. The field's semantic argument is still
under investigation, so it is not named as a frame format or priority.

The module builds with W=1 and no reported warnings/errors in
`../logs/native-prepare-20260909/n1-module-build-44.log`. The private Wi-Fi initramfs is repacked
with the candidate module; the preceding module and initramfs are preserved
under `../artifacts/n1-probe44/`. New comparison logs are grouped under
`../logs/native-prepare-20260909/` to distinguish them from historical
probe numbers. The Linux crossover subsequently returned 0/3, 3/3, 0/3
DHCP offers with preparation values 0, 4, 0; enabling 4 completed a full
DHCP lease. The field now defaults to enabled. A fresh RAM boot with the
normal queue also obtained a lease and passed bidirectional traffic, DNS
and HTTP checks. See
[Linux packet evidence](../N1-DATA-RINGS.md) for current qualification.
