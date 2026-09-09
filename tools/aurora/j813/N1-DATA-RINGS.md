# N1 data-ring protocol evidence

## DHCP fix: native packet preparation value 4 (September 9)

The macOS hypervisor trace captured a real DHCP Discover descriptor with
metadata 0x20000. Exact 25G76 AirshipDataPacket::prepare at 0x18090faf8
encodes `(argument & 7) << 15`. Linux left those three bits zero.

A same-association Linux crossover isolated this field while preserving
queue configuration, packet construction, expiry and policy:

| Test | Bits 17:15 | Sent/completed | DHCP offers |
| --- | --- | --- | --- |
| native-prepare-a | 0 | 3/3 | 0 |
| native-prepare-b | 4 | 3/3 | 3 |
| native-prepare-a2 | 0 | 3/3 | 0 |

Re-enabling value 4 then completed DHCP, assigned 192.168.68.61/22, and
installed the default route through 192.168.68.1. The lease capture contains
four packets with zero capture drops. All three crossover tests have zero
TX failures/bad completions, showing why DMA completion alone did not
establish radio delivery.

`data_tx_native_prepare` now defaults to true; disabling it is retained
only to reproduce the old failure. The existing two-second relative expiry
remains: the corrected Linux footer is 0x30020000 plus 2000000, whereas the
native packet used 0x20000 plus zero. Native queue flags 8 are also not
required for the successful broadcast and normal queue tests. No speculative EOP or
footer-layout change was needed. The exact semantic name of the preparation
argument remains unassigned.

Logs: `logs/linux-n1-arp-native-prepare-{a,b,a2}.log` and
`logs/native-prepare-20260909/dhcp-lease.log`. Private PCAPs, status and lease
receipts are archived under `logs/native-prepare-20260909/evidence.tgz`.
The archive is 2812 bytes, SHA256
`42b1c931150ec1a824d4ecf16b13fc8862dc75d590f2b79a5a9b3329e4bd6837`.
Independent decoding validates IPv4/UDP checksums in all four PCAPs:
A and A2 contain only three Discovers each; B contains three Discovers
and three Offers; the lease contains Discover, Offer, Request and ACK in
one transaction.

### Fresh boot with the normal queue

The rebuilt default-enabled module was loaded from a fresh RAM initramfs.
Its verified parameters are `data_tx_native_prepare=Y`,
`data_tx_broadcast=N`, and `station_link_addresses=Y`. WPA reached COMPLETED,
then normal TR25 opened successfully. DHCP assigned **192.168.68.62/22**,
with default router **192.168.68.1**. Independent PCAP decoding confirms
one Discover, Offer, Request and ACK in one transaction, all IPv4/UDP
checksums valid and zero capture drops.

| Check | Result |
| --- | --- |
| Linux to router | 3/3 replies, zero loss |
| Host Mac to Linux | 3/3 replies, zero loss |
| Linux to 1.1.1.1, initial | 2/3 replies |
| Linux to 1.1.1.1, follow-up | 10/10 replies, zero loss |
| DNS lookup | example.com resolves successfully |
| HTTP download | wget succeeds; returned title is Example Domain |
| TX at traffic-test end | 47 submitted/completed, zero failures/bad completions |
| TX at evidence archival | 49 submitted/completed, zero failures/bad completions |

The isolated initial external ping loss was not reproduced in the follow-up;
its cause is not established. This proves DHCP and bidirectional traffic on
the tested WPA2 single-link configuration, not long-duration reliability,
throughput, roaming or suspend/resume qualification.

Final logs are grouped under `logs/native-prepare-20260909/`:
`normal-{boot,setup,connect,dhcp,traffic,traffic-repeat,host-ping}.log`.
Private `normal-evidence.tgz` contains the DHCP capture, client/capture
receipts, module parameters, address/route/link status and downloaded HTML.
It is 2236 bytes, SHA256
`237766c3536d88dae5067e3439687c296c9130cf6b1920157c4ca8f16c7d209e`.
The final module SHA256 is
`93c70f3c4c37144446d46bf5f076af15c5d225dd94093e9f10489bdfa4183bb4`;
the private initramfs SHA256 is
`e20d2a0d3f6d4fa779079be04f14a3d5953bdcd6829fc16ac28d4259e5cc486f`.
The W=1 build passed without warnings/errors, and all five artifact manifest
checks passed before boot. [README.md](README.md) records the working setup.

## Historical packet-path experiments

The following entries describe the result at each earlier probe. The current
DHCP and normal unicast results are recorded above.

### Probe43 and exact stock final transmit descriptor (September 9)

With explicit station link addresses and broadcast TR25, DHCP43a/default
policy and43b/control policy1 both send3/complete3 with zero offers. ARP43c
EthernetII,43d LLC/SNAP and43e two-byte prefix likewise send3/complete3 with
zero replies. No TX failure/bad increment occurs. The protected packet and
status archive `re/n1-evidence-43.tgz` is3582B,SHA256
`322ffa1a99932a4ac98d09a12fb6d4877f410323c6a3b17d8ce38cce80263161`.

Exact25G76 AirshipDK independently confirms the final DMA record:
`ashp::direct_tr_controller::enqueue_batch`0x18023142c gets the packet's
data offset/size and passes its inline footer to
`ashp::acipc::tr_controller::enqueue_transfer`0x18028e234. That routine
uses flags2 for inline metadata plus1 for mapped payload. Single-buffer
chain finalization at0x1802837c8 leaves remaining-chain count zero.
`ashp::acipc::tr_controller::sync`0x18028bd48 serializes flags/length at0,
payload address at4,tag at12,chain count at14,then copies the configured
footer after the16-byte physical descriptor (header size0). This matches
Linux's current layout; do not move the footer or add a speculative EOP bit.

Directed TX through the normal queue remains unproven. The bounded packet
helper now supports three unicast ARP probes to the host's locally read
en0 address, with sender IP zero and no network reconfiguration. Independent
host observation can establish delivery without relying on a DHCP offer.

## Stock 25G76 reference and probe 41 (September 9)

Read-only collection from the Air establishes macOS26.6.1/25G76, stock
WLANACIAdapter UUIDa481c1c0-9a27-379b-a1cd-dc16b88a5ee5. The earlier `re/dk`
adapter was built for SDK27. The exact stock cache and extracted images are
private under `re/stock-macos/25G76`; source/download SHA256 comparisons pass.
The staged N1 firmware version strings match the native registry.

Stock `queryLastTxStats`0x180914d58 requires280 bytes, not320. Stock
`getLastTxInfo`0x1809150f8 walks28-byte records, not32. Probe41 validates the
new `tx_power_report` request on both radio bitmaps: rc0/valid1, size308,
tag0xf920/length280. Only reviewed rate/power offsets14–17,20–21,24,27 are
printed; unreviewed fields are omitted. These are recent firmware records,
not a per-packet ACK or AP-delivery result.

WPA completes with normal TR25 and DRAM firmware logging disabled. DHCP41a
and41b both send3/complete3 with no offer. Interval41b MIB deltas are
host-TX3, core-TX1, success4; abort/expiry/peer-gone/retries-exhausted,
RX error-drop and filter-drop stay unchanged. RX crypto error snapshot is
zero. Logging is therefore not required to reproduce the data failure.
Evidence: `logs/linux-n1-before-41b.log`, `logs/linux-n1-after-41b.log`,
`logs/linux-n1-arp-41a-dhcp-no-fwlog.log`,
`logs/linux-n1-arp-41b-dhcp-no-fwlog.log`.

Stock `AppleWLANLogicalLinkSTA::attachIPCRings`0x18035c6a8 and
`AirshipIPCAdapter::attachDataTxRing`0x1809098dc preserve the measured MCBC
setup: broadcast destination and AC3 mapped to TID6. The packet metadata,
policy encoding and physical16-byte footer agree with the earlier reference.
Stock `dumpMloStatus`0x180915a14 also expects TLV377/64bytes and the same
17-byte link stride. Probe41's non-MLO status lacks TLV377; this is not
evidence of a newly shifted parser layout.

### Address-generation source result and candidate 42

Stock `connectAp`0x1808e94cc emits TLV376/12bytes; Linux previously sent the
SDK27 14-byte form. The candidate corrects that exact length.
Firmware `0x574000` copies only12 bytes for this tag; the two trailing zeros
in the old request are not, by themselves, evidence of a changed option.
Stock `startInterface`0x1808e26ec appends TLV385/18bytes when its address-array
flag is set; the newer image used386. Native firmware `0x5736c0` confirms
tag0x181 and strict length18. If absent, it invokes RNG three times and
forces each generated address to locally administered unicast before writing
the18-byte array into the interface object at+6. Configure-interface
`0x573584` separately copies the six-byte host address to interface+0.
This proves the source of the observed netdev/link address difference.

Candidate42 adds a default-off `station_link_addresses` switch. For the
experimental single-link WPA2 client it supplies the netdev identity in all
three possible link slots before firmware interface start. It does not set
the association's MLO flag. Treat its effect on encrypted data as untested
until hardware packet delivery succeeds. Module SHA256:
`ada8682327625a2a9b62a1feb7c33baedd838e245d0c8b359ed67b38704e1cc1`;
initramfs SHA256:
`f05692e1053cd4b72eacd8ea521decf6098872b9cdf3191b301ae406b8ff0e1b`.

Hardware42 accepts `station_link_addresses=1`, repeats WPA COMPLETED, and
reports non-MLD/bitmap2 with the active link address equal to the netdev.
This validates the address setup change. The protected identity archive is
`re/n1-identity-42.tgz`,319B, SHA256
`f1c2ebeea044dc53d0de066741578f8fb2009462bf374203886900fd6ad3892e`.

Controlled receive42b is the first verified directed receive in this campaign:
host sequences0–2 broadcast and3–5 unicast all arrive on Linux. All six
Ethernet frames match host captures byte-for-byte; tcpdump reports zero
kernel drops. WPA remains complete. Target PCAP594B SHA256
`41eb438732d4b1983eb62fe75a1bcb1d84e68ad4400dd7856d308329245b40c1`.
DHCP42a still has zero offers (TXdelta4/4 including background traffic),
and ARP42c to the router has zero replies (TXdelta3/3, failed0, bad0).
The corrected identity clears directed RX but is insufficient with the
generic normal TX queue for these broadcast probes. Next compare the
stock MCBC queue settings with the corrected identity; earlier MCBC tests
preceded the address correction.

Probe41's protected packet/status archive is `re/n1-evidence-41.tgz`,1099B,
SHA256`170641e8391ac772301f53bf55b456872d1a99f5cfe13770144a70b86f6d0854`.
Its station snapshot is non-MLD, bitmap2, and confirms the active firmware
link differs from the netdev. No credentials are archived.

## Probe 28: authenticated receive DMA and exhaustion

The connection-timeout fix completed WPA authentication (firmware result 0,
phase 7; wpa_supplicant COMPLETED). RX CR 207 and TR 12 both advanced to 60,
with valid completions for distinct posted buffer cookies and nonzero packet
lengths. This proves receive DMA after authentication. Completion word 0 is
0x000c0403; word 1 contains cookie in bits 15:0 and byte length in bits 31:16;
word 2 is zero. The 16-byte WLAN footer begins at byte 12; entries stride 32.

The subsequent stability check found Alpha stage 3, RX CR 127 and TR 127/127.
No host buffer recycling existed, so the posted pool was exhausted. Do not
claim a stable connection or ordinary network delivery from the WPA result.
Evidence: `logs/linux-n1-wpa-timeout-fix-28.log` and
`logs/linux-n1-connected-28-stability.log`.

Probe 29 adds bounded RX completion consumption to the existing locked poll
path. It validates ring indices, ring ID, cookie, length and completion status,
copies up to eight private samples, acknowledges CR entries, reposts the
same buffers with their original cookies, and rings the measured RX doorbell.
It counts packets as dropped until the Ethernet framing is verified.
Probe 29 validates recycling across multiple wraps: 383 packets received and
reposted, zero bad completions, WPA COMPLETED and Alpha stage 2. Evidence:
`logs/linux-n1-rx-stability-29b.log`. Its private snapshot is
`build/j813-centauri/re/firmware/n1-rx-snapshot-29.bin` under AuroraSilicon,
33032 bytes, mode 0600, SHA-256
`5b7f0619034bb821602bcc28f12cde0633dc6e77b4ed9de9526353b847a0bf92`.
`inspect-n1-rx.py` validates all eight frames after skipping the footer's
two-byte prefix: seven IPv4 packets with valid header checksums and one
IPv6 packet with a valid length. No addresses or payloads are printed.

For subsequent netdev TX, the existing TR 25 uses 128 entries of 32 bytes and
TX CR 192 has 2048 entries of 32 bytes. Retain coherent payload storage until
the corresponding completion, not merely until the transfer tail advances.
The completion's inline word at +12 carries firmware status in its low byte.
Host `mapFwTxStatusToHostTxStatus` at 0x180a08c54 treats 0 and 0x83 as success;
other mapped codes are failures. `AirshipDataTxTransferRing::processCompletions`
at 0x180a0d404 reads that status from packet-private +0x20. Normal TX must use
a bounded skb queue and a worker because ndo_start_xmit cannot take the
existing sleeping firmware mutex. Probe 31 implements this bounded queue,
uses 128 separate coherent payload slots at window +0xd00000, and enables
ordinary RX delivery via netif_rx. Window +8 MiB through +13 MiB is reserved
firmware scratch; compile-time assertions prevent the TX region overlapping
that range or RX buffers. The earlier probe 30 reached interface setup but
was not connected: review caught its overlapping proposed TX region before
any packet submission. Probe 31 repeats WPA success and delivers 1,270 RX
packets to Linux with zero malformed completions. Normal TX consumes 37
descriptors with successful firmware statuses, but only 36 buffers retire.
Ordinary and broadcast DHCP attempts receive no lease. Evidence:
`logs/linux-n1-health-31c.log`, `logs/linux-n1-dhcp-observed-31b.log`.

Probe 31 identifies a coalesced-completion bug: TX completion cookie 5 has
word 2 equal to 0x200, retiring cookies 4 and 5 together. Airship's
`cr_controller::sync` at 0x1802ff258 maps descriptor bytes 9:10 into the
completion's u16 count. `transfer_queue::complete_transfer` at 0x1802e3800
uses byte 1 bit 0 as the multicomp flag and walks backward from the final
cookie to find the preceding count-1 transfers, applying the same inline
status to all of them. The Linux candidate validates every busy slot before
retiring the group and exposes coalesced/tx_bad counters. Probe 32 validates
one coalesced completion on hardware: WPA COMPLETED, Alpha stage 2,
7 submitted/7 completed, coalesced=1, tx_bad=0 and failed=0. Evidence:
`logs/linux-n1-wpa-32.log`. This explains the one
unreleased TX buffer, but is not yet established as the DHCP failure cause.

A protected 20-second capture on the host's existing Wi-Fi interface sees
no DHCP frames during the probe 31 broadcast attempt. The capture ends and
leaves no process running. AP forwarding behavior makes this absence
inconclusive; probe 32 stages tcpdump for a capture directly inside Linux.
The first host capture required explicit termination after the normal
signal was ineffective. The helper now has a bounded forced-stop fallback;
its second capture completes with a structurally valid empty pcap.

Probe 32's tcpdump cannot open because the minimal kernel lacks EVENTFD.
The capture error handler then executed `exit 1` in the PID-1 interactive
shell, causing an init-exit panic. This is a test-harness failure; the DHCP
attempt was not executed and no capture evidence was obtained. The next
image enables EVENTFD. `linux-command.py` now runs commands in a subshell
and returns the actual remote status; `init` also restarts exited consoles
while keeping PID 1 alive. Local exit/errexit checks preserve the parent.

Probe 33 boots with EVENTFD and validates the command guard on hardware:
`exit 7` returns host status 7, and the next command prints
`PARENT_SHELL_SURVIVED` and successfully initializes the firmware. WPA
completes again. A bounded native tcpdump records three DHCP DISCOVERs and
no replies, with zero capture drops; `inspect-n1-dhcp.py` verifies all three
IPv4 and UDP checksums, Ethernet/IP/BOOTP broadcast flags, consistent
transaction identity, and matching Ethernet-source/client MAC fields.
At capture end Alpha is 2, TX is 14 submitted/14 completed, coalesced=1,
failed=0, tx_bad=0; RX is 82 received/reposted with bad=0. Evidence:
`logs/linux-n1-dhcp-capture-33.log`. The private 1098-byte pcap is
`build/j813-centauri/re/firmware/n1-dhcp-native-33.pcap` under AuroraSilicon,
mode 0600, SHA-256
`28b696acd37845a14a3208986763814fd6377bfdb7c556030d445cf3320919b0`.
This excludes malformed DHCP packets at the Linux packet-socket boundary;
it does not establish their on-air framing or AP receipt. Investigate the
firmware's per-destination/broadcast TX ring metadata next. Keep probe 33
live for diagnostics; it has no active capture or DHCP client after the test.

Next controlled experiment (probe 34): use the same image and packet format,
but open normal TX TR 25 only after WPA completes. TX firmware's
`rings_mgr_open_tr` (0x10b014) calls `rings_mgr_update_encryptionMode`
(0x10b228), which snapshots the link's encryption flag into ring byte +0x22.
Opening before any link exists clears that bit. Later
`rings_mgr_lookup_and_assign_linkid_for_tr` (0x10c658) updates the link ID
without calling that encryption update directly. Other migration paths do
update it, so a stale encryption flag is a hypothesis to test, not yet a
proven DHCP cause. Reboot 34 retains EAPOL TX TR 24 and RX rings before WPA,
and delays only normal TX TR 25. Resident installation remains unchanged.

Probe 34 does not resolve DHCP: WPA completes with normal TX unopened, then
TR 25 opens successfully, but the native capture again contains only three
valid DISCOVERs and no replies. TX completes all 13 packets at the later
health check; all 69 received packets reach Linux, rx_dropped=rx_errors=0,
tx_errors=0 and Alpha stage 2. This rejects opening TR 25 after WPA as a
sufficient fix and excludes our receive-drop branch for this attempt.
Evidence: `logs/linux-n1-wpa-before-data-34.log`,
`logs/linux-n1-dhcp-late-ring-34.log`, `logs/linux-n1-health-34.log`.
Private pcap: 1098 bytes, mode 0600, SHA-256
`d23ad22d9afed681f37f58cf31519a69cbdcfa99c50d5bb5e10d7ca42794dc91`.

The remaining broadcast-ring hypothesis has a concrete host reference:
AppleWLANLogicalLinkSTA::attachIPCRings at 0x1803ce398 constructs a separate
MCBC transfer ring with an eight-byte parameter block containing interface,
AC 3, and six ff destination bytes (stores at 0x1803ce398 function body,
decompile lines 330-348). Linux currently opens TR 25 with all-zero ten-byte
clientdata. Follow its transformation through Airship before changing the
firmware ring extension; no broadcast-ring hardware test has run yet.

The transformation is now verified through AirshipIPCAdapter's factory
0x1809e6704 (copies the eight host bytes into options +0x58) and
AirshipDataTxTransferRing::initWithOptions at 0x180a0b3a4. The latter emits
ten clientdata bytes: 0, mapped hardware interface, TID, AC, destination MAC.
The pinned AC-to-TID table at 0x180a1e980 is [0, 2, 4, 6]; logical interface
0 maps to hardware interface 0 through 0x180a1e960. Thus the measured
broadcast extension is `00 00 06 03 ff ff ff ff ff ff`.

Probe 35 adds a default-off `data_tx_broadcast=1` module option that opens
TR 25 with that extension and submits only Ethernet broadcast frames.
Unicast and non-broadcast multicast packets are deliberately dropped in
this diagnostic mode. This permits DHCP DISCOVER/REQUEST and ARP broadcast
tests, but is not a general networking implementation or completion gate.
Keep normal TX creation after WPA as in probe 34 so the firmware ring
metadata is the changed variable. The W=1 build passes without diagnostics;
the native test is now complete. WPA reports COMPLETED, but the broadcast
ring still produces no DHCP reply: all three DISCOVERs complete with status
success and pass the native pcap checksum/framing checks. At the later health
check Alpha is 2, TX is 3/3 with zero errors, and all 352 RX packets are
reposted and delivered without errors or drops. The 15 TX drops are the
diagnostic's intentional suppression of other destination types. Broadcast
clientdata alone is therefore not a sufficient fix. Evidence:
`logs/linux-n1-dhcp-broadcast-ring-35.log`, `logs/linux-n1-health-35.log`.
Private pcap: 1098 bytes, mode 0600, SHA-256
`7bdd8fa9eb79d3300606f8ce4d3da0980eebd105a2acf9123589dfe5c89296c6`.
The RAM image now stages `capture-n1-dhcp.sh` as `/bin/n1-dhcp-test`.

Probe 35 also sends three broadcast ARP probes for the host Mac, with sender
IPv4 unspecified. All three captured requests have valid ARP/Ethernet fields,
but there is no reply. `probe-n1-arp.py` derives the host address without
printing it, retains raw output mode 0600, and leaves host networking alone.
The 198-byte private pcap SHA-256 is
`13837ded37d4e5f8b964f20ca78eb3f2e7079899cde48654d349b1ba9b5e0b8e`.
This broadens the failure beyond DHCP, without proving AP receipt.

After a successful WPA reconnect, DRAM logging initializes and recycles on
the same boot. Private captures 35a/b/c have matching target/local hashes.
The tighter ARP interval in 35c acknowledges the previous snapshot immediately
before transmission. The exact TX firmware call site 0x1140c9 identifies
host TR 25 and cookies 9/10 in
`tx_exchange_handle_winmove_common_and_ring_fp_messages` (0x113e20).
Their matching 0x11429b radio completions have raw status 0. Linux reaches
10 submitted/10 completed with no errors; RX reaches 901 with bad=0; Alpha
remains at 2. No ARP reply is received. These records prove that the test
packets reach the radio completion path, not that the AP accepts their
framing, encryption, or network payload. Inspect those next.

Capture 35c: 31 valid blocks, 115293 records; reject the inconsistent block
27 at offset 12204 in full. SHA-256
`dfc9ebd51fff16335497e4a50a7351e2d6dcbf796383ea2968cbf0bcd1444dca`.
The logger is full at consumer 58/producer 90; after retaining this capture,
the next safe acknowledgement is 87. No capture or ARP client remains live.
Evidence: `logs/linux-n1-arp-35c.log` (private),
`logs/linux-n1-dhcp-fwlog-35b.log`, `logs/linux-n1-completions-35.log`.

A subsequent three-request ARP probe targets the host's DHCP-provided router
instead of the host itself, to avoid relying on client-to-client forwarding.
It also receives zero replies. Alpha is 2, TX is 13/13, RX is 1315 with bad=0.
Raw output is private: `logs/linux-n1-arp-35-router.log`.

Static follow-up creates the previously missing Thumb function
`link_mgr_update_link` at 0x107dc0..0x1083b1 in the mapped TX project.
Input bytes +0x24/+0x25/+0x26 become link flags +0x256 bits 3/4/5. Changes
to those encryption flags update all attached transfer rings via 0x10b228.
Exact log site 0x1082a1 emits only a link identifier and three encryption
booleans; it is now whitelisted. No such record occurs in captures 35a/b/c,
so the runtime encryption flag is still unobserved. Do not infer disabled
encryption from its absence. Current next checks are that flag during WPA
and the host's exact packet/descriptor framing.

The 35d reconnect campaign retains five verified full snapshots and a final
snapshot. WPA completes with new handshake timestamps, but those snapshots
still miss its timer/result and encryption-enable records. Source inspection
identifies a diagnostic bottleneck: scan_worker/connect_worker hold wifi->lock
across their transactions, and fwlog reads/acknowledgements take the same
mutex. Thus the collector cannot recycle logs during the interval it needs.
Probe 36 separates a logger-only mutex (initialization takes command then
logger lock; reads/ack take only logger lock), and adds non-secret
connecting/scanning booleans to logger status. Debugfs removal still drains
operations before the controller frees the independent coherent buffer.
The W=1 build passes; hardware verification is next. No packet-format change
is included in this probe.

`capture-n1-connect.sh` retains snapshots in target RAM, compares each full
snapshot with the stopped mapping before acknowledgement, and saves a final
unacknowledged snapshot. Individual captures fit linux-download.py's 16 MiB
artifact limit; a combined 20 MiB archive was correctly rejected and no local
partial artifact was created. Downloaded 35d-0..4 and 35d-final all pass
target/local SHA-256 verification. Do not mistake the missing events for
evidence that encryption is disabled.

Probe 36 validates logging while scanning, then captures successful WPA and
the enabled data-encryption flag on link 2. See `N1-FIRMWARE-LOGGING.md` for
the exact reviewed sites and values. DHCP still receives no reply, with
3/3 successful TX completions and 499 received/reposted packets, bad=0.

The RAM-only `n1-packet-probe.c` sends three bounded ARP requests per case,
with sender IPv4 zero and no address claim. Cases are normal Ethernet II,
two FF padding bytes before Ethernet, and 802.3 LLC/SNAP. FF padding keeps
the broadcast-only diagnostic filter applicable; it is not claimed to be
valid Ethernet framing at the Linux boundary. All three cases receive no
ARP reply, and Alpha remains 2 with TX 12/12 and RX 1010, bad=0. Keep the
normal driver packet format unchanged. The host helper derives the router
address privately and exposes only counters. Private captures and logs:
`n1-arp-36-{ethernet,prefix2,llc}.pcap` in the ignored firmware RE directory,
and `logs/linux-n1-arp-36-{ethernet,prefix2,llc}.log`.

Build the packet helper with the builder's default AArch64 ELF alignment:
`cc -O2 -Wall -Wextra -Werror n1-packet-probe.c -o n1-packet-probe`.
The target kernel uses 16 KiB pages. An initial 4 KiB-aligned executable
faulted before main; the default 64 KiB-aligned executable loads and runs
all three experiments. Its SHA-256 is
`3e55992c508bbc1986c7b8b6945a552b69ec21f15fad58959c1f14f86f8bf136`.

Next source lead is address setup. Host `ACIStationAdapter::startInterface`
at 0x1809bce38 optionally appends TLV 386 with three six-byte addresses when
the supplied mloAddrArray flag +0x19 is set. Linux currently uses the measured
five-byte fixed payload without that optional TLV. Its requirement and
firmware fallback behavior are unproven; do not assume it explains the
failure or insert guessed addresses. The six-byte configure-interface
command shape itself matches host 0x1809bcc04. Airship's 0x1802eab80 also
confirms our descriptor's flags/length, 64-bit DMA address, cookie and
16-byte inline footer layout.

### Probe 36f–36m: controlled receive tests and station addresses

`n1-host-rx-probe.py` sends bounded UDP probes through the host's existing
en0 interface without changing its address, route, channel, or association.
The default test sends three Ethernet broadcasts and three Ethernet unicasts;
all use the IP limited-broadcast destination and a recognizable test marker.
The helper uses a separate non-promiscuous capture handle to retain only its
own test frames. Captures and address inputs stay in the ignored, mode-0600
firmware RE directory. Elevation credentials use osascript stdin only.

Tests 36f and 36g each capture only sequences 0–2 (broadcast) on the M5.
The 36g host capture independently contains all six frames, with the expected
broadcast/station destinations. The target records zero capture drops. A
direct-only repeat, 36h, captures all three transmissions on the host and
zero matching packets on the M5. This proves controlled broadcast delivery;
it does not prove the access point transmitted the missing unicasts over air.
Keep AP forwarding behavior and firmware receive filtering/decryption open.
CR 14 is a monitor completion ring, not a missing ordinary unicast queue;
do not reopen it speculatively (the earlier monitor experiment hit stage 3).

The host station-statistics printer at 0x1809d6bb8 identifies a shared header:
u32 isStaMLD, u32 active-link bitmap, then three 24-byte link records. Within
each link record, the peer MAC is at +5, local MAC at +11, RSSI at +17 and
noise at +18. The native 752-byte response has isStaMLD=0, bitmap=2; link
record 1's peer matches the currently associated BSSID, and its signal/noise
are plausible. Its local address differs from Linux's wlan0 address. The
Linux address and wpa_supplicant address agree and remained unchanged during
the tests. Comparisons output booleans only. Do not dump the raw statistics
header as generic counters: it contains private MAC addresses.

The counter portion differs from the 888-byte host ABI. For the direct-only
36h interval, words at 0x1bc, 0x1c4 and 0x1e0 increased by 200, and 0x1c8
by 88384. Counter names/stride are not yet established for this native ABI;
do not assign the newer host labels based on plausible values alone.

Address substitution alone is not sufficient. Test 36j targets the
firmware-reported address (host capture filename retains the earlier 36i
label) and still gets zero matching target packets. Tests 36k–36m use the
firmware address for both Ethernet source and ARP sender hardware address,
with Ethernet II, LLC/SNAP and prefix2 layouts respectively. Each sends three
router ARP probes with sender IP zero and receives no reply. Retained pcaps
verify all nine intended probes used the firmware address in both source
fields. The kernel's normal packet format/address behavior is unchanged.

`n1-packet-probe.c` accepts an optional `firmware` source argument, reads the
root-only station-statistics node, and validates the exact native response
and a single non-MLD link before using that local address. The host ARP helper
exposes this as `--firmware-source` with `--wire-format`. The native helper
build passes `-Wall -Wextra -Werror`, retains 64-KiB ELF load alignment, and
was uploaded with SHA-256
`05d7ed705bcc9cbca0f33e7d96747948c4bcd6d74ba2f696838add4727ba77cf`.

Private evidence, all paths relative to the ignored firmware RE directory:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| n1-host-rx-36f.pcap | 309 | dbf7d88a7f0638de015e68e0db245d45961c24e0069c5b32a48c5bee748b168e |
| n1-host-rx-36g-host.pcap | 594 | d2b9a06a33da3c1fb8d666be3f658af1447f14f8631497f712bf3fe3c284e332 |
| n1-host-rx-36g.pcap | 309 | ab6bb437f44bcceb3ba21d055ac0f8743823fbf6e6f0cc34b53e70c6371073d6 |
| n1-host-rx-36h.pcap and n1-host-rx-36j.pcap | 24 each | c6d5b6f39aa334ca0ef3e27b3cd8cf6a82e453275c16d5fa2f6397a63bb5506f |
| n1-arp-36k-firmware-source.pcap | 256 | 53f2e678d82b6d246ad3a7e1298f21651c5dbdb87e4575ff86c1db6bc9512f92 |
| n1-arp-36l-firmware-llc.pcap | 324 | 6f9bcd9489ad199826da837b31fe137df2931110f70ff44b87d61a1319742abb |
| n1-arp-36m-firmware-prefix2.pcap | 320 | bc60f5435c56d80d2b9edf178c4448669ad19480cca055df067ea3e8159ba995 |

Latest live check: WPA COMPLETED, Alpha 2, TX 21 submitted/21 completed,
RX 5204 received/reposted, bad=0, no IPv4 address. All bounded tests have
finished. Preserve the live probe36 image; no new kernel was booted here.
Next investigate why interface configuration and the reported link address
disagree, alongside the ordinary-data link and receive routing. The missing
directed probes do not identify a particular encryption or routing defect.

Current Alpha DMA window reservations (offsets from its 16 MiB allocation):

| Region | Start | Exclusive end |
| --- | ---: | ---: |
| TX completion entries | 0x200000 | 0x210000 |
| RX completion entries | 0x240000 | 0x241000 |
| EAPOL transfer entries | 0x500000 | 0x501000 |
| RX transfer entries | 0x520000 | 0x520800 |
| Normal TX transfer entries | 0x540000 | 0x541000 |
| Firmware scratch (owned by device) | 0x800000 | 0xd00000 |
| Normal TX payloads | 0xd00000 | 0xd40000 |
| Posted RX payloads (127 buffers) | 0xe00000 | 0xe7f000 |
| Existing bounded EAPOL probe payloads | 0xf00000 | 0xf00200 |

Control/management descriptors and buffers also occupy lower offsets; the
table above names the packet regions and the critical firmware reservation.

The root-only binary `data_rx_snapshot` has little-endian u32 version/count,
followed by eight entries of 32-byte completion plus 4096-byte packet storage
(33032 bytes total). Unused packet bytes stay zero. Download with
`linux-download.py` into the ignored private RE directory; never print frames,
addresses or payloads. Decode only protocol/length validation aggregates.

## Required clientdata extension (2026-09-08)

UMAC code in the retained `aciw.bin` is mapped at file offset + 0x100000.
The firmware log catalog identifies `htm_acipc_open_data_cr_CB`, whose
implementation starts at 0x117548. It asserts at 0x11760e when the callback
argument's u32 at +0x2c is zero. That field is **clientdata length**, not a
pointer: `FUN_00176140` sets it from message length minus 0x2c, and places
the pointer to bytes following the 44-byte header at callback argument +0x30.
The callback consumes extension byte 1 as the hardware interface index.

This supersedes the old claim in AppleCentauriCommon/TASKS.md that data
rings are rejected unconditionally and the host message shape is exhausted.
Every recorded failing probe omitted the required extension. Hardware
validation of the corrected request is pending.

Apple's WLANACIAdapter confirms a ten-byte extension:

- AirshipDataRxCompletionRing::initWithOptions, 0x1809a50e0: constructs the
  extension at private state +0x66 and stores length 10 at +0x70.
- AirshipDataTxCompletionRing::initWithOptions, 0x180a06174: constructs it at
  private state +0x68 and stores length 10 at +0x74.
- Byte 0 is zero; byte 1 is toHardwareIndex(interface); byte 2 maps the link
  type through [0, 2, 4, 6]; byte 3 preserves the link type; bytes 4..9 copy
  the interface's remaining six bytes.
- toHardwareIndex(0) is 0 (table at 0x180a1e960).

An explicit `data_probe` debugfs write now tries an empty CR with this
extension. Value 76 selects the TX completion ring (index 192, 2048 slots);
14 selects monitor completion (index 206, 256 slots). These use separate
reserved DMA regions, a 16-byte completion footer, and no posted packet
buffers. Each probe can run once per firmware boot. The ordinary scan path
is unchanged. Header routing fields remain under investigation; ring-open
acceptance alone is not proof of packet transport.

### Native probe 10

The TX CR (76, index 192) completed its open request and remained at Alpha
stage 2 after 100 ms. A subsequent monitor CR (14, index 206) saw stage 3;
the next scan failed. This proves that adding the extension can pass the
initial TX ring-open boundary, but does not yet prove sustained operation.
Evidence: `logs/linux-n1-data-cr-10.log` and
`logs/linux-n1-data-cr-regression-10.log`. The next probe adds the plist's
doorbell and interrupt vectors and checks each open after one second.

### Transfer-ring size correction

The firmware TR parser at 0x171c48 reads size from wire offset 0x18. It
overwrites wire offset 0x1c with the ring's index for software control rings.
Apple's host builder at 0x1802e72b8 also places its configured ring size
(controller +0x234) at wire +0x18. The native driver previously put 0xffff
there and the intended size at +0x1c. This can explain why bounded scans
worked but wrapping the receive queue failed: firmware had a different ring
size. Correcting the size is pending hardware validation; recycling is still
disabled until that validation passes.

### Native probe 11

With the ten-byte extension and explicit doorbell/interrupt vectors, CR 76,
TR 24, CR 15 (receive group 12), and TR 12 each opened and stayed at Alpha
stage 2 for a one-second check. Scanning after TX-ring open completed with
24 channels and 88 beacons; the WPA scan after all four opens also completed,
reaching 48 channels and 158 beacons cumulatively. The transfer-size fix is
therefore compatible with native control traffic.

Association still succeeded, but Alpha then aborted before ConnectComplete.
The receive ring had no buffers and incorrectly requested a 16-byte transfer
footer. Probe 12 removes that footer, posts 127 separate 4096-byte RX buffers
in the reserved DMA window, and exposes completion metadata through read-only
debugfs. Opening rings alone has not proven packet DMA or authentication.

Evidence: `logs/linux-n1-data-tx-11.log`, `logs/linux-n1-data-rx-11.log`,
`logs/linux-n1-wpa-data-rings-11.log`.

### Native probe 12 and queue exhaustion

127 RX buffers were posted and firmware stayed at stage 2. Two WPA attempts
associated and reported timeout 362. A third scan reached exactly 191
management frames cumulatively, then firmware aborted. No data RX completion
was produced. The earlier association-time abort is therefore not sufficient
evidence that opening the data rings caused the failure; bounded control
queues are a demonstrated confounder. Probe 13 reuses RX slots and wraps the
command queue with the corrected advertised ring sizes.

Evidence: `logs/linux-n1-data-buffers-12.log` and
`logs/linux-n1-wpa-data-buffers-12.log`.

### Firmware supplicant bank

The supplicant code range uses VM = file offset + 0x3dd000. Five independent
assertions from the firmware log catalog match this mapping exactly. The
extraction helper creates `aciw-bank-500000.bin` from file offset 0x123000.
This broad analysis image is valid for the supplicant range only: other
firmware ranges have different relocation deltas and must be mapped using
their own catalog assertions before decompiling them. The packet-send range
0x4fc000..0x508000 uses delta 0x3db000 (all 138 assertions agree) and is
separately extracted as `aciw-code-4fc000.bin`.
The timeout handler starts at VM 0x52720c and writes status 362 at supplicant
state +0x2a82. It calls 0x5290f4, which exports 40 bytes beginning at state
+0x2a84; state byte +0x2a88 tracks handshake progress. The EAPOL response
builder at 0x5291e4 sets that byte to 3 for message 2 and 6 for message 4.
Tracing how these diagnostics reach ACI may distinguish missing message 1
from failure after message 2 without exposing keys.

Firmware and calibration remain private ignored build inputs. No credentials
or firmware payload bytes belong in these notes.

### Native probe 13: queue reuse proven

Ten consecutive scans completed successfully: 240 channel completions and
757 beacon reports, firmware stage 2 throughout. Both command and receive
queues crossed their wrap points. All four data rings then opened and 127
RX buffers were posted. Repeated WPA attempts still associated and timed out
with status 362, while management reception exceeded 1,000 frames without
the previous queue-exhaustion abort. Packet completion indices stayed zero.

Evidence: `logs/linux-n1-scan-wrap-13.log`,
`logs/linux-n1-data-buffers-13.log`,
`logs/linux-n1-wpa-data-buffers-13.log`.

WLANACIAdapter event dispatch at 0x1809dd860 routes event 0x50 to
0x1809e0cc4. Its 64-byte fixed layout contains result u32 at 0x18, phase u8
at 0x1c, and five timestamps at 0x24 through 0x34. This matches firmware
0x5290f4's 40-byte diagnostic payload. Probe 14 logs only these fields.

While probe 14 uploaded, host `connectAp` analysis found another omitted
field: payload +0x48 is the ACI WSEC cipher, set through `mapRSNtoCipher`.
The supported CCMP suite maps to 4. The native command had left it zero.
The updated module sets 4 and retains the phase diagnostic. Native probe 14
associated repeatedly, confirmed the expected PMK, and reported result 362,
phase 3, with only the first two handshake timestamps populated. No data
completions arrived and firmware remained healthy. This proves firmware
receives message 1 and reaches the message-2 builder, but does not yet prove
that message 2 went on air. Evidence: `logs/linux-n1-wpa-cipher-14.log`.
Firmware phase 1 begins near 0x527900, phase 2 is set by the
message-1 handler at 0x52797c, and phase 3 by its message-2 response builder.

## Native transmit DMA proof (probes 15–16)

Probe 15 accepted ACI command 7 mode 3 (power save off), and opened normal
TR 25. WPA still ended in phase 3/result 362 with healthy firmware.

Probe 16 submitted one 18-byte Ethernet EAPOL-Start through TR 24, with
descriptor flags 3 (DMA plus a 16-byte WLAN footer). Hardware advanced
TR tail 24 from 0 to 1 and CR head 192 from 0 to 1, remaining at stage 2.
This establishes data transmit and completion DMA; on-air success is not
established. The completion has eight words:
`00180502 00000001 00000100 0fff2003 5fe2a606 00000000 0179a53d 00000000`.
Evidence: `logs/linux-n1-tx-probe-16.log`.

WPA after this probe still timed out; 2.4 GHz testing also timed out, at
phase 1. No receive data completion arrived. The statistics command does
return TLV 0xf928, but its native payload is 752 bytes, while the analyzed
host helper expects 888. Probe 17 accepts either measured length and adds a
bounded optional EAPOL-Start immediately after association. It remains
disabled by default and allows at most eight independent buffers per boot.

The verified supplicant completion handler at 0x52e1a0 sets phase 4 and
timestamp +0x2a98 for message-kind 1, regardless of the low-three-bit TX
result. Phase 3 with that timestamp still zero therefore precedes this
callback; it is not simply a recorded negative TX result. The receive-M3
handler at 0x527d90 sets phase 5 and timestamp +0x2a9c. These addresses use
the separately verified supplicant mapping delta 0x3dd000.

Probe 17 validates the 752-byte counter record and four independent host TX
completions. It does not validate association-time transmission: the saved
log shows the supplicant timeout before the counter query returns and the
probe submits. Probe 18 moves submission into the association-event handler,
before another synchronous ACI command can delay it. Counters are separate.
Evidence: `logs/linux-n1-wpa-17.log`.

Probe 18 also corrects the all-zero MLD-global TLV 376. The host initializer
at 0x1809c4b0c emits `01 00 01 ff ff ff ff ff ff ff ff 03 00 00` before
candidate overrides. This is a measured command mismatch, not yet a proven
cause of the WPA timeout. Test with the optional EAPOL probe disabled first.

Probe 18 leaves the same phase-3 timeout with those MLD defaults. Its
association-event probe submits before the timeout and completes DMA twice,
without resolving authentication. Evidence: `logs/linux-n1-wpa-mld-18.log`,
`logs/linux-n1-eapol-assoc-18.log`. The first completion's word at +12 is
0x040d2003; the second is 0x0fff2003.

TX completion ABI clarification: the firmware TX status is the low byte of
the **inline word at CR descriptor +12**, not footer byte zero. Airship's
`cr_controller::sync` forwards that word into the transfer completion;
`save_transfer_completion` stores it at queued-transfer +0x44.
`get_transfer_completion` returns it as the high word of its +8 field;
`direct_tr_controller::dequeue_batch` copies that to the batch entry's
completion-word pointer (+0x68). `AirshipDataPacket::initializeMetaData`
points this at packet-private +0x20. The timestamp is in the footer at +16.
Thus the native probe reports firmware TX status **3**, mapped by host
`mapFwTxStatusToHostTxStatus` to result 4 / 0xe0018200 (non-success).
The earlier completion-DMA proof stands; it did not establish radio success.

Probe 19 adds read-only country status (ACI command 10) and forwards explicit
cfg80211 country updates to command 9 with `{alpha2[0], alpha2[1], 1}`.
It does not select a country automatically or bypass power/SAR controls.
Native probe 19 read `58 5a 00` (XZ) initially while cfg80211 reported world
domain 00. `iw reg set US` invoked the driver callback successfully; native
readback became `55 53 01` and cfg80211 reported US/DFS-FCC. This validates
country synchronization, not WPA. Evidence: `logs/linux-n1-country-before-19.log`
and `logs/linux-n1-country-us-19.log`.

The subsequent US-country WPA test still times out in phase 3; country
synchronization did not resolve the transmit blockage. Network retries were
disabled and the native shell and all three firmware functions remained live.
Evidence: `logs/linux-n1-wpa-us-19.log`.

The separate TX-core image `wftp` is now extracted for analysis. Its direct
VM = file offset + 0x100000 mapping matches all 958 TX assertion sites in
the matching firmware catalog. SHA-256:
`6d05b7d5d1e16742478e2155ef87130e21bf91fe0bf4355f0ee5e7d827bf954f`.
Use project `j813-n1-tx` and Thumb mode; this is distinct from UMAC `aciw`.

Probe 20 successfully queries interface status (command 20). The first parser
incorrectly began TLVs at +24. The host cursor skips a fixed 32-byte response;
probe 21 corrects that and identifies idle TLV 380 with 26 bytes. A missing
TLV 377 is not proof that the interface has no link. MLO-specific decoding
remains unvalidated. Probe 20's WPA retries still report phase 3 / result 362.

Probe 21's optional `fw_debug` ring 6 opens and initially remains at stage 2,
but the subsequent scan finds Alpha at abort stage 3. It returns no completed
trace buffers. This experiment does not diagnose the WPA failure. Its trace
interface has been removed from the driver; do not repeat the incomplete
logger setup. Evidence: `logs/linux-n1-start-21.log` and
`logs/linux-n1-wpa-trace-21.log`. Recover before further Wi-Fi commands.

The next build adds explicit, separately selectable macOS reference TX setup:
`tx_config` action 1 sends command 43 with four 2,000,000-microsecond packet
lifetimes (host reset defaults); action 2 sends command 52 with TLV 340/10,
the fixed control profile 1. Command 56 reads profiles 0 and 1 without changing
them. The often-misread value 0xa0154 is the TLV header, not a command ID.
Probe 22 accepts both setters. Profile 1 reads back rate 0x800, RTS threshold
65535, retries 255/255, type 1, ID 1; profile 0 remains rejected. Both the
lifetime-only and subsequent control-profile WPA tests still stop at phase 3.
The association-time EAPOL-Start probe with profile 1 completes DMA, with no
authenticated connection or RX completion. Evidence:
`logs/linux-n1-wpa-lifetime-22.log`, `logs/linux-n1-wpa-profile-22.log`,
`logs/linux-n1-wpa-control-eapol-22.log`. Reference functions:
`AppleWLAN80211STA::resetTxPacketLifetime` 0x18037fe64,
`ACISystemAdapter::setTxPacketLifetime` 0x1809ef4f0,
`ACIStationAdapter::configureTxDataCtrlPacketProfile` 0x1809bd7e8.

Probe 23 adds write-only `host_wakeup` (write 1). Host `hostWakeup` at
0x1809fa800 uses ACI command 6 with state 0 at request +26. Firmware
`SysCmdSetHostPowerState` at 0x570f08 accepts a fixed 27-byte request with
no TLVs; this test retains the current event filter. Do not copy the newer
host's TLV 280/92 blindly: this firmware accepts a maximum 86-byte event list.
This is separate from station 802.11 power-save command 7. Native probe 23
accepts host wakeup with status 0 and remains at stage 2. WPA still stops at
phase 3, both with wakeup alone and combined with the TX defaults. The
association-time host packet's completion word is 0x07f02003 (status 3),
and the RX ring has no completions. Thus omitted host wakeup does not by
itself explain the blockage. Evidence: `logs/linux-n1-start-wakeup-23.log`,
`logs/linux-n1-wpa-wakeup-23.log`, `logs/linux-n1-wpa-awake-profile-23.log`.

Probe 23 also tested a forced 2442 MHz candidate. The supplicant log confirms
that frequency; WPA still stops in phase 3. Evidence:
`logs/linux-n1-wpa-2ghz-23.log`, `logs/linux-n1-2ghz-selection-23.log`.
The matching firmware connect handler at 0x574000 confirms the 154-byte
fixed request, candidate/channel placement, AKM +0x5f, cipher +0x60 and
TLVs beginning +0x9a. It copies the first 12 bytes of MLD tag 376.

Probe 24 adds two independent, default-off packet diagnostic switches:
`eapol_unicast` addresses EAPOL-Start to the associated AP; the earlier
probe always used the PAE multicast address. `eapol_relative_lifetime`
uses the measured AirshipDataPacket::setRelativeExpirationTime encoding:
mode 6 in metadata bits 29:27 and 2,000,000 microseconds in word 2.
Do not assume host-packet rejection and missing firmware M2 completion have
the same cause. Native probe 24 receives four unicast TX completions with
status 3, no RX completions and the same phase-3 WPA timeout. With relative
expiration enabled, the fifth descriptor completes DMA and WPA still times
out; the old diagnostic only prints the first four TX records, so the fifth
TX status has not been observed. Evidence: `logs/linux-n1-wpa-unicast-24.log`
and `logs/linux-n1-wpa-unicast-expiry-24.log`.

Probe 25 displays all eight bounded TX completion records and consumes
`eapol_after_assoc` after one association-event probe. Re-enable that switch
explicitly for each packet test; supplicant retries no longer spend the
remaining diagnostic buffers automatically. Repeat the relative-expiration
case before interpreting its TX status.
Native probe 25 verifies that case: slot 0 is unicast with relative
expiration, its completion word is 0x0abe2003 (status 3), and the RX ring
remains empty. WPA still reports phase 3 / result 362. The one-shot switch
reads N after the single packet despite subsequent supplicant retries.
Evidence: `logs/linux-n1-wpa-unicast-expiry-25.log`.

Firmware mapping improvement: use `map-n1-firmware.py` and Kawaii project
`j813-n1-mapped`, binary `aciw-mapped.elf`. The image's own `fwsg` table has
37 entries at file +0x276800 and its footer at +0x276ca0. This verifies 2,425
UMAC assertion sites, excludes 36 ROM-only sites, and identifies the remaining
catalog entry as the crash-handler Thumb pointer. Ghidra imports 5,147
functions; catalog annotations name 1,047 unambiguous functions. The helper
also writes a private `aciw-assertions.json` for annotations.
The old debug command 0xf0000 history belongs to the TOF/ranging region,
as shown by its writers in __TOF. It is not an FGPS or WPA trace facility.

### Follow-up analysis after probe 25

The TX firmware now has its complete three-segment map too:
`map-n1-firmware.py --image wftp` verifies all 958 TX catalog assertions.
Use Kawaii project `j813-n1-tx-mapped`, binary `wftp-mapped.elf` (822 functions,
330 unambiguous catalog names). The source SHA is pinned in the helper.
Both mapped ELF files are private analysis artifacts, never boot payloads.

The host TX transfer-ring client extension is confirmed against
WLANACIAdapter 0x180a0b3a4: version, mapped interface, mapped link type,
raw link type, then six peer-address bytes. The link-type table at
0x180a1e980 is [0, 2, 4, 6], and hardware interface 0 maps to 0.
The initial all-zero extension therefore matches the default option values;
this comparison did not identify a new ABI correction.

The firmware-generated EAPOL path is independent of the host data TR:
FGPS_DispatchEngines (0x4ffa5c) allocates stream type 0 from pool 5;
htm_acipc_host_ctx_CB (0x116b64) seeds one pool-5 WQE.
udisp_htm_pull_core_buffers (0x117fa0), case 5, sends it through
0x11a4d4 to ctm_send_core_buf (0x118dfc), then returns the WQE.
The CTM sender can defer on a full internal ring. This is a static path
description, not proof of the runtime stall or of resource exhaustion.
Do not add an arbitrary host service ring to supply that internal pool.

The mapped SHA-1 descriptor at 0x438ff8 points to the software compression
function at 0x55aeb8. A missing hardware SHA-1 engine setup is not supported
by this path. Authentication still has no M2 transmit callback; host status
3 remains a shared failure category. No new hardware fix was made during
these analysis checks. Probe 26 subsequently added hardware-validated DRAM
logging without changing the failed WPA result; see `N1-FIRMWARE-LOGGING.md`.

Captures 27c/27d supersede the earlier lack of internal completion visibility:
the actual M2 is built and submitted, then completes with failure in CTM
(flow 3, tag 6, 141 bytes). TX raw status 5 is a mode-0 radio queue flush,
mapped to UMAC status 3. The supplicant M2 completion timestamp is still zero.
This explains the completion category but not why the packet is flushed;
queue scheduling and the flush requester remain under investigation.

## Probe 37: RX crypto counters and normal queue metadata

The host `ACIStatisticsAdapter::printRxChipCounterStats` at `0x1809d7b80`
uses statistics type 30. `getStatistics` at `0x1809ca6f4` sends command
`0xb0001`, a 22-byte payload containing LE64 BIT(30), byte 8 equal to 1,
and TLV 363 with nine zero scope bytes. The table at `0x180a195e0` maps
type 30 to response TLV `0xf921`, length 64. The new root-only read-only
`rx_crypto_stats` debugfs node validates response status, tag, bounds and
exact record length before exposing sixteen LE32 counters.

Probe 37 returns `rc=0 valid=1 size=92 tag=0xf921 length=64`. Before WPA all
counters are zero. After WPA and after failed DHCP, all ten duplicate,
TKIP/CCMP replay and decryption error counters remain zero. PHY receive
count advances from 45825 to 88695 during that interval. DHCP returns 1,
TX is 3/3 without errors and RX is 75/75 without malformed completions.
This excludes a recorded increase in those errors during this attempt; it
does not establish that the AP sent a reply or that all data keys are correct.
Evidence: `logs/linux-n1-crypto-baseline-37.log` and
`logs/linux-n1-dhcp-crypto-37.log`.

The revised capture collector completes through WPA on this hardware boot.
Its retention/checksum/ack sequence no longer compares a whole changing live
buffer. The final partial capture is retained without acknowledgement.
See `logs/linux-n1-connect-trace-37.log`. Do not acknowledge subsequent
logger blocks without first preserving the corresponding capture.

The normal STA base queue's zero destination metadata is supported by
`AppleWLANLogicalLinkSTA::initWithOptions` at `0x1803cdf74`: it zeroes the
options and varies the access category. TX queue initialization at
`0x1803a7920` copies the six option bytes at +0x14 into private +0x8c/+0x90;
`attachIPCRing` at `0x1803a8b1c` passes them to IPC. Separate destination
flow queues exist, but they do not justify substituting the AP BSSID into
the base AC0/TID0 extension.

Private probe37 statistics and the Linux address were downloaded with
matching checksums. The reported firmware local address remains different
from Linux's; both change from probe36. The reported firmware address is
unicast and locally administered in both boots. Cause remains unproven.
`logs/linux-n1-health-37b.log` verifies WPA COMPLETED, Alpha 2, TX 3/3,
RX 1127 with bad=0, carrier 1 and no IPv4 address. The diagnostic remains
broadcast-only, so general connectivity is not claimed.

Build evidence: module SHA-256
`f19ec5dfa81be8ae556e40b8241191cfb7376b1530ecfa6e5c0d2f368e0338f7`,
initramfs SHA-256
`08e0b6c37fe778e884d3ba9c88768c06d36029b37fed07a9f2686d18b301233a`.
The W=1 module build passes. Native boot evidence is
`logs/linux-wifi-boot-37.log`. No normal packet handling changes were made
for the crypto counter experiment.

## Probe 38: normal queue, firmware source, and host ARP observation

The image includes the bounded native packet probe under `/bin`; its two
ELF PT_LOAD alignments are 64 KiB, compatible with the 16 KiB kernel. The
host ARP helper prefers that binary and falls back to `/run` for older boots.
The module remains identical to probe37; only `data_tx_broadcast=0` changes
the normal queue experiment. WPA repeats COMPLETED, Alpha stays at stage 2.

Firmware-source ARP probes 38a (Ethernet), 38b (LLC/SNAP) and 38c (prefix2)
each send three requests to the router and receive no replies. TX reaches
13/13, 17/17 and 21/21 respectively, with no malformed RX or TX completions.
DHCP38 also fails: TX25/25, RX426/bad0, no TKIP/CCMP replay/decryption errors,
and two duplicate RX frames. General TX mode does not resolve connectivity.
IPv4/IPv6 global address counts, IPv6 default routes and neighbours are zero.

`n1-host-rx-probe.py --mode observe-arp` opens a non-promiscuous, nonblocking
en0 capture and creates a protected readiness marker after successful setup.
It injects nothing and retains only ARP involving the provided Linux or
firmware station address. The 20-second observer precedes native probe38d:
three firmware-source Ethernet ARP requests for the host Mac, sender IP zero.
The native PCAP confirms all three requested frames have the intended source
MAC, destination IP, broadcast Ethernet destination and ARP fields. The host
capture is a valid 24-byte PCAP with zero records; native replies are zero.
TX31/31 and RX728/bad0 remain healthy. This is not an on-air observation and
does not establish whether the AP received, accepted or forwarded the frames.

Private evidence: `n1-host-arp-38d.pcap` plus five files
`n1-evidence-38.part-0` through `n1-evidence-38.part-4` under the ignored
firmware RE directory. Concatenating the parts in order yields an 8783500-byte
gzip tar archive with SHA-256
`11489b1538973ecfd2560becb7b1c8869ec9b25ac4ec7a5df45d2ae3547b28c8`,
matching the target. The archive contains nine 4 MiB logger snapshots,
crypto/identity records and all four ARP captures plus DHCP. All eight full
snapshot checksums and the final partial snapshot checksum also verify.
The original full archive download failed decoding; no corrupt artifact was
accepted. The target archive passed gzip validation. Since the RAM rootfs
does not include `split`, `dd` produced five bounded pieces for transfer.

The next module adds a read-only `dot11_mib_stats` node. Host
`printDot11MibStats` at `0x1809d7fb0` requests type25, scope2/interface0.
The interface mapping at `0x180a1e960` maps host zero to firmware zero.
Request command is `0xb0001`, payload22: LE64 BIT(25), byte8=1,
TLV363/length9 at offsets9/11, LE64 interface0 at13, scope2 at21.
Response TLV is `0xf91b`, length192, with 24 LE64 counters. Native
`FUN_0011fb48` uses size-field base `0x180770` and 32-byte strides;
type25 entry `0x180a84` confirms combined tag/size `0xf91b00cc`
(192 data bytes plus 12 firmware-internal header bytes).

Counter indices0..6 are RX pushed, pushed-multicast, MPDU, duplicates,
AMPDU duplicates, MPDU error drops, and MSDU filter drops. Indices7..15 are
nine RX hole counters. Indices16..23 are TX pulled, pulled-multicast,
pulled-core, success, abort-request, peer-gone, retries-exhausted and
lifetime-expired. The names follow the host printer; success is not promoted
to independent proof of AP acknowledgement. The parser validates status,
tag, exact size and bounds before exposing counters. W=1 build passes.
Probe39 subsequently validates the request below.

## Probe 39: native MIB outcomes and actual TR encryption flag

Native firmware returns `rc=0 valid=1 size=220 tag=0xf91b length=192`.
Before connection all 24 counters are zero. After successful WPA and TR25
opening, RX delivered=29 matches Linux RX29 and TX pulled=8 matches native
TX8/8. Firmware-core TX=66 and success=70/abort-request=4 account for the
combined 74 host/core packets at that checkpoint. This validates useful
counter correspondence, not an on-air capture.

Firmware-source ARP39a still returns no reply. Across its observation window,
host TX pulled rises by five and core TX by two; success rises by seven.
Abort, peer-gone, retry-exhausted, lifetime-expired and RX filter drops do not
increase. Background packets are included, so the seven successes are not
all assigned to the three explicit ARP probes. Evidence:
`logs/linux-n1-mib-39a-result.log` and
`logs/linux-n1-arp-39a-normal-firmware.log`.

The bounded DHCP39b interval is cleaner: host TX pulled +3 and success +3,
core TX unchanged. No abort, peer-gone, exhausted retries, lifetime expiry
or RX filter-drop increase occurs. RX delivered +67, MPDU +483, duplicate
MPDU +224 and RX error-drop +2 are recorded. Crypto statistics after the
interval show two multicast CCMP replay errors and zero other replay or
decryption errors. The two drops are not identified as DHCP packets. DHCP
still returns 1; WPA remains COMPLETED, Alpha 2, TX18/18, RX354/bad0.
Evidence: `logs/linux-n1-mib-dhcp-39b.log`.

`rings_mgr_open_tr` at `0x10b014` obtains a link through `0x109f44`, writes
the link ID at ring+0x17, and calls `rings_mgr_update_encryptionMode` at
`0x10b228`. That function selects link encryption bits3/4/5 according to
ring type/state and records the selected boolean at ring+0x22 bit0.
Link updates at `0x107dc0` also propagate changed encryption booleans to all
rings assigned to the link. Merely opening the ring after WPA does not
statically bypass this update.

The reviewed four-argument log at call `0x10b1dc`, source4/site`0x10b1e1`,
reports ring IDs, AC/index, state/encryption and multicast/EAPOL booleans.
No address or key is included in those arguments. Decoder now permits that
specific site. Private `n1-fwlog-39-post.bin` has 31 valid blocks and 119527
records, consumer203/producer235. It contains one matching record:
`ring_ids=0x190001 ac_index=0x190000 state_encryption=0x10000
multicast_eapol=0`. This proves the normal TR25 opened with its encryption
boolean set. It does not prove correct headers, key selection or encrypted
payload acceptance by the AP. These remain the next trace targets.

Private post-capture SHA-256:
`5080e7d3551d1c0907089265c19a6b90c7dcace5f84f2407a46eda307f615b18`.
The seven-file MIB/ARP/DHCP archive `n1-evidence-39-small.tar.gz` is 836 bytes,
SHA-256 `e97209eb282b66c981b9e7c42d997b72cf1e015f8834252bf6c482f90c3c29b0`.
Module SHA-256:
`7098bcfcf99d1231f49391147c2e803e80b3d1a412947e6f710e17cd557fbdf6`.
Initramfs SHA-256:
`0fd8f43bea3cbc076d6b578ee49d7f9cee46991f087e0e2de7c1b7c27cc473fd`.
W=1 build and native boot are verified. No normal TX/RX behaviour changes
were made for this instrumentation.

### Complete DHCP client identity comparison, 39c/39d

The bounded C helper gains a DHCP DISCOVER mode: three transmissions, no
DHCPREQUEST/lease acceptance and no interface address changes. It uses a
random transaction ID, 300-byte padded BOOTP/DHCP body, correct IPv4 and UDP
checksums, broadcast flag and destination, and client ID type1/MAC. It
recognizes replies only with matching transaction ID/chaddr, correct BOOTP
shape, valid IP/UDP checksums and bounded options; offers require message2
and a nonzero offered address. The response fields are never printed.

Probe39c uses the firmware-reported station MAC for Ethernet source, chaddr
and client ID. Probe39d uses the Linux netdev MAC in all three fields. Both
send three DISCOVERs and receive zero replies/offers. Independent parsing
of both native PCAPs validates exactly three 342-byte DISCOVERs per case,
IP/UDP checksums, sender/destination addresses, requested broadcast reply,
client ID and message type. Each case uses one transaction ID. The two
station addresses still differ. Address replacement only in the transmitted
packet and DHCP fields does not restore a reply.

The updated helper compiles with `-Wall -Wextra -Werror`, has two 64 KiB
PT_LOAD alignments, and is uploaded with checksum verification to
`/run/n1-packet-probe`. Its SHA-256 is
`a012f80db899a44fae382eb96e232f9a81fdeedab11fd291dae0ba7ded0eae76`.
The host wrapper now prefers `/run` for an uploaded experiment and falls
back to the packaged `/bin` helper; `--wire-format dhcp` selects a bounded
UDP67/68 capture and prints a DHCP result marker.

Private identity/DHCP archive `n1-evidence-39-address-dhcp.tar.gz`, 729 bytes,
SHA-256 `1e46696ef5e47b432cba9a9788d8910cb56828ed3d87ad6397aa56c495ae20d8`,
retains both captures and current firmware/netdev/WPA identity records.
The full connect39 archive is retained as four `n1-evidence-39.part-N`
files. Their concatenation is 7831404 bytes with SHA-256
`4abf527c82dcb463f2de9432351490f2ae0afa8a8d6105ae8668d649900f074a`,
matching the target; all seven full snapshot checksums and the final
snapshot checksum verify.

Host descriptor flag3 is also supported directly by Airship's enqueue
path: flag1 denotes mapped payload memory, flag2 denotes inline header or
footer metadata. A DMA packet with the 16-byte footer therefore sets both.
That evidence does not support changing normal data packets to flag1.

Next traces: packet-header construction, key selection and the host's
post-WPA handling. Symbol lookup finds
`AppleWLAN80211STAEventHandler::handleSupplicantdEvent` at `0x1803a3ed4` and
`ACIConnectionAdapter::setKey` at `0x1809c60c8`; no explicit STA controlled-port
setter was identified by the symbol-name search. That negative search does
not rule out an authorization step elsewhere. No speculative key or
authorization command has been sent.

## Post-WPA trace and probe 40: transmit-policy comparison

The bounded post-WPA host trace does not reveal a separate STA controlled-port
command. `handleSupplicantdEvent` 0x1803a3ed4 forwards host notification0x4c.
`handleConnected` 0x1803a34b4 reaches `handleConnectionCompleteEvent`
0x1803e1068. Its success call at 0x1803c619c is `configurePSWakeSiso`, which
submits power options `[1,1,2]`; it is not a port-authorization setter.
`notifyConnectionCompletion` 0x1803e0de4 and the associated-event path produce
host link/channel notifications. This only bounds these immediate paths.

LMAC `LINK_MANAGER_encryption_set` 0x10f39c logs link/previous/requested mode
at reviewed source3/site0x10f3d3. The key configuration handler 0x11a71c maps
CCMP cipher4 to mode2. Three deduplicated events in connect39 snapshot1 set
link2 from0 to2 and context34 from0 to2, then2 to2. No key bytes are decoded.
The reviewed source4/site0x10a139, call0x10a134 inside
`link_mgr_find_linkid_for_tr_params` 0x109f44, reports internal ring1,
packed interface/type/link/role `0x02020000`, selection0x100, MLD0 in the
retained 39 post buffer. Thus TR25 selects the same link2 as pairwise radio
encryption. Actual key contents, nonce/AAD, ciphertext, and AP acceptance
are still unproven.

The apparent two DMA-fault regex hits in `linux-n1-dma-health-39g.log` were
both harmless `fserror_init` startup messages. The retained 91-byte match
file hashes to `c34de817edab365439ae638aaef28a69a9df4d01219abc9da9049deb86397275`.
They must not be reported as DART faults.

Profile0 is rejected with status313; profile1 reads back rate0x800,
threshold65535, retries255/255, type1/id1. This is also the historical probe22
behavior and does not establish that a default profile is missing.
`setTxPolicyId(1)` 0x1809ed27c supplies bits60 and48 in the 16-byte TX metadata.
Probe40 adds root-only `data_tx_control_profile`, default false, applying
those bits only after the existing control-profile configuration succeeds.
It is an experiment, not a replacement default or a demonstrated fix.

Although `AirshipDataPacket` calls this logical TX metadata a header, the
actual Airship configuration at
`/System/DriverKit/System/Library/PrivateFrameworks/AirshipDK.framework/t2026-CentauriAlpha.plist`
sets `transfer_footer_size=16` for both data-tx and EAPOL; no transfer header
size is specified. Native placement after the 16-byte DMA descriptor agrees
with that configuration. Do not move metadata before the descriptor merely
because the packet class uses the word header.

Hardware comparison, all with firmware MAC used consistently in DHCP fields:

| Probe | Policy and request | Result | Native TX completed |
| --- | --- | --- | --- |
| 40d | default, DHCP DISCOVER x3 | no offer | 19/19 |
| 40e | control1, DHCP DISCOVER x3 | no offer | 22/22 |
| 40f | control1, Ethernet ARP x3 | no reply | 27/27 |
| 40h | restored default, DHCP DISCOVER x3 | no offer | 31/31 |

40a ran before TR25 opened and submitted zero packets to firmware; it is
excluded. The host probe wrapper now emits before/after TX accounting,
requires sufficient aggregate submissions/completions for native probes,
rejects counter resets/errors, and returns the client's nonzero result.
Aggregate counters are necessary evidence, not per-packet on-air proof.
The updated wrapper was hardware exercised in40h: submitted+4/completed+4,
failed0/tx_bad0 and client result1. Background traffic accounts for the
additional transmission; do not label all four as DHCP.

During the combined control-policy observation window, Dot11 MIB reports
host-pulled+8, core-pulled+5 and success+13. No abort, peer-gone,
retries-exhausted, lifetime-expired or RX-filter increase occurs. The later
crypto snapshot has four multicast CCMP replay errors, none attributed to
DHCP; no unicast/multicast CCMP decryption error is reported. Restoring policy0
preserves WPA COMPLETED, Alpha2, TX31/31, RX906/bad0, IPv4global0.

Independent PCAP parsing validates all nine captured DISCOVER IP/UDP
checksums and broadcast flags; the ARP capture contains three frames.
`n1-evidence-40-small.tar.gz` is1214 bytes, SHA-256
`db0a440cda6c8572870d65b2bce3ac4bce0e34575198c6851b8403c080570cda`.
The 4MiB post buffer hashes to
`8f88ff991d5e94cde41ffb532c3c16b8305f3a4e5dca1f332d7cfa2b5373da16`;
31 blocks validate, with one in-flight/malformed block excluded. It filled
before the delayed normal-ring opening, so it does not independently repeat
the probe39 ring-selection proof.

All eight connect40 snapshots are retained as four private
`n1-evidence-40-connect.part-N` files. Their concatenation is 7868854 bytes,
SHA-256 `6b3c7f5ffb0bbc9202dd98be1087eb3d74daf7b69d9c147eeb0b5e1b8468f20a`,
matching the target archive. All seven full-snapshot checksums and the final
snapshot checksum verify. No snapshot acknowledgement is pending on the host.

Probe40 module:96856 bytes, SHA-256
`e4d2c7f21c013371e940c2d03f44990b9d1079590cadab59bdd9ac7ce054f242`.
Initramfs:22337638 bytes, SHA-256
`137275fb23bc49c0487c8e279c2f8a1a437b1614dce36d1d42259cd6fdc8dd70`.
W=1 build has no warnings; `linux-wifi-boot-40.log` records
`NATIVE_LINUX_SHELL_VERIFIED`. Kernel image is unchanged. The current
packaged C helper includes DHCP mode, checksum identical to the39 RAM override.

Additional static leads, not hardware experiments: debug rate override
0x1809f26d4 sends44 bytes with command0x70004 to subtargets0..4 in common
header byte13. A generic ACI command with zeroed routing is insufficient.
The legacy-rate table0x180a1e880 maps1000/2000/5500/11000/6000/9000/12000/
18000/24000/36000/48000/54000 to codes0..11; this does not decode profile
rate0x800. `queryLastTxStats` 0x1809f2ddc uses statistics bit29, TLV363/9,
scope1 and a radio bitmap. The host expects response0xf920/320 bytes but
the native statistics table specifies total292, hence payload280. Do not
copy the host's320-byte parsing/stride without tracing the native layout.
