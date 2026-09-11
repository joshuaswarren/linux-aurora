# M5 first-compute reference and remaining execution gap

The native reference proves successful M5 compute and its submission encoding.
Linux still stalls before producing output. The shader bytes, command encoding
and six program-page mapping profiles now match the controlled native job.

## Verified native reference

Target: Mac17,3 / T8142, macOS 25G76, GFX RTKit-3255.160.4.

The first-stamp witness is the private workbench capture:

```text
j813-linux/logs/m5-gpu-startup-hv-20260910/sksm-submit-1789081402372803000
```

It contains 70 hash-verified objects, queue 10 / priority 2 / DM2 / stamp 1,
completion of the same descriptor, and correct output. This is the first job
on that queue; earlier native graphics means it is not a fresh-GPU baseline.

The newer capture adds stable page-table walks around context data reads:

```text
j813-linux/logs/m5-live-reference-hv-20260911/sksm-submit-1789099816130161000
```

Its 46 objects have matching hashes. Queue 9 / stamp 4 completes the same
descriptor successfully. Both controlled macOS runs verified 3,072 compute
values and 12,288 rendered pixels across three queues.

A further fresh native boot captured the same queue and stamp as Linux's
probe, queue 11 / stamp 1:

```text
j813-linux/logs/m5-native-monitor-reference-hv-20260911/sksm-submit-1789102307882731000
```

Its 46 objects include same-descriptor completion and six stable mapping
profiles. All three comparison tools pass; the controlled workload again
verifies 3,072 compute values and 12,288 pixels. This is a first-queue-job
reference, not a fresh-GPU baseline: native graphics already ran.

From the Linux source tree, set `NATIVE_CAPTURE` to a directory above:

```sh
python3 tools/aurora/j813/gpu/verify-m5-sksm.py "$NATIVE_CAPTURE"
python3 tools/aurora/j813/gpu/verify-m5-compute-registers.py "$NATIVE_CAPTURE"
python3 tools/aurora/j813/gpu/verify-m5-compute-program.py "$NATIVE_CAPTURE"
```

The checks compare the production Rust serializers against a 512-byte SKSM
entry, 21-byte tag14 command, 12-byte producer transition, 40 registers over
480 bytes, and defined ranges of six program pages. The program check covers
64 shader bytes, 132 pipeline bytes, 48 command-stream bytes, preemption,
robustness, input/output, and same-descriptor completion. When mapping
receipts exist, it also checks all six page-table profiles.

## Mapping correction verified on Linux

The native walks exposed a real permission difference:

| Program page | Native leaf attributes | Linux protection |
| --- | --- | --- |
| Shader and CDM stream | `0x80000000000c8b` | `PROT_GPU_SHARED_RO` |
| Pipeline, preemption, buffers, robustness | `0xc0000000000c8b` | `PROT_GPU_SHARED_RW` |

Linux previously used GPU-RW for all six pages. It now uses GPU-RO for only
the shader and stream; CPU initialization retains its separate WC mapping.
Root attributes `0x3000000000001` and intermediate attributes `3` match too.

A fresh Linux mode9 capture verifies the installed correction:
`logs/m5-program-readonly-retained-20260911` contains 33 objects over 243
physical 4 KiB pages, with no read errors and matching hashes. All six mapping
profiles match native. The job still leaves all 1,024 output words untouched,
with unchanged inputs and eventual firmware recovery. Matching permissions
is therefore a measured correction, not a sufficient fix for execution.

## Early Linux state without a firmware halt

Mode9 reads four hardware-event producer words, twelve slot words, and three
queue words only when queue 11 is already selected. It never changes hardware
selectors and checks power and selection before and after its reads.

Four hardware samples at polls 1–4 took 12/10/10/10 microseconds. No recovery
report appeared during any read. Event producers stayed zero and queue words
stayed `[0x31000070005f0001, 1, 0x8000000000]`. Firmware later requested
recovery. These are sequential live observations, not an atomic snapshot.

Mode10 adds the firmware's compute monitors through the AP read port. Four
hardware samples took 17/17/15/15 microseconds, each returning five valid,
stable observations with no recovery report during the read:

```text
slot state     = 0x1000d
slot progress  = 0xb0000000001
pipe0 +0x1688   = 1
DM2 +0xa730     = 0
slot state     = 0x1000d
```

The M5 port encoding uses namespace bit 43 and index shift 44. Firmware
`0x7144..0x719c` and `0x7b6c..0x7b90`, with table `0x44984`, supply these
reads. The pipe mask is at bits 16–18, unlike the Neo decoder's bits 24–25.
Slot progress uses the existing direct slot-zero view. The AP issues only
read tags; no register-data, bank-selector, recovery-acknowledgment or
firmware-private-state writes are made.

These monitor values do not yet establish shader instruction start or
completion. Mode10 still leaves all 1,024 outputs untouched before recovery.
Its retained RAM capture, `logs/m5-live-monitors-retained-20260911`, has
33 objects / 243 physical pages, no errors and matching hashes.

## Qualified native compute observation

The native helper `m5_native_monitor.py` performs its reads in one short
proxy call. It uses the existing hypervisor's reserved heap for code/results;
no guest or firmware executable is patched. SGX stores are only read tags to
AP port 9. It checks both power domains before low-SGX reads, follows the
naturally selected slot, accepts only a single active pipe, and rechecks
power, slot state, and bank selection afterward. Unread fields remain zero
and must not be interpreted without their qualification flags.

The tiny reference powers down before both publication/completion samples.
`m5-metal-busy-reference.m` supplies two bounded native compute commands
whose 131,072 output values are checked against CPU results. Its iteration
count is bounded at 1,048,576. A fresh native boot supplied a valid 7.125-us
observation while queue 9 / stamp 1 was executing, with unchanged power,
slot and queue selections. Both commands subsequently completed and passed
output verification, taking about 314.1 and 488.6 ms. Native graphics ran
before this test; stamp 1 establishes the queue's first job, not a completely
uninitialized GPU.

Private observation: `logs/m5-native-first-active-hv-20260911/`
`native-monitor-1789103518752966000/observations.json`.

| Observation | Working native job | Stalled Linux mode10 |
| --- | --- | --- |
| Selected slot | 0 | 0 |
| Slot state | `0x1000d` | `0x1000d` |
| Slot progress | `0x90000000001` (queue 9, stamp 1) | `0xb0000000001` (queue 11, stamp 1) |
| Pipe 0 register `0x1688` | 1 | 1 |
| Pipe 0 register `0xa730` | 0 | 0 |
| Own queue register `0x21008` | `0x31000070005a0001` | `0x31000070005f0001` |
| Own queue register `0x21020` | 1 | 1 |
| Own queue register `0x21028` | `0x8000000000` | `0x8000000000` |
| Event producers | All four zero | All four zero |
| Active compute slot mask | 1 | 1 |

The queue IDs, ring bases, and owned pointers differ, as expected for separate
allocations. The observed status values also occur during successful native
execution; none by itself identifies the Linux failure.

An earlier qualified sample on reused queue 11 / stamp 8 had `0x1688 = 3`
and `0x21028 = 8`. The first-stamp result shows those are not required initial
values. In particular, `0x8000000000` is not by itself evidence of a poisoned
completion register. Do not seed it or force the progress monitor to 3.
The reused-queue observation remains at `logs/m5-native-monitor-reference-hv-20260911/`
`native-monitor-1789102801822738000/observations.json`.

`trace-m5-native-monitor.py` observes at most two matching compute
publications, with an overall 128-submission bound. It samples the first
publication's queue again at the second publication; Metal may use a different
hardware queue for that second command. All observations are sequential,
not atomic. It refuses to replace another capture's breakpoint.

## Stop requests do not qualify frozen captures

An earlier mode8 attempt reached dispatch and incorrectly reported a frozen
snapshot. Later RAM contained a published recovery event. CPU controls were
zero, but statuses were GFX `0x6c` and GFX1 `0x68`.

The exact native ASCWrap `_isIdle` tests `CPU_STATUS & 3 != 0`.
`_runCPU(false)` clears control bit 4 then bit 5; it is not a verified debug
halt. A serialized provider getter now exposes control and status, and the
host reader's refusal was verified on the M5.

Mode8 qualification is disabled even if the conservative status mask matches.
It never emits the quiescent marker required by before-recovery readers.
Retained physical RAM remains useful evidence, but is not automatically a
view of firmware-private caches or the original stall state. Modes9/10 avoid
relying on a halt predicate.

## Remaining reference needed

A successful M5 compute example beginning with fresh GPU initialization,
including global setup, queue registration, initial completion state, memory
mappings, one minimal job and its hardware completion, would isolate missing
prerequisites. The current native witness supplies the job and program
mappings; the bounded workload now supplies a qualified first-stamp active
sample. The observed queue and pipe admission state matches Linux after
accounting for owned queue IDs and pointers. The next comparison must reach
deeper into shader execution and memory access, while retaining a complete
fresh-initialization reference as an independent way to find prerequisites.
An independent reference was requested in agents standup.

Kernel, modules and RAM payloads build. The 31 canonical capture tests pass,
including six tests executing the native sampler's actual AArch64 code
against synthetic power, bank-selection, and pipe-state changes.
Linux GPU execution, accelerated rendering, and DCP presentation remain
unqualified.
