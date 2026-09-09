# Tracing branch review — September 9, 2026

The branch inventories were checked through the authenticated GitHub API.
The public browser returned 404; that does not mean the repositories are absent.
This review did not merge branches or change the live carrier.

## References that transfer to M5

* [Neo speaker trace preparation](https://github.com/aurora-silicon/neo-bringup/blob/1ee093a421b91defae05ad3b8dd5eb35e264e942/docs/j700-speaker-trace-preparation.md): install the protocol decoder before its board-specific correction layer; trace only the relevant registers; collect before/after snapshots for one controlled operation. Avoid unrelated tracers in the same run.
* [AOP management trace](https://github.com/aurora-silicon/neo-bringup/blob/1ee093a421b91defae05ad3b8dd5eb35e264e942/research/j700-aop-audio-management-trace-2026-09-02.md): preserve raw AFK payloads and remove per-message guest backtraces. A working generic decoder still had the wrong endpoint map for the new chip.
* [Existing WLAN tracer](https://github.com/aurora-silicon/m1n1-aurora/blob/a2a1db46e9f3d2c1a8fb96fe23d484e73f27edae/proxyclient/hv/trace_wlan.py): translate DMA addresses through the device's DART stream and read ring entries when indices/mailboxes change. This is an architectural example, not an N1-compatible descriptor decoder. The MT7932 tracer is likewise a different radio protocol.
* [DART tracing implementation](https://github.com/aurora-silicon/m1n1-aurora/blob/a2a1db46e9f3d2c1a8fb96fe23d484e73f27edae/proxyclient/m1n1/trace/dart.py): invalidate the software translation cache on observed guest TLB invalidation. On the SPTM M5 guest, additionally account for mappings changed through secure calls rather than assuming all table changes appear as guest MMIO.
* [Neo AOP/DART investigation](https://github.com/aurora-silicon/neo-bringup/blob/826613ebcf90301a8c0e1a04fe490497eafbe752/research/j700-aop-afk-dart-fix-2026-08-31.md): `DART.initialize()` clears live stream tables. A passive tracer must not initialize an already-running coprocessor's DART. Reusing an existing L0 also exposed a separate Python mapper bug.
* [PMP start evidence](https://github.com/aurora-silicon/neo-bringup/blob/826613ebcf90301a8c0e1a04fe490497eafbe752/research/evidence/HV-pmp-start-traced-20260902T005046Z.txt): distinguish the guest issuing RUN from the coprocessor actually becoming ready. The authors also withdrew an apparent blank-firmware theory after checking the same offsets outside the guest.

## Branch selection

The M5 isolated worktree already starts at J700 `a2a1db46`, including the
WLAN, MT7932, DART and audio tracing infrastructure. `J700-eoshv` is 53 commits
behind that baseline and `wip/j700-hosted-audio` is 26 behind; switching to
either does not add a newer tracing implementation.

`fix/j700-dart-retention` at `de6c93e2` has two unique commits and is 53 behind
J700. Its DART change carries the ADT `retention` property into SPTM state and
keeps translated streams enabled during the sleep transition. The saved M5
`dart-mtp` node has no `retention` property. This is useful lifecycle evidence,
but is not a demonstrated fix for guest16's MTP fault. Do not merge the whole
branch as a tracing upgrade.

## Application to the current capture

The narrower M5 guest25 capture now proves385 PMP mailbox IRQs delivered to
CPU6, with RTKit hello/discovery/IOReporting traffic. PMP status still reads
zero. The older Neo missing-IRQ hypothesis therefore does not fit this M5
run. The golden Neo module explicitly initializes PMP DAPF, but its register
index must not be ported blindly: M5's extra dart-pmp apertures identify as
RWSP rather than DAPF. The status-address calculation in `tools/pmp_assist.py`
does transfer: update aperture minus0x10000 plus16 times PTD index1, confirmed
by the native ApplePTD read PC in the journal.

The Neo NVMe CQ control became directly relevant during this review. Guest17
passed the MTP fault after guest-only node removal, then hit ANS2's completion
queue doorbell assertion. Read-only M5 inspection reproduced CAP.MQES=0x40
(65 advertised entries) versus AQA/IOQA=0x003f003f (64 programmed entries).
The Neo loader's count correction was previously excluded by the M5 profile.
It is now ported to the exact M5 instruction at `0xfffffe000a9ecc14`, with
hash and nine-instruction guards. Offline tests pass; this is not yet a
successful M5 storage boot. See `README.md` for the hardware evidence.

`n1-trace.py` currently captures PCIe configuration and BAR register ordering;
it does not capture packet DMA. The next capture layer must correlate the N1
ring descriptor, translated payload, producer index/doorbell and completion
for one ARP or DHCP exchange, using the exact 25G76 N1 ABI. Bound buffer reads
and event counts, preserve private raw bytes and label incomplete captures.

The local Linux IPC implementation supplies concrete observation points to
confirm against native traffic: control-context setup at BAR offsets
`0x8054/0x8058` with control`0x907c`, and Alpha-context setup at
`0x10/0x14` with control`0x1028`. The context header supplies four DMA index
arrays at`+0x10..+0x28` and message completion/transfer ring pointers at
`+0x34/+0x3c`. Decode the native open-ring messages to learn actual ring DMA
addresses and sizes rather than reusing Linux allocation offsets. The Linux
data transmit path rings Alpha`+0x1030`; its descriptors are32 bytes.
The saved M5 topology identifies`/arm-io/dart-apcie0` as dart-id0, but the
live stream/remap selection still needs verification. Invalidate the host
translation cache before bounded capture reads because SPTM can update DART
tables without a guest MMIO event. These are capture anchors, not proof of a
native ring trace.

Guest16's broad PMGR SYNC trace collected 4,442 events, mostly MCC counter
reads. The MCC overlap is now writes-only. Guest16 stopped at an MTP DART
panic rather than guest15's asynchronous PMGR SError; that is a changed first
failure, not proof the SError is fixed. Narrow tracing and an MTP isolation
boot were the next controls before native N1 packet capture. Guest20 then
proved the PMGR faulting caller is CPU energy-accumulator restoration and
collected a provenance-checked msgbuf. It reached1,180 NVMe calls without the
earlier CQ panic. The isolated energy-restore omission is being tested in
guest21; root mount and native N1 packet capture are still outstanding.
