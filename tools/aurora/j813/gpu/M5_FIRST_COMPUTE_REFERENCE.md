# M5 first verified Linux compute and native reference

The M5 now executes the controlled compute shader under native Linux. A fresh
RAM boot with `m5_initdata_test=10` verified all 1,024 `out = in * 3 + 5`
results, unchanged inputs and retired queues without a reported GPU restart.
The corrected program includes the required first 1 KiB of shader setup,
and register `0x14028` selects the same owned UMA pool as the populated table.
This qualifies one fixed compute job, not a render node, general shader
compilation, graphics rendering or DCP presentation.

## Controlled shader-page replay

The replay workload compiles `out = in * 3 + 6`, while its replacement test
checks `out = in * 3 + 5`. This distinguishes execution of the replacement
from successful execution of the original shader. Both CPU checks and the
same-descriptor completion capture verify all 1,024 output words.

| Program contents installed before native publication | Result |
| --- | --- |
| Original native +6 program | 1,024 correct +6 values |
| Original Linux pages: only the 64-byte shader body populated | 1,024 output sentinels untouched |
| Linux body in the otherwise intact native shader page | 1,024 correct +5 values |
| Native 128 bytes immediately before the body, plus Linux body | 1,024 output sentinels untouched |
| First 1 KiB of the native page with Linux +5 body; remaining page zero | 1,024 correct +5 values |

The pipeline and CDM pages already match Linux in full. Every replay
preflights all three program pages, their DRAM translations, exact native
contents, controlled input/output sentinels, and argument pointers. It
journals the originals, verifies replacement readbacks, and restores all
three pages at the matching completion before returning to the guest.
Restoration failure keeps the guest paused. The replay is opt-in; ordinary
SKSM capture remains read-only.

The first 1 KiB is sufficient for this controlled native job. The exact
instruction or data dependency within its preceding 960 bytes is not yet
isolated. This evidence does not prove Linux submission, GPU rendering, or
DCP presentation. The native regression after restoration passed 3,072
compute values and 12,288 rendered pixels.

Before the pool-selection correction, the rebuilt Linux mode10 probe left all 1,024 output words untouched
with unchanged inputs. Five stable downstream samples at polls 1, 2, 3, 4,
and 32 retain `a0a8=0`, followed by a type4 recovery event for queue11. The
shader-page correction is therefore necessary for the controlled native
replay, but was not sufficient to fix the Linux submission/initialization gap.
Evidence: `logs/m5-shader-prefix-linux-compute-20260911.log`.
The retained Linux capture `logs/m5-shader-prefix-linux-retained-20260911`
contains 33 objects over 243 physical pages, with no failed reads and all
hashes valid. Its complete shader page matches the successful native prefix
replay byte for byte, SHA256
`32f033d1c45c805abc61f7fa1a805847ac648ddc0377a7a6f0f1450ea07d15f8`.

Private workbench evidence:

```text
Baseline: logs/m5-program-replay-native-hv-20260911/sksm-submit-1789106749735284000
Original Linux pages: logs/m5-program-replay-fresh-hv-20260911/sksm-submit-1789107358396222000
Body only: logs/m5-program-replay-fresh-hv-20260911/sksm-submit-1789107613428386000
192-byte record: logs/m5-program-replay-fresh-hv-20260911/sksm-submit-1789107786651165000
1 KiB prefix: logs/m5-program-replay-fresh-hv-20260911/sksm-submit-1789107880418593000
Qualification: logs/m5-program-replay-shader-prefix-qualified-20260911.json
Regression: logs/m5-program-replay-native-regression-20260911.json
```

`m5_compute_program.rs` now initializes the measured first 1 KiB at page
offset zero. `verify-m5-compute-program.py` compares that full range, rather
than accepting only the 64-byte body at offset `0x3c0`.

To reproduce, build `m5-metal-reference.m` on macOS. Capture a successful
`M5_REFERENCE_SHADER_ADD=6 M5_REFERENCE_COMPUTE_ONLY=1` run with extra views
`[[0x10000000000,0x4000],[0x1000001c000,0x4000],[0x10000090000,0x4000]]`.
Then explicitly set `M5_SKSM_PROGRAM_REPLAY` to a JSON object containing
absolute `native`, `linux`, and `workload` paths, and the selected `variant`.
The Linux source capture is the original 64-byte-body retained image; its
three hashes are pinned in the replay helper for this controlled comparison.
`stamp` defaults to 1. An explicit `"captured"` uses the observed queue stamp
while preserving every exact program/context/buffer guard. Native queue IDs
and stamps can be recycled between processes.

Run the replay workload with `M5_REFERENCE_SHADER_ADD=6`,
`M5_REFERENCE_EXPECT_ADD=5`, and `M5_REFERENCE_COMPUTE_ONLY=1`. Use
`verify-m5-program-replay.py CAPTURE WORKLOAD_JSON` to qualify the result.
The verifier reports native submission separately from Linux qualification.

The optional `shader-prefix-linux-dispatch` variant also replaces the three
copies of the dispatch identity and its two descriptor fields with Linux's
`1/1` values. It preserves the native private-memory pool selection and
checks that selection against the shared-control owner. Each identity field
is restored separately, preserving unrelated completion writes. The native
replay `sksm-submit-1789110487839751000` verified all 1,024 replacement `+5`
results and all restoration readbacks. The subsequent native regression
passed 3,072 compute values and 12,288 rendered pixels.

## Private-memory pool selection

Register `0x14028` selects the owned UMA pool hardware ID. Seven native
captures with independent pool-owner records match this register to
shared-control `+8`. Captures that also include the UMA slot verify its
index and populated descriptor separately. All 30 captured host
descriptors match their signed `+0x788` field; 25G76 `generateRegisterList`
loads that field at `0x89e38dc` and emits the register at `0x89e3900..391c`.
The value varies between submissions and is not a fixed execution control.

Linux previously populated UMA slot 0 while emitting register value 2.
The probe now uses the same owned ID for the shared-control record and
register program. The pool verifier rejects a mismatched register selection,
and the register verifier derives the expected value from the independent
host descriptor. The corrected fresh Linux boot passed all 1,024 exact
outputs, unchanged inputs and both retired queues. The transaction and
both firmware CPU stop requests succeeded. Evidence is retained in
`logs/m5-owned-pool-linux-compute-20260911.log` and
`logs/m5-owned-pool-linux-retained-20260911`.
The latter contains 33 objects over 243 physical pages, with no read errors
and all hashes valid. Independent decoding of its retained buffer confirms
all 1,024 exact outputs, all 1,024 original inputs and no output sentinels.
Its register `0x14028` and shared-control pool ID are both zero.

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
the 1 KiB shader image, 132 pipeline bytes, 48 command-stream bytes, preemption,
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
profiles match native. That earlier job left all 1,024 output words untouched,
with unchanged inputs and eventual firmware recovery. Matching permissions
was therefore a measured correction, not a sufficient fix for execution.

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

These monitor values alone do not establish shader instruction start or
completion. That earlier mode10 run left all 1,024 outputs untouched before recovery.
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

## Additional DM2 progress monitor

The exact firmware's table at `0x44990` contains the indexed monitor
`[0xa010, 3, 0xa0a8, 0xa0a8, 0]`. Code `0x7c68..0x7c94` encodes its
selector directly in the AP read tag:

```text
(pipe_index << 44) | (1 << 43) | (3 << 40) | 0xa0a8
```

No write to the selector register `0xa010` is needed. Both samplers now
include this read after qualifying the compute pipe. Native format version 3
names the result `pipe_a0a8`; extended Linux mode10 returns six values:
`[slot_state, slot_progress, 0x1688, 0xa730, 0xa0a8, slot_state_after]`.

A native sample on queue 10 / stamp 2, slot 6 / pipe 0 returned `0x5e80` from
this monitor. Its 7.958-us observation passed all guards, and the workload
subsequently verified 131,072 values. The private record is
`logs/m5-native-first-active-hv-20260911/native-monitor-1789103902119094000/observations.json`.

On Linux, the first four samples returned zero in 18/16/16/16 us, with six
valid values, stable guards, and no recovery during the reads. The retained
capture `logs/m5-downstream-linux-retained-20260911` has 33 objects over 243
physical pages, no errors, and matching hashes. The actual retained shader,
pipeline, and CDM bytes match the native reference at their declared offsets.

Mode10 now attempts later observations at polls 32/64/128/256, in addition
to its first four; mode9 retains four early observations. A fresh M5 run
reached poll 32 before recovery. All five qualified samples still returned
zero at `0xa0a8`. Later polls were not reached. Logs:
`logs/m5-downstream-late-compute-20260911.log`.

Both Linux runs left 1,024 output words untouched with unchanged inputs and
eventual firmware recovery. This establishes a reproducible difference in
the observed workload states, not a register meaning or a root cause: native
uses a longer workload and different queue history. Do not infer a specific
shader-fetch or memory fault from the monitor value alone.

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

## Remaining graphics work

The fresh Linux compute result now supplies the previously missing minimal
execution witness. Repeated jobs and completion handling, general shader
compilation, graphics submission and render-node integration still need
qualification before testing DCP presentation and XFCE/glxgears.
The controlled native reference remains useful for each independent step.

Kernel, modules and RAM payloads build. The 48 canonical capture/replay
tests and five Rust compute-codec tests pass. Linux execution is qualified
for this one fixed compute job; accelerated rendering and DCP are not.
