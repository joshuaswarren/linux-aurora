# Native Linux N1 bring-up

## Current result: DHCP and bidirectional Wi-Fi work (September 9)

**Linux now obtains a DHCP lease.** Native packet-preparation value 4 fixes
the blocker: the same-connection crossover is 0/3 offers with value 0,
3/3 with value 4, then 0/3 after restoring zero. Re-enabling 4 completes
the full DHCP exchange and assigns 192.168.68.61/22 with a default route.
The driver now enables this field by default. A fresh RAM boot with the
normal queue obtained 192.168.68.62/22, passed router pings, incoming host
pings, DNS resolution and an HTTP download. One initial external ping was
lost; the follow-up passed 10/10. Final counters show 47 submitted and
47 completed, zero failures or bad completions. See
[the packet evidence](N1-DATA-RINGS.md).

The tested setup explicitly uses `station_link_addresses=1` and the existing
firmware/ring setup sequence documented in [README.md](README.md). This is
a RAM prototype with a qualified WPA2 single-link connection; sustained
throughput, roaming, suspend/resume and automatic startup remain unqualified.
The target remains in native Linux with Wi-Fi connected. Resident Stage 1
and installed boot files are unchanged.

Guest38 provides the working native DHCP baseline under m1n1. The
exact 25G76 Mac17,3 returned a DHCP ACK for en0 and passed three router pings
with zero loss. Time stealing stays disabled and a temporary caffeinate
assertion prevents idle sleep. Both Linux test and default-enabled module
builds pass with W=1. See [guest38 evidence](hv/N1-GUEST38.md).

Guest37 passed the missing-firmware startup failure on hardware by staging
the verified native boot-objects-path in the guest ADT. The carrier and
matched resident LP/iBoot pair are unchanged. N1 firmware boots, control
and Wi-Fi alpha report IPC-ready, and the kernel reports the Wi-Fi datapath
opening. Native DMA contexts, configuration messages and actual TX
descriptors/payloads are captured through the existing DART mappings,
including a DHCP Discover on native TR43. Its footer 0x20000, zero expiry
and queue flags 8 provide concrete differences to qualify against Linux.
Native SSH worked after clearing accumulated stolen time; later idle sleep
hit a SEP DART fault. Guest38 resolved the native baseline by retaining
time stealing disabled and inhibiting idle sleep. The Linux crossover then
isolated the missing packet-preparation value from the other differences.
See [guest37 evidence](hv/N1-GUEST37.md).

## Earlier bring-up records

These entries retain the state of each earlier experiment. Current DHCP
qualification is recorded above and in N1-DATA-RINGS.md.

Guest36 identified the preceding shutdown reason: centaurid saw built-in
hardware but cached firmware_present=0, then successfully called powerOff
from the no-firmware branch at image offset 0x23344. Although the guest
selected full macOS UUIDs, its boot-objects-path still selected the m1n1
stub. Host checks and guest37 now qualify the bound native-path correction.
See [guest36 evidence](hv/N1-GUEST36.md).

Guest35 repeated guest34's userspace
boot using the matched resident policy/iBoot pair. Passive prearm captured
six native N1 BAR reads at physical 0xb82000000, including chip identity
registers. The control IPC status was still zero; no native MMIO writes,
DMA submission or TX descriptor capture occurred before PCIe power-down.

Read-only process inspection found centaurid still alive after its control
driver exited. Its reconstructed executable has 22 resident pages and 17
explicitly recorded holes; UUID 8D0DD22B-0D13-3B45-B936-B17662C6715B. Native
startup code distinguishes activation failure, unavailable firmware and
other recovery paths, with a ten-minute deferred retry. The actual path
is not yet established. Guest36 will pause the shutdown call before device
removal using the unchanged carrier32. Native N1 TX and Linux DHCP remain
pending. See [guest35 evidence](hv/N1-GUEST35.md).

Guest33 reproduced the SEP `boop`
panic and captured 126 passive mailbox packets. Its retained shared-memory
buffer proves that SEP received the native LocalPolicy with the resident
iBoot manifest; the policy's signed nsih does not match that manifest's
SHA384. The original resident policy does match. Guest34 tested that matched
pair and boot identity while retaining the native cryptex query mirrors.
Host regression checks pass; the SEP assertion itself remains unidentified.
See [SEP handoff evidence](hv/SEP-GUEST33.md).
N1 control is enumerated, but native N1 TX capture and Linux DHCP remain pending.

Guest31 passed display isolation and
successfully authenticated and mounted the system/app cryptex images.
The boot-policy correction is now hardware validated. It reached launchd
starting fsck before `pmap_recycle_page` found a stale page reference.
A production-C regression reproduced a permission-change accounting bug;
carrier32 corrects it, passes host tests and builds successfully. Guests32
and33 passed that failure on hardware. No native N1 TX capture or
Linux DHCP fix has been demonstrated.

Guest30 stopped before root mount on
an internal-display DART panic (SID 0 PTE-invalid read at DVA
`0x100025b5000`). It did not reach the cryptex check or N1 enumeration.
A headless guest31 configuration is prepared and passes ADT preservation
checks for N1 PCIe/DART and shared audio. No macOS TX capture or Linux DHCP
fix was demonstrated in guest30; its controller was stopped before guest31.

Earlier guest29 passed the copyout
fault, authenticated Macintosh HD, started launchd and enumerated N1.
Guest28's NVMe completed 6,808 calls with zero violations. Exact native analysis
identified the missing cryptex field as `/chosen/asmb/lp-spih`; guest29
proved that field is now read correctly, but native userspace selects the
generic ticket instead of the personalized ticket expected by the signed
policy. Both sides of the failure match the captured files byte for byte
through SHA384. Guest28 avoided the external-display
panic but later stopped on a SEP epoch-request timeout. The PCI tracer
also needs to account for native PIODMA configuration access.
Native N1 DMA/TX capture and Linux DHCP validation remain pending; exact
diagnostics and private evidence are in`hv/README.md`.

Guest30 carried the source-proven ticket-selection correction, but its
earlier display fault prevented validation of that correction. Exact
25G76 AppleImage4 derives secure boot level from the inherited `lp-smb0`
and `lp-smb1` fields; the stub supplies level 2, which makes dyld select
the generic ticket. Native macOS supplies level 0. The guest now receives
the native signed policy's complete `lp-*` mirrors and boot-manifest hash,
validated against a fresh capture taken after the latest startup-disk
verification. Host binding and ADT roundtrip checks pass; hardware proof
is pending. Linux's broadcast TX/ARP/DHCP failure is still unresolved.

The stock macOS reference was reached by SSH and independently identified
as Mac17,3 / T8142, macOS 26.6.1 / 25G76, XNU 12377.161.13. Its default route
used Wi-Fi, and the native registry reported RSN complete, 5 GHz/channel40,
80 MHz and country US. Native firmware 72.129.725 / Core80211 324.129.733
matches the version strings in our staged Linux firmware.

The user subsequently authorized the hypervisor trace route to obtain a
known-good native connection/DHCP reference. The isolated preparation is in
`hv/README.md`. The M5 now executes XNU under the hypervisor (guest07),
but macOS userspace and an N1 hypervisor trace remain unproven.
Neo's KIS/SPTM bootstrap is the reference, with M5-specific PMGR, CPU,
kernel-patch and N1 checks required before it can provide that capture.
Hardware validated the carrier, complete kernel readback and SPTM seed.
A first EL2 failure was traced to M5's locked timer-FIQ gate and fixed using
the existing J813 architectural timer-mask approach. XNU then reported an
ARM_LARGE_MEMORY mismatch. Exact-build instruction comparison found that
M5's SPTM argument tail is shifted16 bytes relative to Neo. The corrected
0x350-byte layout passed the capability check on guest07. A later locked
trace-control register was given a per-CPU software shadow; the same guest
resumed and reached an unimplemented SPTM selector35. Native analysis proves
this is batched PAPT memory tagging. The locked hardware register identifies
the tag-storage RAM omitted by the previous seed. Carrier08 now implements
tag/untag mapping and reference-count updates; the Python seed reserves that
RAM and supplies the native MTE handoff. Production-function tests and the
ARM build pass. Guest08 completed3 tagging calls with no rejected requests,
then faulted on a tagged kernel address. A paused-hardware translation check
proved that enabling TCR.TBI1 resolves that address to the correct RAM.
Guest09 passed that fault, reaching36,380 SPTM calls, then failed compact
VM-pointer decoding: the Neo bootstrap virtual layout loses bit38 in M5's
pointer format. Guest10 was interrupted during upload. Guest11 exercised the
lower M5 layout and reached a different VM panic involving a static object
whose LDG returned tag0. A bounded hardware test proved that initializing its
default tag15 restores the pointer; the tag, mapping and data were restored.
The M5 seed now prepares writable kernel tags through an architectural STG
stub. Guest12 stopped before entry on a large frame-table readback timeout;
the KIS wrapper now verifies every byte through bounded1-MiB reads. Guest13
passed both failures and reached515 tagged pages, then stalled because the
carrier injected an expired timer despite the guest mask. A live isolation
check cleared that stall and reached643 tagged pages before an ST2G permission
fault on a read-only, non-tagged mapping. Carrier14 fixes the timer mask
handling; guest14 independently passed that stall. The remaining fault
was `threads_ro` resolving to tag0 because canonical tagging was disabled.
A restored single-LDG hardware probe proved TCR.MTX0/MTX1 returns the correct
default kernel tag15. The seed now enables these controls and removes the
earlier static-tag stub and mapping workaround. Guest15 passed both allocator
panics and reached78,557 SPTM calls and1,027 tagged pages with zero rejects.
It stopped on asynchronous SError during AppleT8142PMGR execution. The host
error diagnostic was corrected to avoid clearing locked registers. Guest16
reached 109,861 SPTM calls and 2,179 tagged pages, then panicked on a dart-mtp
STE-invalid fault. The PMGR trace was dominated by MCC reads; that overlap
is now writes-only. Guest15's SError remains unqualified. Guest17 isolated
MTP and reached storage, then hit ANS2's CQ doorbell panic. Read-only hardware
inspection found the same 65-advertised/64-programmed queue mismatch as Neo.
The exact M5 guest instruction correction passes offline tests and awaits
hardware qualification. Guest18 pinpointed the next EL2 SError: a forwarded
64-bit zero write to PMGR `0x211e48000`, from ApplePMGR::writeReg64. A SYNC
trace breakpoint now stops before that access for stack and storage-state
inspection. Guest19 did not enter XNU because host authorization timed out;
the helper now uses explicit Keychain-backed macOS authorization, and the
M5 startup retains its already verified KIS transport. The user restored
the normal USB proxy, carrier20c completed a fresh RAM handoff, and guest20
reached the breakpoint with KIS responsive. The stack identifies the
unconditional CPU energy-accumulator restore; a guarded, optional one-call
omission is prepared for guest21. Guest20 reached116,583 SPTM calls,2,295
tagged pages and1,180 NVMe calls, with zero MTE rejects/NVMe violations. The
earlier CQ panic did not repeat before this stop, but root mount remains
unproven. A provenance-checked private msgbuf was collected. Guest21 passed
PMGR restoration, reaching134,846 SPTM calls, then hit a USB DART replica
consistency assertion. Read-only comparison identified disabled SID14 state
omitted by the shim's power-up replay. The production-C host test reproduces
the original failure and passes with the replay correction. Guest22 reached
the root-volume mount, then failed root authentication. Direct read-only
GPT/APFS inventory proved that its inherited boot UUID identifies the
`m1n1 proxy` volume group (18 files, no snapshots or integrity metadata),
not the full `Macintosh HD` installation. Guest23 is uploading with both
guest root selectors changed to the verified macOS volume group. The other
iBoot handoff bytes retain their values. Root authentication and userspace
remain unproven, and USB0 DART had not run before guest22's different panic.
`hv/BRANCH-TRACE-REVIEW.md` records the
requested branch comparison and reusable DART-aware tracing methods. Native
userspace/N1 capture and Linux DHCP remain
unfinished. See `hv/README.md` and
`hv/MTE-NOTES.md` for hardware evidence and qualification limits.

**Reference correction:** the previously analyzed `re/dk/WLANACIAdapter`
Mach-O has minimum/SDK 27.0 and UUID 931403fe-7efa-3980-8b29-a8686a1c863b.
The Air's exact stock adapter has UUID a481c1c0-9a27-379b-a1cd-dc16b88a5ee5
and SHA-256 419ac7e82fd88fef479ac4f15d9426015e3d86a7e1e87f4fedfef64289832b0a.
`collect-n1-stock.py` now collects and checksum-verifies the target's stock
DriverKit cache over read-only SSH, extracts the relevant driver images, and
retains a whitelist of non-identifying registry fields. Inputs are private in
`AuroraSilicon/build/j813-centauri/re/stock-macos/25G76`.

This resolves the TX-power-report length discrepancy: stock
`queryLastTxStats` at 0x180914d58 explicitly requires 280 bytes, matching
firmware, while the newer image at 0x1809f2ddc requires 320. The stock
`setTxPolicyId` at 0x18090fa68 uses the same bit60 / policy<<48 encoding.
Packet prepare, relative-expiration encoding and metadata sizes also agree
in the reviewed stock routines. The stock Alpha plist differs from the
host's in 11 values (explicit single RX/monitor instances and doorbell timer
tolerance); its normal TX footer/layout agrees. Do not claim the version
mismatch itself explains absent Linux DHCP/ARP. Revalidate remaining host
protocol conclusions against this exact stock image before new experiments.

## Linux receive milestone: probe 42 (September 9)

**Directed receive is now proven.** With `station_link_addresses=1`, WPA
repeats COMPLETED and the active firmware link address equals the netdev.
Controlled host test42b sends three broadcasts and three directed UDP
frames. Linux receives all six, byte-for-byte identical to the host capture,
with zero capture drops. Earlier36f–36j tests received only broadcasts.
Private host/target captures are `re/firmware/n1-host-rx-42b-host.pcap` and
`re/firmware/n1-host-rx-42b.pcap`; the target capture SHA256 is
`41eb438732d4b1983eb62fe75a1bcb1d84e68ad4400dd7856d308329245b40c1`.

DHCP42a still receives no offer, with native TX delta4/4 including one
background packet. The remaining blocker is transmit delivery / usable
networking. This receive result must not be described as complete Wi-Fi.
The corrected image runs in RAM; DRAM firmware logging remains disabled.

The complete protected42 packet/status archive is `re/n1-evidence-42.tgz`,
1838B, SHA256
`72972009cebf3ba97e29cd829c19741be77f071616f46506f998a850b1716320`.
RAM boot43 used the same binary and explicit addresses, with
`data_tx_broadcast=1` to test the stock MCBC settings. DHCP43a (default
policy), DHCP43b (control policy1), and ARP43c–43e (EthernetII, LLC/SNAP,
two-byte prefix) each submitted/completed3 with no reply or offer and no
TX failure/bad increment. WPA completed; firmware logging stayed disabled.
The protected archive is `re/n1-evidence-43.tgz`,3582B,SHA256
`322ffa1a99932a4ac98d09a12fb6d4877f410323c6a3b17d8ce38cce80263161`.
The broadcast-only diagnostic cannot provide general networking.

Exact stock Airship final serialization also confirms the current physical
TX descriptor layout: flags3,24-bit length,64-bit payload DMA address,
16-bit tag,zero remaining-chain count for one buffer, followed by the
16-byte packet metadata footer. No missing end-of-packet flag was found.
Normal-queue RAM boot44 repeats WPA COMPLETED with explicit station link
addresses. The native unicast probe builds cleanly, but was not uploaded or
run when the user selected hypervisor tracing. Directed TX remains unproven.

## Previous Linux checkpoint: probe 41 (September 9)

The Air returned to RAM Linux with the new read-only `tx_power_report`.
Both radio bitmaps return rc0/valid1, size308, tag0xf920, length280. The
decoder follows the stock 28-byte record stride and emits only reviewed
rate/power fields. It does not expose unreviewed fields or establish an
individual packet's ACK or AP delivery.

WPA repeats COMPLETED, normal TR25 is open, and firmware DRAM logging has
remained disabled throughout this run. Firmware-source DHCP probes41a/41b
each send three packets with native submitted/completed deltas3/3 and no
offer. In the before/after41b interval, firmware host-TX increases3,
core-TX1 and success4, with no abort, expiry, peer-gone, retries-exhausted,
RX error-drop or filter-drop increase. RX crypto errors remain zero.
The no-data problem persists without firmware logging.

Exact stock source also corrects two older reference assumptions:
`connectAp` at0x1808e94cc emits TLV376 with12 bytes (newer SDK27:14);
`startInterface` at0x1808e26ec uses address TLV385, length18 (newer:386).
Firmware0x5736c0 independently recognizes385 and, when it is omitted,
generates three random locally administered link addresses. This explains
why the reported link identity differed from the netdev; it does not yet
prove that difference causes missing DHCP.

Candidate42 builds cleanly with W=1. It corrects TLV376 to12 bytes and adds
the optional, default-off `station_link_addresses` diagnostic, which uses
the netdev identity for all three potential firmware links in this
single-link WPA2 client. Hardware validation is recorded above.
Probe41 evidence is archived privately as `re/n1-evidence-41.tgz`, SHA256
`170641e8391ac772301f53bf55b456872d1a99f5cfe13770144a70b86f6d0854`.
No credentials are included in that archive.

## Previous Linux state: probe 40 (September 9)

WPA remains COMPLETED in RAM-only Linux, normal TX mode. The control-profile
comparison is complete: firmware-source DHCP with default policy (40d),
control policy 1 (40e), and restored default (40h) each sends three valid
DISCOVER packets and receives no offer. Control-policy ARP (40f) also gets
no reply. TX reaches 31/31, RX906/reposted906/bad0, and global IPv4 count is
zero at the evidence checkpoint. The diagnostic switch is restored to N.
The first 40a attempt preceded opening TR25 and is invalid as a network-path
experiment; 40d supersedes it. The helper now checks before/after firmware
TX accounting and propagates the packet probe's result.

The new reviewed firmware site `0x10a139` proves that probe39's normal TR25
(internal ring 1) selected interface0/link2/role2. That matches the pairwise
link where retained connection logs show lower-radio encryption mode2.
The group context34 also receives mode2. The immediate post-WPA host path
contains host notifications and power-save setup; no separate authorization
command was found in that bounded trace. Key bytes, encrypted frame validity,
and AP acceptance remain unproven.

The build passed W=1 and the native shell was verified. Module SHA-256:
`e4d2c7f21c013371e940c2d03f44990b9d1079590cadab59bdd9ac7ce054f242`.
The small evidence archive and post-connect firmware buffer are retained
privately on the host. The logger is consumer203/producer235; the saved post
buffer SHA-256 is `8f88ff991d5e94cde41ffb532c3c16b8305f3a4e5dca1f332d7cfa2b5373da16`.
Recheck its state before any further acknowledgement. The next boundary to
resolve is how firmware constructs and encrypts normal transmitted frames.
See `N1-DATA-RINGS.md` for test limits and exact evidence.

## Previous checkpoint: probe 39

The read-only Dot11 MIB request is hardware validated: `rc=0 valid=1
size=220 tag=0xf91b length=192`. Firmware RX-delivered and host-TX counters
match native queue counts at the post-WPA baseline. Authentication repeats
COMPLETED with general TX enabled. An ARP experiment still gets no replies.

The bounded DHCP39b interval adds exactly three host TX packets and three
firmware success results, with no TX abort, peer-gone, retries-exhausted,
lifetime-expired or RX filter-drop increase. No DHCP reply arrives. RX error
drops increase by two and the crypto snapshot reports two multicast CCMP
replay errors, without TKIP or CCMP decryption errors. Those drops are not
attributed to DHCP. Do not generalize the earlier zero-error samples to this
later interval. Evidence: `logs/linux-n1-mib-dhcp-39b.log`.

A retained post-connect firmware capture now proves TR25 itself opens with
encryption enabled: reviewed source4/site`0x10b1e1` reports ring IDs
`0x190001`, AC/index`0x190000`, state/encryption`0x10000`, multicast/EAPOL0.
This goes beyond the earlier link-level flag. It does not validate packet
headers, key selection, the encrypted bytes or AP acceptance. Those are the
next investigation. WPA remains COMPLETED, Alpha 2, TX21/21 and RX656/bad0
at the archive check. Usable networking is still unproven.

Probe39c adds a bounded DHCP DISCOVER with firmware MAC used consistently
as Ethernet source, BOOTP chaddr and client ID; probe39d is the same test
with Linux's MAC. Both send three packets and receive no offer. Independent
PCAP parsing verifies all six 342-byte frames, IP/UDP checksums, broadcast
flags and matching client/address fields. This closes the untested DHCP
identity combination; packet/client-address substitution alone is not the
fix. It does not rule out deeper firmware address configuration issues.
The updated C helper is uploaded to `/run/n1-packet-probe`; the wrapper now
prefers an explicit RAM override before the packaged `/bin` fallback.
Evidence: `logs/linux-n1-arp-39c-dhcp-firmware.log` and
`logs/linux-n1-arp-39d-dhcp-netdev.log`.

All eight connect39 firmware snapshots are saved in four protected archive
parts and their checksums verify. The separate address/DHCP archive also
retains current station/netdev identities privately. No serial command or
capture is left running at this checkpoint. The firmware logger remains
consumer203/producer235; preserve the current buffer before acknowledging
any further blocks. The post buffer at that state has already been saved.

## Previous experiment: probe 38

Normal TX mode (`data_tx_broadcast=0`) booted successfully and repeated WPA
COMPLETED. Firmware-source ARP probes to the router received no replies in
Ethernet, LLC/SNAP or two-byte-prefix layouts. A further Ethernet probe to
the host Mac also received no reply. Its native capture contains exactly
three correctly formed requested probes, while a concurrent non-promiscuous
host ARP capture contains no matching requests or replies. This is not an
over-the-air capture and does not prove where the AP path loses the packets.

DHCP still returns 1. No global IPv4 or IPv6 address, IPv6 default route or
IPv6 neighbour is present. WPA remains COMPLETED; TX reaches 33/33 and RX
993 with bad=0 at the archive check. RX crypto counters after DHCP show two
duplicates but no TKIP/CCMP replay or decryption errors. The normal-queue and
firmware-address combination is not sufficient to restore connectivity.

Evidence: `logs/linux-n1-arp-38a-normal-firmware.log`,
`logs/linux-n1-arp-38b-normal-firmware-llc.log`,
`logs/linux-n1-arp-38c-normal-firmware-prefix2.log`,
`logs/linux-n1-arp-38d-host-firmware.log`,
`logs/linux-n1-dhcp-normal-38.log`, `logs/linux-n1-ip-health-38.log`.
All nine firmware snapshots and native packet captures are retained in five
protected archive parts; their concatenation matches the target SHA-256.

The next image added a read-only `dot11_mib_stats` request matching the host's
192-byte statistics record. Its named counters include RX filter drops and
TX retries exhausted/lifetime expiry. Build passes W=1, but this request is
now hardware validated by probe39 above. Firmware `tx_status_success` will not itself be
claimed as independent proof of AP acknowledgement or packet delivery.

## Previous checkpoint: probe 37

At this checkpoint the M5 remained in RAM-only Linux, with WPA COMPLETED and Alpha at stage 2.
The latest live check records TX 3/3, RX 1127 received/reposted, zero malformed
completions, carrier up, and no IPv4 address. DHCP and ARP replies remain
unproven. The current module uses the broadcast-only diagnostic TX mode;
it cannot establish general-purpose connectivity in that mode.

Probe 37 adds a read-only RX crypto statistics request matching the analyzed
host driver. Firmware returns the expected 64-byte record. All ten duplicate,
replay and decryption error counters remain zero before and after the failed
three-attempt DHCP test. This does not prove the AP transmitted a reply.
The revised firmware logger collector also completes through successful WPA.
Both firmware and Linux station addresses change between probes 36 and 37,
but the reported firmware address still differs from Linux's in probe 37.
The mismatch is an unresolved lead, not an established root cause.

The host's normal STA base TX queue uses zero destination-MAC metadata, so
the original zero-filled AC0/TID0 extension is supported by that code path.
A next bounded experiment is the normal queue plus firmware-source packets;
the firmware-source tests so far used the broadcast diagnostic queue.
Evidence: `logs/linux-n1-connect-trace-37.log`,
`logs/linux-n1-dhcp-crypto-37.log`, `logs/linux-n1-health-37b.log`.

## Hardware-proven

- Native Linux boots through m1n1 and U-Boot with the network configuration.
- The generic ECAM host enumerates Apple 106b:1900 (N1 ROM Control).
- The Apple DART driver recognizes DART v2.4 at the measured PCIe mapper.
- `apple_centauri.ko` loads, binds to Control, reads exec stage 0, and allocates
  a coherent 16 KiB DMA mapping. This proves allocation, not device DMA.
- WPA supplicant 2.11 executes and reaches the native driver's connect path.
- Native ROM firmware DMA succeeded: response 1, stage 4 (preboot), MSI
  mailbox 0xa5a0. Linux shell/timer remained live. This proves device DMA and
  signed-image acceptance, beyond the earlier allocation-only result.
- Native PCIe PME/PERST/refclk transition succeeded: PME ack and L2 observed,
  root LTSSM returned to L0, endpoint changed to 106b:1901, execution stage 1.
  Evidence: `logs/linux-n1-reset-01.log`.
- Native OS bootstrap succeeded for Control, Alpha/Wi-Fi and Beta. The Control
  IPC handshake and CCHI hello returned successfully; Alpha accepted its
  context with the required 5 MiB scratch region. All three remained at stage
  2 for a later check approximately 446 seconds after startup.
  Evidence: `logs/linux-n1-ipc-01.log`, `logs/linux-n1-ipc-survival.log`.
- `wlan0` registered in Linux, all four Alpha command/response/event/management
  ring pairs opened, and configure/start-interface commands succeeded.
  Evidence: `logs/linux-n1-wlan-01.log`.
- Native scanning now succeeds: 24 channel events across three short chunks,
  89 beacon/probe responses accepted by cfg80211, and a real iw BSS listing.
  Evidence: `logs/linux-n1-scan-02.log`. The command-bit-30 and frame-length
  fixes resolved the first scan attempt's failures.
- Correct 5 GHz band encoding reaches successful access-point association.
  Two native attempts emitted Associated status 0, then ConnectComplete
  status 362 (four-way handshake timeout). Authentication did not succeed.
  Evidence: `logs/linux-n1-wpa-03.log`.
- Firmware key transfer is verified: `AciGetPMK` after association returned
  the exact 32-byte key supplied by WPA supplicant. Only match booleans were
  logged. Authentication still ended with status 362. This excludes a
  host-to-firmware key transfer mismatch, not an incorrect AP password.
  Evidence: `logs/linux-n1-wpa-04.log`.
  The user subsequently confirmed that the supplied password currently works
  on the host Mac for the same network. Remaining investigation is focused
  on the native firmware handshake and EAPOL/data path.
- Data TX CR 76, EAPOL TR 24, RX CR 15 and RX TR 12 now open and remain
  at stage 2. A required ten-byte WLAN clientdata extension was missing from
  earlier probes. 127 receive buffers are posted, but packet DMA is unproven.
- Control queue recycling is hardware validated. Ten scans crossed command
  and receive wrap points with 757 beacon reports. Subsequent WPA retries
  reached 1,127 beacon reports without the previous exhaustion abort.
  Evidence: `logs/linux-n1-scan-wrap-13.log`,
  `logs/linux-n1-wpa-data-buffers-13.log`.
- ACI event 0x50 now exposes non-secret handshake diagnostics. With the fixed
  CCMP cipher field set to 4, repeated attempts reported phase 3 and result
  362, with only the first two timestamps populated. Firmware receives WPA
  message 1 and enters the message-2 builder; transmission is not yet proven.
  Evidence: `logs/linux-n1-wpa-cipher-14.log`.
- Native transmit DMA is proven. Probe 16 submitted a credential-free
  EAPOL-Start descriptor; TR tail 24 and CR head 192 both advanced to 1 and
  firmware stayed at stage 2. Completion metadata was captured, but on-air
  transmission remains unproven. Evidence: `logs/linux-n1-tx-probe-16.log`.
- Probe 26 initialized the separate DRAM firmware logger (version 0x0201,
  32 blocks of 64 KiB) and privately retrieved its complete 4 MiB mapping
  with a matching SHA-256. Firmware remained at stage 2 through WPA retries.
  The ring contained diagnostic data and filled with producer 32/consumer 0.
  Records are not yet decoded; this does not identify the authentication cause.
  Evidence: `logs/linux-n1-fwlog-26-start.log`,
  `logs/linux-n1-wpa-fwlog-26.log`; see `N1-FIRMWARE-LOGGING.md`.
- Probe 27 validated logger recycling: after acknowledging consumer 29,
  firmware advanced producer 32 to 61 and stayed at stage 2 through another
  WPA attempt. Two private captures passed SHA-256 verification. WPA still
  timed out in phase 3. Evidence: `logs/linux-n1-fwlog-27-ack.log`,
  `logs/linux-n1-wpa-fwlog-27b.log`.
- Captures 27c/27d now trace the real M2 packet: construction and FGPS submit
  return, CTM flow 3/tag 6/length 141, then a failed completion for that tag.
  TX raw status 5 maps to UMAC status 3. Radio firmware logs and code identify
  status 5 as a mode-0 queue flush. On-air transmission is not proven.
- Further decoding of both captures identifies the earlier failure: the
  firmware arms the WPA timer with duration zero, reports result 362, and
  only then submits M2 into CTM. The driver's zero-filled TLV 0xf913 replaced
  the firmware's connection timeout defaults. It now supplies the pinned
  firmware defaults `[260, 6000, 2000, 15000, 300]`, including the 6000 ms
  WPA2-PSK timeout. The W=1 module build passed.
- Probe 28 validates the fix on native M5 Linux: firmware reports result 0,
  phase 7, all five handshake timestamps populated; wpa_supplicant reports
  `COMPLETED`. Alpha remains at stage 2. Authentication is now working.
  Normal packet delivery, DHCP and usable networking remain unfinished.
  Evidence: `logs/linux-n1-wpa-timeout-fix-28.log`;
  see `N1-FIRMWARE-LOGGING.md`.
- Probe 28 later exhausted all 127 RX buffers and Alpha reached stage 3.
  Probe 29 adds recycling and repeats successful WPA authentication. The
  stability check reaches 383 received/reposted packets, zero malformed
  completions, WPA COMPLETED and Alpha stage 2 across multiple ring wraps.
  Protected samples validate ordinary Ethernet framing with two prefix bytes,
  seven IPv4 header checksums and one IPv6 packet length. Evidence:
  `logs/linux-n1-rx-stability-29b.log`; see `N1-DATA-RINGS.md`.
- The next module implements Linux RX delivery and a bounded normal TX queue
  retaining DMA payloads until completion. Probe 30 was stopped at interface
  setup before transmitting: review caught overlap with firmware scratch.
  Probe 31 moves TX payloads beyond the shared scratch boundary and adds
  compile-time overlap checks. DHCP and connectivity remain unproven.
- Probe 31 repeats WPA success, delivers 1,270 packets to Linux, and submits
  37 normal TX descriptors with successful firmware statuses. RX has zero
  malformed completions; Alpha stays at stage 2. DHCP still gets no lease.
  A verified coalesced TX completion retired two buffers, while the initial
  implementation released only the final cookie. Probe 32 fixes that count
  handling and stages tcpdump for a native packet capture. Probe 32 validates
  WPA COMPLETED and 7 submitted/7 completed TX buffers, including one
  coalesced completion, with no TX errors. See `N1-DATA-RINGS.md` and
  `logs/linux-n1-dhcp-observed-31b.log`.
- Probe 32's packet capture fails because the minimal kernel lacks EVENTFD.
  Its error handler exits the PID-1 shell and causes a kernel panic before
  DHCP runs. The next image enables EVENTFD; the command helper isolates
  exit/errexit in a subshell and propagates remote status, and init keeps
  PID 1 alive when consoles exit. Local shell checks pass. This is a harness
  recovery, not DHCP or radio evidence.
- Probe 33 validates the harness recovery, repeats WPA success, and captures
  three correctly formed DHCP DISCOVERs directly in Linux. No DHCP reply is
  received, and the capture has no kernel drops. TX is 14 submitted/14
  completed with no errors; RX is 82 received/reposted with no malformed
  entries; Alpha stays at stage 2. The next investigation is firmware
  broadcast/per-destination TX ring metadata. See `N1-DATA-RINGS.md` and
  `logs/linux-n1-dhcp-capture-33.log`.
- Probe 34 delays normal TX ring creation until after WPA success. DHCP
  still gets no reply, despite three valid captured DISCOVERs. All 13 TX
  packets complete and all 69 received packets reach Linux with zero RX
  drops/errors and TX errors. Alpha remains at stage 2. Late ring creation
  alone is not the fix. A separate broadcast TX ring in the measured host
  driver is the next concrete protocol difference to trace.
- Probe 35 uses the measured broadcast ring extension after WPA completes.
  It still captures three valid DHCP DISCOVERs and no reply. All three TX
  packets complete successfully, all 352 RX packets are delivered with zero
  errors/drops, and Alpha remains at stage 2. The diagnostic intentionally
  drops other TX destinations. Broadcast metadata alone is not sufficient;
  packet metadata and a non-DHCP network exchange are the next checks.
  Probe 35 is live and authenticated. See `N1-DATA-RINGS.md` and
  `logs/linux-n1-dhcp-broadcast-ring-35.log`.
- A separate ARP exchange also receives no reply. Firmware capture 35c
  identifies host TR 25/cookies 9 and 10 in the radio completion path with
  raw status 0, matching normal TX progress. The remaining boundary is
  packet framing/encryption and AP acceptance, beyond mere queue progress.
  WPA remains successful, TX reaches 10/10, RX reaches 901 with bad=0, and
  Alpha stays at 2. This is still not usable IP connectivity.
- Probe 36 separates log capture from the command-worker mutex. A retained
  snapshot with scanning=1 proves logging is available while that worker is
  busy. Two private captures contain the successful WPA timer/result and
  link 2's enabled data-encryption flag, ruling out a missing enable flag.
  DHCP still gets no reply, with 3/3 successful TX completions and 499 RX
  packets without malformed entries. Native Ethernet, prefix2 and LLC/SNAP
  probes all fail to get ARP replies. See `N1-FIRMWARE-LOGGING.md`.
- Probe 36f–36m establishes controlled broadcast reception from the host,
  while directed probes are present in host captures and absent in target
  captures. The firmware's reported local station address differs from
  Linux's; the same statistics header identifies the correct associated AP.
  Sending to that firmware address and using it as the ARP source still do
  not restore replies, including all three packet layouts. Address mismatch
  alone is not an established cause. Latest live state is WPA COMPLETED,
  Alpha 2, TX 21/21, RX 5204/bad=0, and no IPv4 address. All tests finished;
  probe36 remained booted at that checkpoint; probe37 now supersedes it.
  See `N1-DATA-RINGS.md` for evidence and limits.

Evidence: `logs/linux-wifi-boot-01.log`, `logs/linux-n1-module-03.log`.
Firmware evidence: `logs/linux-wifi-boot-02.log`, `logs/linux-n1-rom-01.log`.
The first module lacked version records because allnoconfig disabled both
version formats despite MODVERSIONS=y. Enabling BASIC_MODVERSIONS corrected
the module, and it loaded in the already-running kernel without force flags.

## Current experiment

The module has a root-only `rom_boot` debugfs trigger. It requests the saved
J813 firmware, allocates firmware/descriptor/MSI-mailbox buffers through Linux
DMA APIs, programs the measured ROM scatter descriptor, and polls the result.
It retains buffers until removal, including on timeout. Removal disables MSI
and bus mastering before freeing them. ROM loading, OS-stage firmware,
radio startup, scanning and WPA authentication are proven. Normal Linux
packet delivery is running; DHCP and usable connectivity are being tested.

The larger RAM image includes the validated firmware and regulatory database.
`logs/kernel-wifi-build-02.log` is the current build log; a historical compile
error there was fixed by removing the obsolete no_llseek initializer.
`logs/linux-wifi-boot-39.log` is the latest completed native boot capture.
The firmware key-check proof was captured on boot 07.

WPA attempts in `logs/linux-n1-wpa-02.log` repeatedly received firmware status
349 (`AciWifiInalidChannels`) before authentication. The older RE notes had
swapped the 5/6 GHz ACI band values. `WLANACIAdapter`'s `AppleChanSpecToACI`
at `0x180a08400`, its inverse at `0x180a084d4`, and chanspec band constants
confirm that ACI 1 is 5 GHz and ACI 2 is 6 GHz. The driver, ABI notes and
retained proxy connect helpers are corrected. The subsequent hardware test
passed association and removed status 349, but exposed handshake status 362.

The first three scans exhausted the prototype's fixed receive postings.
A modulo-index recycling experiment, based on AirshipDK's
`ring_index_pair::advance_head_index` and `advance_tail_index` at
`0x180293a2c` / `0x180293af4`, faulted Alpha after 192 management frames.
`logs/linux-n1-alpha-abort.log` confirms Alpha execution stage 3 while Linux
remained responsive. That experiment was withdrawn, then revisited after
fixing the TR size field: size belongs at wire +0x18, not +0x1c. Probe 13
validated recycling with the corrected size. This earlier failure is resolved.

The current optional `verify_pmk=1` module parameter queries AciGetPMK
(`0xf0025`) after association, compares the returned 32-byte key to the one
supplied by WPA supplicant and erases the key buffers. Only availability and
match booleans are logged. Hardware returned `rc=0 available=1 matches=1`.
The diagnostic is disabled by default. It does not establish whether the
supplied password is correct.

The protected WPA setup helper now uses a distinct acknowledgement per
command, accepts a first-line success marker and verifies the target config
checksum. Credentials are not printed. The initramfs mounts RAM `/run` and
debugfs. Its packaged module and init match the standalone artifacts.
The probe-14 replacement module was uploaded into target RAM with SHA-256
verification before loading (`logs/linux-n1-module-upload-14.log`). The
firmware-containing initramfs is mode 0600. WPA retries were stopped after
the phase-3 diagnostic (`logs/linux-n1-cipher-health-14.log`).

The subsequent raw `/dev/mem` PCI-register health check raised an
asynchronous SError in Linux (`logs/linux-n1-final-health.log`). Do not repeat
that check. Its cause has not been established; it is separate from the
saved association/key-match proof. Recover through RID 1 and RAM chainload,
then use the module's mapped debugfs status plus normal shell/timer checks.
Require `run-chainload.py` to exit successfully before starting
`boot-linux.py`; a transient resident-proxy timeout does not satisfy that gate.

Probe 15 is hardware validated through radio startup. Disabling power save
(ACI command 7, mode 3) succeeds, and normal data TR 25 opens at stage 2.
Neither changes the WPA phase-3 timeout. The station statistics request
(ACI bit 37) returns success but no validated station record. Evidence:
`logs/linux-n1-power-off-15.log`, `logs/linux-n1-tr25-15.log`.

Probe 16 validated EAPOL-Start DMA and found that the native station counter
record is 752 bytes, versus 888 in the analyzed host helper. WPA still fails;
a 2.4 GHz attempt timed out at phase 1. Probe 17 validated the shorter
record. Probe 18 corrected the MLD-global defaults but WPA still timed out.
It submitted EAPOL-Start directly from association-event handling, and
completed DMA with firmware TX status 3 (non-success). The optional test is
bounded to eight independent descriptors per boot. Probe 19 booted successfully
and verified country synchronization: the initial firmware XZ setting became
US after `iw reg set US`. WPA still timed out in phase 3 with the confirmed
US setting. Evidence: `logs/linux-n1-country-us-19.log` and
`logs/linux-n1-wpa-us-19.log`. The target remains healthy with retries stopped.

Probe 20 adds a read-only firmware link-status query. Its TLV cursor was
corrected to +32 in probe 21. The optional trace-ring experiment in probe 21
then caused Alpha to enter abort stage 3 before returning trace data; Control,
Beta and the Linux shell remained live. The trace implementation is removed.
Probe 22 restored scanning and association. Firmware accepted the macOS
packet-lifetime defaults and control TX profile 1; profile 1 reads back
exactly after configuration. Both separate WPA tests still time out in phase 3.
The association-time EAPOL-Start probe using profile 1 completes DMA but
does not resolve authentication or produce RX data.
Probe 23 verifies that explicit host wakeup (ACI command 6, awake state 0)
is accepted with status 0 and Alpha stays at stage 2. WPA still stops in
phase 3, both alone and combined with the TX defaults. The bounded
association-time packet probe completes with TX status 3 and no RX data.
Probe 24 confirms the same TX status for unicast EAPOL-Start. Probe 25
extends completion visibility to all eight diagnostic buffers and makes
the automatic probe run once per explicit request. Its unicast probe with
explicit relative expiration still returns TX status 3; WPA stays in phase 3.
The native shell is healthy, with retries and the one-shot probe disabled.
No WPA or data-connectivity success is claimed.
`reboot-m5.py` accepts an authorized
administrator credential through a no-echo prompt and passes it to osascript
through stdin, without retaining it in source, argv or logs. These reboots
completed and `chainload-wifi-19.log` proves the RAM proxy is alive.
Use the protected WPA helper after each fresh RAM boot, and specify
`wpa_cli -p/run/wpa_supplicant -iwlan0 status` to use the configured socket.

## Firmware provenance

The original macOS diagnostic capture was retained under the old research
job's `tmp/diag/Diagnostics-2026-08-17-00-09-01-750` directory. Its 81 factory
tags are byte-identical to the retained J813 firmware extraction. Patch bays
match the separately saved J813 files; calibration matches the captured
metadata container. The global production IPSW manifest covers the same
payloads; only its server nonce and signature differ from the macOS capture.
`stage-n1-firmware.py` checks these boundaries and substitutes that complete
signed manifest without modifying its contents or any firmware payload.

Firmware and board calibration live only in private local build artifacts.
Do not publish them. Network credentials are held in a separate protected
local file, never in source, logs, documentation, or Discord.

The UMAC image has an embedded `fwsg` version-1 segment table at file
0x276800, referenced by its final 32-byte footer. `map-n1-firmware.py`
reconstructs all 37 segments into the private analysis-only
`aciw-mapped.elf`. The map verifies 2,425 UMAC catalog assertion instructions;
36 catalog sites belong to ROM and one is the crash-handler Thumb pointer.
Prefer this complete map to the earlier partial raw-bank imports. It is
not a boot payload and does not change the signed firmware used by Linux.

The same mapper now supports `--image wftp`: all three TX-processor segments
and 958 assertions verify. The complete TX Ghidra project is
`j813-n1-tx-mapped` / `wftp-mapped.elf`. Further reference checks confirm the
default TX-ring client extension and a software SHA-1 compression path.
Neither is a proven fix. The next tracing boundary is the internal
firmware-generated packet queue and its completion callback; see the
follow-up analysis in `N1-DATA-RINGS.md`. Probe 25 remains the last hardware
test and authentication is still failing.

## Remaining work

1. Completed: native ROM firmware DMA and signed-image acceptance.
2. Completed: measured PCIe PERST/refclk transition from preboot to boot.
3. Completed: secondary memory, Control CCHI hello, memswap handoff and Alpha
   IPC handshake. `centauri_ipc.c` uses measured v259 layouts.
4. Completed: native scanning. Mask command bit 30 (more-data-pending) and
   use actual management payload lengths. The earlier failed attempt is in
   `logs/linux-n1-scan-01.log`; the passing run is `linux-n1-scan-02.log`.
   Queue recycling is now hardware validated by probe 13.
5. Complete authenticated association and packet TX/RX with WPA supplicant.
   WPA2-PSK connect/disconnect operations and firmware handshake-offload flags
   are implemented. Correcting the band code proved association; hardware
   still reports a four-way handshake timeout with the supplied credentials.
   Connection success requires firmware's
   ConnectComplete event, not merely Associated. TX stays stopped even after
   authentication until the data-ring protocol works. `configure-wpa.py`
   installs the protected configuration into target RAM without logging it.

The prior proxy experiments prove scanning and association attempts. Native
Linux now additionally proves queue reuse and data-ring opening. WPA still
stops in firmware phase 3; there is no authenticated connection, packet
delivery, DHCP lease or connectivity proof. See `N1-DATA-RINGS.md` for the
firmware/host ABI corrections and exact probe evidence.

## Recovery and reporting

Keep the resident image and internal disks intact; payloads use RAM chainload.
Select only host RID 1 and the dedicated M5 KIS console. RID 0 is the separate
Neo task. See README.md for recovery commands and source revisions.

Progress reporting contract (reread in full after context loss):
`/Users/mcruz/.codex/attachments/877c20f2-0018-42bb-9097-021833f19a67/pasted-text.txt`.
Webhook comes only from the authorized runtime environment. Discord has
received hardware milestones for native firmware/IPC startup and scanning,
followed by the confirmed WPA channel-encoding diagnosis and successful
association milestone. Authentication and packet networking are not working.
