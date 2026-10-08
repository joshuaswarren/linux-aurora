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

This is deliberately a bounded transport experiment: traffic may wait during
the interval, and automatic runtime PM is not implemented. The optional
`awake_own_irq` mode described below still requires hardware qualification.
A final ownership register sample is not a residency integral.
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

## Associated round-trip qualification completed on #45

Boot 5df69e17-6e70-4a2f-8ee5-5e6f36d939e6: 1 ms, 100 ms and two 1000 ms
trials passed ownership/DMA recovery, carrier and NM connected checks, and
all twenty post-trial gateway pings. No disconnect/reassociation was logged.
FAST PS latency is still variable (one probe 904 ms); these bounded holds
do not yet qualify continuous packet-triggered wake or a power saving.
Capture: wifi-own-qualify45b/.

## MSI wake candidate

`awake_own_irq=Y` preserves the normal device interrupt mask during a bounded
ownership trial and enables Linux MSI after ownership has been acknowledged.
The guarded hard handler does no WFDMA access; it takes exactly one disable
reference and signals completion. The controlling task synchronizes the
handler, balances that reference, recovers driver ownership, checks DMA and
finally restores the normal IRQ path. A timeout follows the same recovery.
Legacy INTx is refused. An IRQ may be a stale cause or background event: early
completion alone is not proof that a particular packet woke the interface.
Test quiet controls and timestamped external probes separately.

The real IRQ-handler regression checks zero WFDMA accesses during ownership,
only one mask reference for repeated MSI delivery, and ordinary dispatch
after restoring the guard. Built kernel #46; hardware checks pending.

## Station profile comparison on #45

Same boot as the completed ownership qualification, all ownership probes
finished before measurement. Three 120-second LCD/backlight-off intervals:
FAST PS 1.152648 W, MAX PS 1.331233 W, restored FAST PS 1.150693 W. All
post-interval three-packet gateway probes had zero loss. MAX PS therefore
provides no saving with this firmware/AP configuration; its draw resembles
CAM. This is an observed outcome, not proof of how firmware interprets or
honors that profile. The selector and original PS setting were restored.
Capture: wifi-maxps-series45/.

## #46 interrupt wake exposes a rearm failure

Boot e9edfe4e-c27e-4ef9-9944-0a131b93a87c, Image SHA256
8e8cdd4564cb6e9da1b5f895c9a370d1b293456ffcb1a5e8dc9f5a60f18fc9b0.
The timer-only 100 ms control recovered and all three pings passed. With
MSI wake armed, the next 100 ms trial ended in 12.07 ms with irq_woke=1,
last ownership word4, result0 and admission/quiescence cleared. However,
all subsequent gateway probes failed, so the harness saved logs and rebooted
into normal #6. No power test ran. Capture: wifi-ownirq-qualify46/.

A code review found the consumed MSI cause was never serviced by the guarded
handler. Since the device mask was already enabled, restoring that same mask
did not ensure a fresh edge. #47 masks the device, drains and acknowledges
pending causes after ownership and DMA verification, then enables Linux IRQ.
This is the next candidate fix, not a passing network result yet.

## #47 rearm correction passes short network checks

Boot e18d0253-14f1-4cb2-a911-c4e690f0dd59, Image SHA256
e7e51e2ed35c9a804fe002ab17aaeb773fdf487f795f7114fc6560d5d87c58c9.
The timer-only 100 ms control and four MSI-enabled holds passed ownership,
geometry, carrier and all fifteen post-trial pings. The MSI holds ended in
15.0, 49.8, 77.5 and 3.9 ms, with ownership word4 observed before reclaim.
This fixes the observed receive stall. It does not identify which packet or
background event generated each MSI, nor prove continuous idle residency.
Capture: wifi-ownirq-qualify47/. A duplicate service launch refused to reuse
its existing output directory before any hardware access; the first run's
passing JSON and full kernel log remain the evidence, not that service's
later failed status. A bracketed power experiment follows separately.

## #47 bracketed battery result: no resolved saving

On the same boot, three 120-second awake LCD/backlight-off FAST PS intervals
measured 1.150433 W (reference), 1.156354 W (repeated ownership), and
1.153835 W (restored). Raw accumulated battery counter drops were 33,464,
33,618 and 33,559 respectively, corroborating the lack of improvement without
assuming those raw units are microamp-hours. These are whole-machine watts,
not an isolated radio-rail allocation. The target remains unmet.

The bounded controller completed 608 trials in 132.182 seconds, no busy
admission retries or recovery failures. 606 ended on MSI, two on the deadline;
all final ownership observations were 4 before recovery. Mean recorded hold
was 200.669 ms, median 73.793 ms, range 0.317..1027.837 ms. Those intervals
include handoff overhead and are not a measured firmware sleep residency.
An incoming SSH command succeeded during the loop, taking 2.137 seconds and
reading carrier 1 and healthy status at trial 169. Outgoing traffic can still
wait for an interrupt or the bounded deadline; this is not a production
latency qualification. Post-stage gateway probes all passed without loss.

Captures: wifi-own-power47/ and own47-incoming-ssh.json. The proven controller
is retained as wifi-own-loop.py (remote original own-idle-loop.py), requiring
explicit PS and awake_own_irq selection and a bounded duration. It changes
neither policy automatically. Do not overlap it with another ownership,
suspend, display or power-policy controller. The display trial used its
separate bounded supply/backlight restoration.

Final state: #47 booted, regular FAST PS on, awake_own_irq=N,
station_ps_max=N, thermal idle_poll_ms=0, ownership active/quiesced=0,
613 total successful ownership trials, LCD supply restored. The unused
recovery timer was stopped. Normal #6 remains the GRUB default. Changes and
results are committed locally on eryk_neo_deepsleep; no upstream operations.

The next useful native capture must resolve the remaining policy gap rather
than repeat acknowledged ownership writes: live DCP/SIO shutdown coordination,
ApplePMGR's actual register mappings/access prerequisites, and the Wi-Fi DMA
clock policy during native associated idle. Linux's active WFDMA word includes
bit30 (CLK_GAT_DIS in the MT792x register definitions). The decoded native
ownership callback only writes the always-on ownership register; that does
not establish when it is safe to change DMA clock policy. Keep native
runtime observations distinct from startup constants or bit names. The
existing DCP traces show successful native OFF state but omit the full
firmware exchange and cannot explain Linux's prior CPU-domain write faults.
A further macOS session is needed before reproducing those missing sequences.
