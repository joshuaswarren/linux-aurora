# N1 DRAM firmware diagnostics

## Continuous capture follow-up, probes 35-36

`capture-n1-connect.sh` saves private full-ring snapshots in target RAM and
checks their byte count and retained-file checksum before acknowledging
producer-3. It also retains the final partial batch without acknowledgement.
The campaign is bounded to 25 seconds and 128 full snapshots; it prints only
counts and driver health. Retrieve the individual
4 MiB files with `linux-download.py`, whose local artifact limit is 16 MiB.

Probe 35d exposed a locking limitation: scan/connect workers and firmware log
operations share the command mutex. Continuous user-space polling therefore
cannot guarantee capture during those transactions. Probe 36 introduces a
separate logger mutex and exposes connecting/scanning booleans so simultaneous
capture can be verified. Initialization takes command then logger lock;
ordinary log reads and acknowledgements take only logger lock. The logger
allocation remains owned by the controller until debugfs operations are
removed and device DMA is disabled. W=1 build passes. Probe 36a records a
snapshot with scanning=1, proving capture can run during a command worker.

Comparing every byte against the live mapping was too strict: two retained
36a snapshots with consumer 29/producer 61 differ only in slot 29 (4924
bytes). Pending=32 does not mean the whole mapping is immutable. The
collector now follows the private download helper's model: preserve and
checksum a complete copy, leave three DMA entries outstanding, and reject
malformed blocks during decoding. The updated collector still needs a run.

The two differing snapshots each contain 31 structurally valid blocks and
both capture the same successful handshake. Site 0x529011 records the
6000000-unit WPA timer, site 0x529141 records result 0/phase 7, and TX site
0x1082a1 reports link flags 0x00010102 before completion and 0x01010102
afterward. Link 2 has data-encryption bit 8 enabled in both records; the
other protection flags finish enabling by completion. Thus a missing
data-encryption-enable flag is not supported as the remaining packet cause.
Captures `n1-fwlog-36a-inconsistent-0.bin` and `-1.bin` are private despite
their name: the decoder excludes their one malformed block in full.
Their SHA-256 values are respectively
`ae70fc90d318556e2389ed63d6e83b76401c6c403732939f131d9a1e1f5c3071`
and `d9a3bf9f00322b4c7f31707c41cbbc199696c9008cd9226f95c7a6d116540e6d`.

New reviewed TX sites: 0x1140c9 associates completion metadata with a host
ring and packet tag, 0x1144cd describes retirement state, and 0x1082a1 reports
a link identifier plus three encryption booleans. No raw packet, key or MAC
arguments are exposed. Probe 35c matches host TR 25 tags 9/10 to raw-status-0
radio completions, but ARP and DHCP still receive no replies. See
`N1-DATA-RINGS.md` for the current networking evidence.

## Hardware evidence, probe 26

The Linux module registered a separate coherent 4 MiB DMA allocation with
the Alpha main ACIPC interface. Firmware initialized it successfully and
remained at execution stage 2 through another WPA test. Authentication still
returned result 362, predominantly phase 3, with no normal packet RX.

`logs/linux-n1-fwlog-26-start.log` records initialization; the WPA test is in
`logs/linux-n1-wpa-fwlog-26.log`. The complete private binary is
`../AuroraSilicon/build/j813-centauri/re/firmware/n1-fwlog-26.bin`, mode 0600,
SHA-256 `3fb58d5ad8ee69f4edbb0bbdc8e46bc6c1093f79064ddd084ea27ead7efbf28a`.
Its checksum was verified against a protected target RAM snapshot.

The capture has version 0x0201, 32 blocks, block size 0x10000, data offset
0x200, consumer 0 and producer 32. All 32 blocks contain nonzero data. It is
not yet a decoded authentication trace; the ring may have filled before WPA.
Naive scanning of aligned words for code pointers is only a heuristic and
must not be treated as function-execution evidence.

This is separate from the earlier `fw_debug` TR6 experiment that aborted
Alpha. There is no evidence that the earlier nominal 2048-byte buffer size
caused that abort; firmware can fragment its internal log buffers.

## Exact ABI evidence

The host's t2026-CentauriAlpha configuration assigns the OS-stage debug memory
region ID 1. AirshipDK `ashp::acipc::direct_memregion::map` at 0x180285fa0
builds a 24-byte map request:

| Offset | Size | Value |
| --- | --- | --- |
| 0 | 2 | Opcode 8 |
| 2 | 2 | Region ID 1 |
| 4 | 4 | Allocation size |
| 8 | 8 | DMA address |
| 16 | 4 | Zero |
| 20 | 4 | Zero |

The exact mapped UMAC firmware's dispatch at 0x1751c4/0x175340 passes region,
size and address to callback 0x5a0001. `DramPhyService_handle_buffer` at
0x5a0000 routes ID 1 and sizes greater than 1 MiB to `InitDramLoggerMgr`
at 0x504ba0. This writes a 0x158-byte control header by GDMA. The allocation
stays owned by the IPC buffer list until bus mastering/MSI are disabled;
failed initialization does not free a possibly device-owned mapping.

| Header offset | Meaning |
| --- | --- |
| 0 | u16 version, observed 0x0201 |
| 2 | u16 power-of-two block count |
| 4 | u32 block size, 0x10000 |
| 8 | u32 count minus one |
| 0xc | u32 data offset within mapping |
| 0x10 | u32 host consumer counter |
| 0xb0 | u32 firmware producer counter |

Counter meanings are verified statically: header VM is 0x20014d6c;
function 0x144cfc compares producer minus consumer against block count.
Initialization registers a GDMA read from host +0x10. There are three
firmware DMA buffer descriptors. Until record completion sequencing is
decoded, `fwlog_ack` conservatively leaves three produced entries outstanding.
Probe 27 hardware validated acknowledgement: after a verified snapshot,
writing consumer 29 succeeded. The first read still showed producer 32;
after a subsequent WPA attempt, producer advanced to 61. Firmware stayed
at stage 2. The implementation rejects out-of-range/backward counters and only
touches the mapped host consumer field, without a new doorbell or BAR probe.

## Private capture procedure

After firmware/interface configuration, with no connect or scan in progress,
write 1 to debugfs `fwlog_start` once per boot. `fwlog_status` exposes only
fixed header metadata. `fwlog_buffer` is a root-only binary file; never cat it
to a console, text log or tool output. Data can contain private connection
material. The firmware can modify blocks while a snapshot is read.

`linux-download.py` captures a target file to a mode-0600 RAM snapshot,
compresses it over the exclusive M5 serial session, rejects malformed or
interleaved payloads, verifies SHA-256 and creates a local mode-0600 artifact
without overwriting existing files. It does not print payloads or exception
values. The probe-26 download validated this path on hardware.

Probe-27 private captures, both 4 MiB with target/local SHA-256 agreement:

- `n1-fwlog-27a.bin`: consumer 0/producer 32,
  `e55e83abd6dada1985daaaeb913a49267f93330a711af011c752a4ebf830618c`.
- `n1-fwlog-27b.bin`: consumer 29/producer 61,
  `5726a2f87193d044882bc7ffb9e496d6e9662aa1ad89c22c1912af3ce2249654`.

Native logs: `logs/linux-n1-wpa-fwlog-27a.log`,
`logs/linux-n1-fwlog-27-ack.log`, `logs/linux-n1-wpa-fwlog-27b.log`.
Linux remains booted with the supplicant network disabled. No firmware trace
payload has been printed or put into a public log.

Next: decode record boundaries/core IDs and establish which portion contains
the authentication interval before inferring a cause.
The `_wma` FTAB plist contains assertion descriptions,
not a catalog of ordinary trace messages. WPA and normal packet networking
remain unproven.

## Decoder leads and limits

UMAC log writers 0x13f8ac, 0x13f8d8, 0x13f908, 0x13f93c and 0x13f974
represent zero through four arguments. The two-argument writer emits the
return address minus the firmware base, masked to 24 bits, OR 0x20000000.
The firmware base global is statically zero (pointer 0x2001cbc4 to 0x20025eb0).
Hardware framing must still be decoded: captured words that match call-site
addresses do not retain that arity encoding consistently. Different cores
also share address ranges. Do not report a matched address as an executed
UMAC function until the record's source and framing are established.

The logger control header lives at UMAC VM 0x20014d6c. Its periodic update
callback 0x144b7c calls `Logger_StartDmaToUpdateDramCb` (0x144c40) and rearms
timer 12 for 50000 units. DMA completion callback 0x144c98 was manually
created in Ghidra; confirm its assembly before relying on decompiler bounds.

Additional read-only host references were extracted with `ipsw dyld extract`:
`../AuroraSilicon/build/j813-centauri/re/dk/CentauriDebug` from the DriverKit
cache and `CentauriDiagnostic` from the OS cache. Neither symbol inventory
exposed a DRAM record decoder. The former mainly contains transport/ring
and CoreCapture plumbing; the latter collects diagnostic files.

## Decoded authentication interval: captures 27c and 27d

`decode-n1-fwlog.py` now validates complete block/record boundaries and emits
metadata only. It excludes an entire inconsistent block, including any
otherwise valid prefix. Each 64 KiB block starts with source 0x7f and tag
0x04000013. Header word at +12 is the index of the final valid word, so the
byte extent is `(word12 + 1) * 4`. Each record contains `3 + (word1 >> 26)`
words. `word0 & 255` is the source, `word1 & 0xffffff` the site identifier.
Bit 31 of word0 selects compressed argument encoding; reviewed queue records
use unsigned LEB128 in this case, otherwise little-endian u32 arguments.
The other timing/encoding fields are retained without claiming a full clock
conversion. Do not assume physical block order gives a complete time order.

Source 6 is UMAC and source 4 is TX. These mappings are supported by exact
logger call-site matches: the TX four-argument logger's call sites match
source 4 in 5,379 records, versus six accidental matches each in sources 2/3.
The 0xc0 logger input tag is shared across processors and does not alone
identify the source. Other sources remain numeric until mapped separately.

Initial parser validation: 32/32 blocks in 26, 31/32 in 27a (slot 0 rejected),
and 32/32 in 27b. Releasing space and enabling the network in the same native
shell command captured the actual M2 interval without a module rebuild:

- 27c: consumer 58/producer 90; 32/32 valid blocks; 138,886 records;
  SHA-256 `90501af3b0d34dcd296b373be4470c969e6f4818d47f09262c7167907c6d281d`.
- 27d: consumer 87/producer 119; 31/32 valid blocks; 129,496 records;
  slot 23 rejected at offset 45520;
  SHA-256 `b9d708df36e834abfe1b11e156a1ae153e7cecbddd3ee1caff4f34009e1441ee`.

Both captures are protected files under the existing private firmware RE
directory and passed target/local SHA-256 verification. Their native runs
are `logs/linux-n1-wpa-fwlog-27c.log` and `...-27d.log`.

Both contain one UMAC log site 0x529ab5: this is after the M2 builder calls
FGPS_SubmitPacket (0x4ff768) and returns. The HMAC computation therefore
returned in these attempts. FGPS dispatch and the internal HTM handoff are
also present. Reviewed, non-secret argument fields identify this packet:

| Observation | Both captures |
| --- | --- |
| CTM selection at 0x11a509 | flow 3, link 2, kind 3, subkind 0 |
| CTM submission at 0x118ff7 | flow 3, buffer tag 6, length 141 |
| TX completion at 0x11429b, source 4 | tag 6, raw status 5, count 1 |
| CTM completion at 0x11a2c5, source 6 | flow 3, tag 6, status 3, count 1 |

The earlier statement that no internal M2 completion was visible is now
superseded: a failed internal completion is visible, while the supplicant's
M2-success callback/timestamp remains absent. On-air M2 transmission is
still unproven. Unrelated periodic flow-1/flow-35 traffic must not be mistaken
for the single kind-3 M2 packet.

## Radio-side status 5 is a flush result

The full LMAC5G analysis image is now mapped with eight segments and all
1,529 assertion sites verified. It is `w5lp-mapped.elf`, SHA-256
`3beac63be0eb6cb6d685c2aa7ad6904e52e7d2318deeebbd45e8b95bec98a5f0`, in
Ghidra project `j813-n1-lmac-mapped`. 843 existing functions were renamed
from the assertion catalog; 118 assertion locations still lack functions.
Both `w5lp` and `w2lp` are supported by the extraction/mapping helper.

LMAC source 3 has the matching queue flush records in both 27c and 27d:
site 0x12dab9 logs queue 2, previous mode 0xff, requested mode 0.
Site 0x12d015 logs that same queue and mode/status 0x00050000.
`TXCPU_LMAC_API_hwtdq_flush_handler` (0x12da34) sets the requested mode;
`HWTDQ_SM_queue_win_move_private` (0x12cfa4) maps modes [0,1,2] to completion
statuses [5,6,6] via the literal at 0x12d03c. This proves raw status 5 is a
flush completion here, not an access-point rejection or a proven expiry.

The M2 TX metadata identifies vtdq 5, local TR 0xaa and radio hardware queue 2.
Further decoding shows radio 1/access category 3 selects vtdq 5 and completes
the selection handshake, then selects an empty list. Blocking mask 1 comes
from `vtdq_mgr_remove_vtdqs_for_link` (0x1048e4), reached after link 2 receives
state 0x10 and pending removal actions 8/0x200. This is link teardown.

## Zero-duration handshake timer identified

Both 27c and 27d log `Supp_AddTimer` at site 0x529011 with link 2 and duration
zero. UMAC logs then establish the order within the same processor:

1. The M2 builder returns (site 0x529ab5).
2. Timer action 2 reaches 0x52720c, which sets result 0x16a (362).
3. Site 0x529141 reports result 362, phase 3.
4. Only afterwards does CTM submit the actual M2 packet, tag 6.

Relative trace ticks from builder return to the first result are 87/86 in
27c/27d, versus 3446/3343 to CTM submission. These are trace ticks, not a
claimed conversion to wall-clock units. The radio flush follows an already
reported authentication failure; it is not the first failure in these traces.

The driver sent a zero-filled 20-byte TLV 0xf913. Command handler 0x574000
copies it over the connection context at offset 0x6cd. Firmware initializer
0x511ee8 sets that structure to little-endian u32 values
`[260, 6000, 2000, 15000, 300]`. Supplicant configuration 0x524b38 obtains
the structure via 0x50b646 and copies the selected timeout into supplicant
offset 0x1578. The AKM-4 table entry selects offset 4 (6000 ms).
`Supp_AddTimer` (0x528ff0) multiplies that value by 1000 and submits the timer.
Zeroing TLV 0xf913 therefore erased the working default and armed an
immediate authentication timeout.

The native driver now supplies these five firmware defaults in that TLV.
The module build passed with W=1. Native probe 28 validates authentication:
firmware result 0, phase 7, all five handshake timestamps populated;
wpa_supplicant state COMPLETED; Alpha stage 2. Evidence is
`logs/linux-n1-wpa-timeout-fix-28.log`. No DRAM logger was needed for this
successful run. Normal Linux packet delivery, DHCP and usable networking
remain unfinished. The later stability check found Alpha stage 3 after all
127 RX buffers were consumed (`logs/linux-n1-connected-28-stability.log`).
Authentication remains proven, but sustained connection is not yet proven.
Probe 29 adds RX completion consumption, bounded private samples and buffer
reposting to address the missing recycling before packet delivery is enabled.
