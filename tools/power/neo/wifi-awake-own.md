# Associated firmware-ownership qualification

`mt7932_fullmac.awake_own_trial=1`, together with the existing `station_ps=1`,
exposes `ownership_trial_ms` and `ownership_trial_status` on the PCI device.
The default is off. Write 1..2000 ms only while the station is associated,
power saving is enabled and transport/scan admission is healthy.

This tests driver/firmware ownership without sending the system-suspend HIF
command or disconnecting the station. Commands are serialized; a separate
TX admission gate covers normal traffic, netpoll and queue wakeups during
DMA retirement. After outstanding descriptors/tokens drain, IRQ service is
synchronized and masked before the always-on ownership register is written.
The delayed return runs in the same sysfs operation independently of SSH.
Ownership and ring geometry must recover before IRQs/host traffic resume.
A failed recovery leaves admission closed for reboot recovery.

This is deliberately a bounded transport experiment: traffic waits during
the interval, and no packet-triggered wake or automatic runtime PM is
implemented. A final ownership register sample is not a residency integral.
No DMA memory is freed, no DART table is changed, and no PCI D3 is requested.

`station_ps_max` separately selects native MAX PS (1) instead of FAST PS (2)
on the next enabled policy request. Toggle station PS off then on to apply;
changing the parameter alone does not send a command. Default false. Preserve
and restore both controls when measuring, and check incoming network traffic
and reconnects separately from firmware submission success.

The kernel Image builds. The real BSS-presence/TX-retirement helper test now
checks that neither a presence event nor descriptor retirement wakes the
queue during an ownership trial, and that clearing admission permits it.
Existing netdev lifecycle regression passes. Hardware qualification pending.

## First #45 round trip and harness failure

Image SHA256 43f705795c50baaaf6a4ba7856565c6803c2f0bf25e4478ab8d467daee4134cc,
boot 8955bb94-0800-4773-b1c2-8168f38f4651. The 1 ms probe acknowledged
firmware ownership (4), returned driver ownership (0), and passed DMA/ring
retention with configuration 0x5030b875. Admission and IRQ quiescence both
cleared. The recorded interval including handoff was 6.94 ms.

The harness then used unsupported `iw dev wlan0 link` (exit161) before its
packet probe and unnecessarily invoked its fallback reboot. This is a tool
failure, not evidence of firmware recovery failure, and it does not qualify
post-handoff packet delivery. Revised checks use carrier, NM state, packet
loss and kernel association history; retain wifi-own-qualify45 as incomplete.
A separate shutdown guard now avoids WFDMA accesses if an actual failed
ownership recovery has left transport quiesced. It is not in #45 and was
not needed for this successful ownership round trip.
