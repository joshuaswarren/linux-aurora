# M5 Fender setup before firmware initialization

The M5 macOS 25G76 AP trace observes a write of zero to physical
`0x580d06030` before either GPU firmware initialization descriptor is
published. The M5 Linux initialization path omitted this update.

The AGXG17G binary independently establishes the operation: the code at
`0xfffffe00089e0408` applies `value & 0xfffffff9`, then writes the result to
SGX offset `0xd06030` through the native register accessor. The observed
store PC is `0xfffffe0008a0d5e4`. The Linux candidate therefore preserves
unrelated bits and clears bits 1 and 2; it checks the resulting register
before publishing either initialization descriptor.

The complete AP GPU write count from boot through the controlled native
test was four:

| Address | Value | Sequence |
| --- | --- | --- |
| `0x580e90000` | `0x30001` | 15 |
| `0x580e80110` | `0x0f00200f` | 20 |
| `0x580e80110` | `0x1f00200f` | 24 |
| `0x580d06030` | `0` | 79 |

GFX1 and GFX initialization captures completed at sequences 135 and 162.
The observer covers the measured 64 MiB GPU aperture, excluding the two
ASC mailbox apertures. It observes AP accesses, not the firmware CPUs'
own register accesses. Its callback never performs a proxy request.

Mailbox traffic reached the shared 100,000-event recording cap. After
pausing the hypervisor, mailbox observers were removed and the recording
cap increased to 120,000. The independent AP callback count remained four
before and after the controlled native test, establishing that the cap
did not discard additional AP writes. Native validation passed three
queues, 3,072 exact compute values and 12,288 render pixels.

Local evidence:

- `logs/m5-host-gpu-setup-hv-20260910/gpu-events.jsonl`
- `logs/m5-host-gpu-setup-hv-20260910/console.log`
- `logs/m5-host-gpu-setup-native-result-20260910.json`
- `hv/kernel/agx-25G76/com.apple.AGXG17G`

The fresh Linux test confirmed Fender changed from `0x6` to `0x0` and both
firmware roles still initialized successfully. The queue and scheduler
checks passed, one kick was accounted for, and both firmware CPU stop
requests succeeded. However, all 1,024 output sentinels remained untouched
after 10,000 polls. This setup correction alone does not fix execution.

`logs/m5-fender-compute-native-20260910.log` retains that result.
`logs/m5-fender-pmgr-readback-20260910.log` also records group control
`0x580e90000=0x30000` in Linux, compared with the native write `0x30001`.
Both GPX and GFX PMGR power-state words read `0x1f0023ff`.

A second fresh Linux boot matched group control from `0x30000` to
`0x30001` before starting either firmware CPU and repeated the Fender
clear. Both initdata acknowledgments and queue setup still passed, but
all 1,024 outputs again remained untouched. The precise role of group
control bit zero remains unqualified; matching it does not fix execution.
See `logs/m5-pmgr-control-compute-native-20260911.log`.

## GPU restart evidence

Retained RAM from that second boot contains a primary event-ring record
of type 4, generation zero and affected stamp index 11. Primary status B
records source count 1, reason 0, hardware summary 1, epoch 1 and recovery
state 1. The exact M5 firmware constructs this report at TEXT offsets
`0x6b90..0x6d60`. Its summary is the OR of SGX offsets `0xc0b0`, `0xc0c8`
and `0xc0d8`; this evidence alone does not identify a GMMU fault.

A further fresh boot with the owned report-ring consumer confirmed the
same type-4 restart live and stopped waiting immediately. Both firmware
CPU stop requests succeeded. The host advances report consumer cursors,
but never acknowledges GPU recovery or rewrites its state. All memory
owners remain pinned. No firmware CPU crash was observed; a GPU restart
request was observed, so the earlier absence of a CPU crash must not be
read as an absence of a GPU fault.

Local evidence:

- `logs/m5-recovery-retained-20260911/manifest.json`
- `logs/m5-recovery-trace-compute-native-20260911.log`
- `logs/m5-recovery-trace-retained-20260911/manifest.json`
- `logs/m5-recovery-records-retained-20260911/manifest.json`

The first trace experiment incorrectly added bits at HW-data `+0xed8`.
The firmware copy at `0x1fc10..0x1fc14` uses a base already offset by
`0x9f8`; its destination is `0x1783f4`, not trace flags at `0x1779fc`.
That experiment produced no primary trace records. The corrected
candidate retains native hardware data and sets primary status-A `+0`:
the exact root initialization at `0x1fadc` retains root `+0xa8`, and
`0x3074..0x3088` copies the pointed-to word into trace flags. A fresh Linux
boot confirmed status-A and retained trace flags both equal `3`, and
primary trace records now appear. No firmware DATA patch is used.

The trace records the queue setup (`0x40`) and kick (`0x41`), then many
processing-loop brackets (`0x105`/`0x106`). Retained final records include
`0x04000113 [2,0,0,1]`, entry into recovery pause bit `0x1000`, and final
pause word `0x1042`. The classifier view at shared `+0xbc80` names one
data-master-2 slot, index zero. All shader outputs remain untouched. Exact fault
classification still needs the additional bit-9 trace class: the byte
load at `0x6260` reads `0x1779fd`, not `0x1779fc`, before testing bit 1.

Evidence: `logs/m5-status-a-trace-compute-native-20260911.log`,
`logs/m5-status-a-trace-retained-20260911/manifest.json` and
`logs/m5-classifier-state-retained-20260911/manifest.json`. The last
capture contains 21 objects from 85 RAM pages, all read successfully.

## Hardware admission and classification

With trace mask `0x203`, a fresh boot preserves the relevant final events
without losing its print budget to loop markers:

| Event | Arguments |
| --- | --- |
| `0x900` | `1086929f, 1086929f, 0, 0` |
| `0x902` | `0, 0, 0, 0` |
| `0x903` | `2, 0, 2, 0` |
| `0x04000909` | `1, 0, 1, 0` |

M5 code `0x80bc..0x8220` selects `0x902` when the active-work state and
work-channel producer checks are empty and the progress interval exceeds
its threshold. This sets classification 2 for data master 2, slot 0.
The retained last-progress timestamp at DATA `0x125bf0` is zero. This
qualifies the classifier path, not a root cause or a reason to bypass it.

A bounded stopped-CPU MMIO read then confirmed hardware admission:
both CPU controls are zero; GPX/GFX report `0x1f0023ff`; selected slot 0
has command `0xfffffc20c0400000`, queue `0xfffffc20c0000840`, state
`0x1000d` and progress `0xb0000000001`. The first fault mask and the
data-master-2 mask are both one; the other two fault masks, other data
master masks and active state are zero. These are exact M5 register reads.
They must not be decoded using Neo's assumed pipe-field layout.

`read-m5-stopped-ksm.py` checks the retained recovery probe, CPU controls
and power state before accessing these registers. Its default mode does
not write selectors. The selected queue is 127, so default-mode queue11
reads are skipped. The selected GMMU view is `0x100` and its fault-info
word is zero; this is not a scan or a claim that all banks are fault-free.

Evidence:

- `logs/m5-classifier-trace-compute-native-20260911.log`
- `logs/m5-classifier-trace-retained-20260911/manifest.json`
- `logs/m5-stopped-ksm-read-20260911.log`
- `logs/m5-stopped-ksm-fault-read-20260911.log`

The optional `--queue11` read temporarily selects queue 11 using M5's
native queue-selector store, checks readback, and restores the previous
selection in `finally`. The qualified run reads enable `0x31000070005f0001`,
stamp `1`, completed word `0x8000000000`, then verifies selection restored
to `127`. See `logs/m5-stopped-queue11-verified-read-20260911.log`.
This completion value is an observation after recovery, not yet proof of
an initial dependency error. A native first-kick capture independently
uses stamp 1 and predecessor 0, so changing their generation without an
earlier hardware read would not be justified.

The native stamp-1 compute capture is the first job on that application
queue, not the first work after GPU startup: its observation list includes
queue 8 at stamps `0x4f6..0x4f9` before queue 10 at stamp 1. The separate
startup capture's first work publication contains TA tags 0, 15 and 14;
that first tag-14 record names queue 0, count 1, priority 0, stamp 1.
Its SKSM dependency entry was not captured, so this does not establish a
required priming job or justify a different initial stamp.

Two no-kick mode-5 boots could not provide a powered queue-register
baseline. The normal setup and a setup with the compute path's idle-policy
request both stopped the ASC CPUs successfully, with GPX `0x1f0023ff`
and GFX `0x1000230f`. The reader refused queue MMIO before any selector
access. Retiring the idle-policy request does not establish powered GFX.
Evidence: `logs/m5-queue11-before-compute-20260911.log` and
`logs/m5-idle-policy-queue11-before-compute-20260911.log`.

Mode 7 isolates a later boundary: it allocates the same job as mode 6,
publishes descriptor tag 3 and channel configuration tag 15, but keeps
tag 14 beyond the producer. On hardware, outer counters retired to
`[2,2,2]`, inner counters to `[3,3,3]`, inputs remained unchanged and
all 1,024 outputs retained their sentinels. Both firmware stop requests
succeeded. GFX still reported `0x1000230f`; no queue MMIO was attempted.

The RAM-only `--before-compute` capture read all 21 objects from 85 pages
without errors. Compared with the mode-6 recovery capture, descriptor,
event state/link, scheduler, completion prefix and program buffers are
byte-identical. Inner counters differ only by 3 versus 4. This qualifies
firmware setup immediately before the failing kick; it does not prove
an initial hardware completion value or successful GPU execution.

Evidence: `logs/m5-prekick-setup-mode7-20260911.log`,
`logs/m5-prekick-queue11-before-compute-20260911.log` and
`logs/m5-prekick-setup-retained-20260911/manifest.json`.

## Cold native SKSM entry

The startup observer now captures the first 4 KiB of a tag-15 ring's
firmware alias before mailbox publication, plus the matching tag-14
metadata. A fresh macOS 25G76 boot captured 24 publications and three
stable ring prefixes without errors, reading 1,106,904 bytes within the
existing DRAM-only budget. The first work used queue 0, priority 2,
data master 0, stamp 1. The only populated 512-byte region in its first
ring snapshot starts at offset `0x200`.

That entry contains header `0x1000000000000004`, no mcache, flag bits
1/61/63 clear, dependency `(queue 0, stamp 0, type 1)`, RCE kind 0 and
auxiliary `0x3fffffffffffff`. Linux's first compute entry has the same
initial dependency and flag conventions for queue 11; its compute QoS
and RCE payload remain separate from native tiling work. There is no
evidence here for changing the initial stamp or dependency type.

The same native boot then verified 3,072 compute values and 12,288 pixels
across three Metal queues. This is a working native reference, not Linux
execution. The startup sequence does not by itself prove that Linux must
submit a tiling job before compute.

Evidence: `logs/m5-cold-sksm-hv-20260911/startup/publication-01/manifest.json`
and `logs/m5-cold-sksm-native-metal-result-20260911.json`. Five synthetic
startup-observer tests pass, including the new bounded cold-ring capture.

## Native firmware event comparison

`launch-m5-gpu-hv.py --capture-startup --native-fw-trace-mask 0x201`
now sets only the primary status-A trace input before the native INBOX1
initialization publication. The guarded writer verifies the current root,
firmware profile, status/ring layout and DRAM translation, and retains an
owner-local restoration receipt before writing. The read-only graph reader
remains unchanged. No firmware DATA, executable code, queue contents or
mailbox payload is modified.

Setting that input after startup did not update the firmware's cached flags.
Exact firmware opcodes 0x16 and 0x2a branch to 0x37d4, which sets
DATA+0x1779f8 and calls the configuration-copy routine at 0x3068. Other
control paths also invoke it. The routine copies status-A+0 to
DATA+0x1779fc and clears the refresh gate at 0x3310. Ordinary work submission
does not necessarily refresh these settings.
The dispatcher subtracts 4 before indexing its table at 0x5ab4; table
indices 0x12 and 0x26 must not be used as wire opcodes. Likewise indices
0x27, 0x29 and 0x2b refer to opcodes 0x2b, 0x2d and 0x2f respectively.

On a fresh 25G76 boot, the pre-publication write read back as 0x201 and a
later paused-owner observation verified firmware flags 0x201. Both firmware
configuration captures and all 24 startup captures succeeded. The native
Metal reference passed 3 queues, 3,072 compute values and 12,288 rendered
pixels. A stable status/ring observation retained the following compute
sequence among other native graphics traffic:

```text
0x41 [9, 3, 2, 1]
0x47 [0, 0, 0, 1]
0x43 [9, 3, 0x2000000000003, 0xfffffc20c00006c0]
```

The firmware event-reader path at 0xe6a4..0xe6dc checks the hardware word
at SGX+0x21168+8*selector before emitting 0x47 at 0xe728. The packet parser
emits 0x43 at 0xb86c. Event 0x48 appears later in the processing path at
0xeaa8. The retained native snapshot contains all three event classes.
Linux's failing run emits its kick `0x41 [b,1,2,1]` but none of 0x47,
0x43 or 0x48 before recovery. This localizes the next investigation to
producing/receiving the first SKSM hardware event; it does not yet establish
whether submission, translation, hardware admission, or event delivery is
the cause. It also does not establish whether earlier native tiling work
is a prerequisite for the first compute job.

`trace-m5-native-fw.py` can snapshot or restore through the existing paused
owner. Restoring the host input to zero does not prove the cached firmware
flags changed: this boot still observed 0x201 immediately afterward. The
helper now retains its receipt until a later observation verifies both
values restored. Six host tests cover write ownership, stale-root/device
mapping rejection, existing-input protection, failed readback and deferred
firmware restoration. The Metal reference passed again after host-input
restoration. Linux shader execution and DCP presentation remain unproven.

Evidence: `logs/m5-native-fw-power-hv-20260911/`, particularly
`native-fw-trace-snapshot-1789093426297467000/manifest.json`, plus
`logs/m5-native-fw-preinit-metal-result-20260911.json` and
`logs/m5-native-fw-input-restored-metal-result-20260911.json`.

All 1,024 shader outputs remain untouched, completion stays zero, and
both firmware stop requests succeed. Linux GPU execution remains open.
