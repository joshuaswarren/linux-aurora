# M5 / J813 GPU bring-up

## Indexed Vulkan rendering, 2026-09-12

The M5 application path now passes a nine-frame sweep of 1 through 160
full-screen indexed triangles, checking every pixel and both queue completions.
Matching the native auxiliary-framebuffer control fixes the fragment recovery
previously triggered at 64 triangles. See [M5_INDEXED_RENDERING.md](M5_INDEXED_RENDERING.md)
for the controlled comparison and remaining qualification gates.

## Earlier compute bring-up

The native Linux candidate identifies the physical T8142 GPU as
**G17G revision C0, one die, one cluster, ten active cores**. Both M5 GPU
firmware processors now complete RTKit startup and acknowledge Linux-owned
initialization data in the native RAM-boot experiment. Linux also registers
an owned SKSM compute queue, binds its scheduler control, and verifies the
firmware's retained records.
An isolated execution context, corrected shader setup and owned compute pool
now execute verified SKSM compute programs under Linux: all 1,024 outputs match,
inputs remain unchanged, and both command queues retire without a reported
GPU restart. Register `0x14028` must select the populated UMA pool slot; the
earlier probe owned slot 0 but incorrectly selected slot 2.
This is a bounded fixed-program diagnostic. It does not expose a render node
or qualify general shader compilation, vertex/fragment rendering or DCP presentation.

## Linux compute triangle, 2026-09-11

A fresh mode11 RAM boot rendered a 32x32 colored triangle using a compute
shader. An independent edge-equation check of retained GPU memory verified
all 1,024 pixels, including 288 triangle pixels and 736 background pixels.
All inputs remained unchanged, command queues retired, and both firmware stop
requests succeeded. The retained shader, pipeline and constants match the
compiled profile. The original arithmetic program remains selectable.

The test image was posted to Discord. It enlarges each GPU pixel 16 times
without filtering; it is not a 512x512 GPU render or a DCP screenshot.
See `M5_TRIANGLE_REFERENCE.md` for reproduction and evidence.

## First Linux compute, 2026-09-11

The fresh mode10 RAM boot passed the exact `out = in * 3 + 5` check, followed
by successful firmware stop requests with all memory owners retained.
Native replay independently verified the corrected shader image and Linux's
dispatch identities. The pool verifier now rejects a register/owner mismatch,
and 48 capture/replay tests plus five Rust codec tests pass.
See `M5_FIRST_COMPUTE_REFERENCE.md` for the causal comparison and evidence.

## Earlier hardware evidence, 2026-09-10 and 2026-09-11

- A fresh RAM boot with `m5_initdata_test=7` retired compute descriptor
  tag 3 and channel configuration tag 15, leaving tag 14 unpublished.
  Outer counters reached `[2, 2, 2]`, inner counters `[3, 3, 3]`; inputs
  were unchanged and all 1,024 outputs retained their sentinels. Both
  firmware CPUs stopped successfully. A 21-object, 85-page RAM capture
  matched the post-recovery descriptor, event state, scheduler and program
  buffers. GFX remained powered down, so the guarded helper refused queue
  MMIO. This verifies setup before SKSM execution, not a completed job.
- Follow-up tests on September 11 confirmed a primary type-4 GPU restart
  report after the compute kick, first in retained RAM and then in a live
  report-ring drain. The report names affected stamp index 11; status B
  records reason 0, hardware summary 1 and recovery epoch/state 1. The
  exact fault source remains unresolved. No recovery acknowledgment is
  sent, and both firmware CPU stop requests succeed. The earlier absence
  of a firmware CPU crash did not establish an absence of a GPU fault.
- Primary M5 firmware tracing is hardware-qualified through status-A `+0`.
  The `m5_fw_trace` parameter selects decoded classes in the owned host
  field before initdata publication. A value of `3` produced native queue,
  kick and recovery-transition records. Report consumers advance within
  their measured ring bounds; ordinary processing-loop brackets do not
  consume the print budget. Stopped RAM identifies data master 2, slot 0
  in the recovery classifier. See `M5_FENDER_SETUP.md` for exact evidence.
- Trace mask `0x203` additionally verifies the firmware's stalled-work
  classification. Guarded reads with both firmware CPUs stopped confirm
  hardware slot 0 contains the owned command and queue pointers, with
  state `0x1000d` and queue-11 stamp 1. The optional queue-selector read
  restores its previous value. These results establish hardware admission;
  all shader output sentinels still remain untouched.
- A native macOS AP trace identified two missing setup writes. Fresh Linux
  tests verified Fender bits 1 and 2 cleared (`0x6` to `0x0`) and PMGR
  control matched (`0x30000` to `0x30001`). Both firmware roles and the
  queue still initialized, but these changes did not produce shader output.
- Two fresh RAM boots with `m5_initdata_test=6` submitted one SKSM compute
  kick after the previous queue, scheduler and context checks passed. Outer
  counters reached `[2, 2, 2]`, inner counters `[4, 4, 4]`, and the firmware's
  added-kick counter reached one. No firmware crash was observed. The second
  run waited 10,000 polls: completion cursor stayed zero and all 1,024 output
  sentinels remained untouched. Both firmware stop requests succeeded.
  The runner intentionally reports failure until exact shader output exists.
- The owned 4-MiB page directory and 32-byte M5 UMA slot were verified against
  a completed native Metal capture, then read back from the stopped Linux
  probe. All 7,168 initial addresses and all descriptor words matched. M5's
  UMA packing differs from Neo's. Initial directory construction belongs to
  the host; an opcode-20 growth request is not used for initial registration.
- Native startup observations retain the same initial reserved-DATA status
  values during working macOS GPU operation. Those snapshots cannot prove
  an unpowered GPU. The Linux probe logs later state transitions without
  using these values as a power-readiness gate or modifying firmware DATA.
- `verify-m5-pool-capture.py CAPTURE --self-test` verifies the native descriptor,
  initial directory, allocation bounds and guards; it rejects the earlier
  empty-directory and Neo-packing mistakes. `capture-m5-linux-compute-state.py`
  reads the stopped probe's retained RAM through its live page tables. Its
  physical reads are limited to System RAM and measured GPU reservations.
- A fresh RAM boot with `m5_initdata_test=5` additionally prepared context 3
  with an independently allocated lower root and empty upper root, both
  tagged with ASID 3. After both initialization acknowledgments, control
  openings, queue registration and scheduler binding, the root pair remained
  current and all 1,024 inputs and output sentinels remained unchanged.
  Both stop requests succeeded. This verifies host preparation and retention;
  this mode submits no shader. Mode 6's execution failure is described above.
- A fresh RAM boot with `m5_initdata_test=3` registered SKSM queue 11 using
  one tag-15 configuration. The outer and inner counters each advanced to
  `[1, 1, 1]`; the firmware DATA record changed from zero to
  `[1, fffffc2000390000, 70005f0000, 400000000, 2]`. This independently
  verifies both aliases, priority 2 and compute data master 2. The probe
  submitted no kick, shader, scheduler binding or context update. Both
  firmware stop requests succeeded and all owners remain pinned.
- Another fresh RAM boot with `m5_initdata_test=4` additionally bound the
  owned scheduler control. Its firmware pointer/header/queue bitmap changed
  from `[0, ffff, 0]` to `[fffffc20c0007200, 2040000, 800]`: both scheduler
  slots allocated as zero, state 4, priority 2, and queue-11 bit set. Both
  rings again completed and both firmware stop requests succeeded. This
  still submits no shader and does not attach a user context or event.
- A fresh RAM boot with the exact T8142 ASCWrap-v6 providers completed both
  roles' HELLO/EPMAP exchanges: protocol 12, endpoints 0, 1, 32 and 33.
  Each provider delivered three receive interrupts through AIC3. GFX1 was
  started before GFX, matching the native M5 trace. Both stop requests
  returned success; owners remain pinned until reboot.
- A second fresh RAM boot completed full RTKit startup for both processors:
  each reported running, no crash, and GPU endpoints 0x20/0x21 available.
  The firmware DATA reservations and native crash-buffer locations were
  retained. No initialization descriptor or GPU command was submitted.
- A subsequent cold RAM boot built the complete M5-owned initialization
  graph and received the exact `0x0009000000000000` acknowledgment from
  GFX1, then GFX. Both private firmware roots and their shared L2 were
  preserved. The probe completed in 52,972 microseconds with no observed
  firmware crash. Stop requests succeeded and the module remains pinned
  until reboot. No GPU job was submitted.
- A further fresh boot repeated both acknowledgments, then sent the single
  native `0x89` power-transition-end note to GFX followed by GFX1. Both
  control queues advanced from `[0, 0, 1]` to `[1, 1, 1]`, consuming their
  measured opening records. The complete probe took 63,184 microseconds
  with no observed firmware crash. This still submits no GPU job.
- U-Boot's default four-bank limit had discarded GPU page-table and DATA
  reservations. `apple_m1_defconfig` now uses `CONFIG_NR_DRAM_BANKS=32`,
  matching m1n1's limit. Linux retains all eight handed-off banks and can
  borrow the reserved root pages. This does not widen RAM over other
  reserved regions such as the framebuffer.
- Linux `7.1.6-aurora-j813-gpu-v9`, Asahi diagnostic module loaded explicitly.
- GPU ID words: `0b022000 000b0800 0011010a 00040404 01320311 10030302`.
- The M5 ADT selects a 43-bit virtual address (42 bits per root plus one
  root-select bit), matching the Neo's geometry. The diagnostic driver's
  42-bit input-root field is not a different virtual-address width. Output
  addresses are 42 bits. G17G firmware/runtime admission is still separate.
- Both GFX roles' loaded TEXT and DATA were captured twice, with matching
  reads, before Linux. Both report `RTKit-3255.160.4.release`.
- Six boot-local UAT/firmware DATA reservations are passed through m1n1.
- GPX and GFX physical power domains are represented with their measured
  dependency. GPX refused power-gating after the diagnostic probe; the next
  diagnostic DT pins it on pending a traced shutdown sequence. That DT change
  is not a power-management qualification.
- Native Wi-Fi obtained DHCP and returned 5/5 pings to `8.8.8.8` after the
  complete N1 startup sequence. Bluetooth initialized with zero TX/RX errors;
  pairing/audio were not repeated. MTP keyboard and trackpad initialized.
- Only simpleDRM `card0` exists. There is no `renderD128`, compute result,
  hardware-rendered frame, or DCP acceptance from this candidate.

The M5 initdata transaction requires the exact captured firmware profile.
It owns the UAT and both firmware roles independently of the Neo manager.
GPU execution, completion and a registered DRM runtime remain open.

## Reproducible diagnostic candidate

`build-m5-gpu-kernel.sh` uses the J813 Lima builder and a separate build output.
It enables Rust/Asahi, device coredumps and built-in Apple mailbox support,
rebuilds the peripheral modules, and retains the qualified v8 files.
`build-m5-gpu-payload.sh` packages a separate RAM payload with Asahi automatic
loading blacklisted. `check-m5-gpu-dtb.py` checks every pre-existing peripheral
property and reference against the qualified v8 DTB.

RAM-chainload the matching m1n1 J813 GPU handoff carrier using the existing
guarded J813 path. `boot-linux.py --gpu --gpu-inventory` checks current-boot
resources, captures loaded firmware in private host files and boots the
candidate. Run `modprobe asahi` explicitly from the native shell to collect
the identification result. A successful module load is not a bound GPU driver:
the diagnostic probe intentionally returns `ENODEV` at the firmware boundary.

For radio regression, use the established N1 startup and protected WPA
configuration, then open normal data ring 25 **once** on the fresh boot.
After association, obtain DHCP with `udhcpc -B -q -f -n -i wlan0
-s /bin/n1-udhcpc -t 3 -T 3`, then run the Wi-Fi-bound ping. Omitting ring 25
leaves receive traffic working but no normal transmit submissions.

`gpu-boot-preflight.py` rejects changed register layouts, missing physical
power parents, overlapping/out-of-RAM reservations and invalid firmware
segment ranges. `test-gpu-preflight.py` exercises the reservation failure
cases. Boot addresses must always come from the current ADT.

## M5 tracing

`launch-m5-gpu-hv.py` uses the already qualified M5 headless macOS carrier and
kernel, retaining resident Stage1 and boot policy. `trace-m5-gpu-boot.py`
observes both ASC mailboxes and GPX/GFX registers before guest entry. Its
asynchronous callbacks only record supplied events; they issue no nested
proxy requests. It does not use the Neo init-data decoder or override the M5
firmware version. Raw traces and firmware binaries remain local.

Both firmware roles were captured successfully before publishing their
initialization descriptors on a cold boot of macOS 25G76. Every object matched
on two reads, and both roles' complete 0x28000-byte shared clusters were
identical. `--capture-init` temporarily makes only each INBOX0 payload write
synchronous, captures the referenced DRAM, then removes that override before
INBOX1 publication. Raw captures stay private.

The actual endpoint-0x20 initialization messages have type 0x81. Endpoint 1
is the RTKit crashlog buffer. Firmware pointers retain their 43-bit address,
including the global-root selector. The working macOS guest uses the separate
SPTM global root reported by the current boot receipt; its firmware mappings
do not appear in the ordinary per-context TTBR pairs.

`m5-metal-reference.m` verifies compute results and rendered pixels through
three fresh Metal command queues. Thirty-two macOS runs each passed 3,072 exact
compute values and 12,288 RGBA pixels. This is a macOS reference, not evidence
that Linux submission, rendering or DCP works.

The bounded SKSM observer now follows the selected command's actual UAT
context, CDM stream, pipeline and explicitly requested shader/data pages.
An optional one-shot hook at the matching retained CL descriptor's completion
captures those pages again. Nine controlled captures each show 1,024 output
sentinels before submission and 1,024 exact results at the matching callback;
the input remains unchanged. All captured objects matched on two reads.
The observer never reads device memory and retains a two-MiB read budget.

The optional owner capture records physical page walks as well as contents.
M5's native compute context 3 has an empty upper root. The descriptor and SKSM
ring's lower aliases in context 0 match their firmware-global physical pages;
those same addresses in context 3 have different backing. Event-state links
also have guard pages and distinct allocations. Linux must retain separate
owners instead of treating these virtual addresses as interchangeable.

`m5_compute_program.rs` constructs the controlled shader, pipeline, CDM stream,
arguments, input/output and robustness pages. `verify-m5-compute-program.py`
compares the defined regions against the private native captures and validates
completed results using the production checker. All nine completed captures
match. This does not establish that every command dependency is covered or
that Linux can execute the shader.

The controlled channel item ring contains the register program (selector 3),
queue configuration (tag 15), then SKSM kick (tag 14). Its producer was 2
before tag-14 publication; the consumer and producer were 3 at completion.
M5 builds tag 15 inline in `AGXCLChannelSKSM::submitBuffer`; observing the
separate generic configuration function missed that path. The production
`m5_channel.rs` writer matches all 134 configuration bytes, independently
taking queue fields, descriptor flags and the scheduler pointer from their
native owners. Remaining event/owner values are opaque inputs. Disabled event
sentinels also follow the native branch. Queue installation now passes on
Linux, followed by scheduler first-bind. Mode 6 exercises context and event
updates, but their effect on successful shader execution remains unqualified.

`verify-m5-channel.py` checks that byte match against a private capture.
`test-m5-sksm-capture.py` checks capture guards, filtered submissions and
matching-descriptor completion. The global-root and per-context walkers stay
separate; neither substitutes a guessed page table. The sampled completion
ring was zero before and at the host callback. Its routing and consumption
timing remain unknown; no completion-ring ABI is claimed from that observation.

The full 4096-byte simple-compute descriptor prefix now matches ten native
captures, including two contexts and independent host descriptor controls.
This covers both register arrays, low aliases, completion-stamp addresses,
status pointers and the native opaque event metadata. In particular, the
32-bit work ordinal at `+0xf70` is distinct from the low byte packed into
register context words; truncating it fails the background-queue captures.
Run `verify-m5-compute-registers.py --descriptor CAPTURE...` to reproduce the
comparison. This is the measured simple-compute profile, not an allocation
size claim or qualification of indirect dispatch and sampler variants.

A matching-completion capture also shows exactly one 24-byte outer CL entry
published and consumed in priority 2: the cached queue pointer, data master 2,
head 3, queue ID and start flag match the controlled job. The firmware's own
tag-15 install branch retains the queue aliases and programs SKSM hardware.
The retained Linux mode-3 experiment now exercises that queue-install branch.
Job execution and completion handling remain unqualified.

## Source construction and M5 differences

Linux now has explicit J813/25G76 serializers for both roots and main objects,
the complete hardware-data block, four completion queue descriptors, the
shared policy allocations, region C and primary/secondary status objects.
The root version is `0x0c8aa322039e04c0`; both loaded firmware
images independently compare that value. The existing Neo runtime continues
to use `0x0c8aa322039604c0` and rejects the M5 version.

`verify-m5-gpu-codecs.py` rebuilds the configuration objects and all 53
register-table slots against independent cold-boot captures. It checks
the seven native allocation sizes in the pinned host binary, validates
pointer rebinding and rejects overlapping backing before modifying output.
It also emulates the exact pinned
M5 host reset routine with synthetic allocations and compares its output
against the Linux channel codec across 32 cases. All comparisons pass.
The driver/module build passes; the explicit M5 initdata transaction now uses
these codecs and has received both firmware acknowledgments in Linux.

`drivers/gpu/drm/asahi/m5_resources.rs` now constructs and retains the M5
initialization memory: both roots, the complete shared/private arenas, four
completion queues, six physical alias pairs, policy backing and the 22 I/O
windows. Firmware pointers are authored from owned allocations. Native
cacheability and permissions are preserved, including read-only register
windows and the 64 KiB extent of slot 39. The two measured control openings
are initialized; subsequent compute/render descriptors are not submitted.
The constructor requires an M5 UAT and has been exercised on Linux hardware.
`m5_live_boot.rs` retains that UAT, the memory graph and both RTKit owners.
This validates initialization only, not GPU jobs or cache-flush behavior.

The native UAT inspection now reads both role-private roots, their shared L2,
TTBAT, global root and handoff through their existing EL2 mappings. The
generic non-cacheable alias does not cover every reservation; an alias
translation fault does not establish physical access protection. The entire
16 KiB handoff page remained zero before and after a verified Metal workload.
This observation does not yet qualify Linux handoff or cache-flush behavior.

## Linux firmware transport experiment

`linux-rtkit-probe/` is an explicitly loaded external module for fresh RAM
boots. It validates both M5 providers and DATA reservations, arms both RTKit
receivers, then starts GFX1 and GFX. Its default mode answers only HELLO and
EPMAP. `full_boot=1` completed the complete RTKit management startup in a
separate cold test. Neither mode submits initdata or GPU jobs.
After a CPU start, owners and the module remain pinned until reboot.

Run `build-m5-rtkit-probe.sh` in the J813 Lima builder against the candidate
kernel, then use
`build-m5-rtkit-probe-payload.sh` and
`boot-linux.py --gpu --gpu-rtkit-probe --gpu-inventory`.
The separate payload enables only the two mailbox nodes; it preserves the
base diagnostic payload and leaves Asahi automatic loading disabled.
Load the probe manually from the native Linux shell after verifying the
current-boot firmware capture. Never load it alongside Asahi.

`m5_firmware_identity.py` identifies the observed 3255.160.4 loaded TEXT pair.
Only the exact eight-byte values of the STKG and PACE boot TLVs vary across
the compared boots. Their offsets, tags and lengths remain in the hash,
along with every executable byte. `verify-m5-firmware-identity.py` checks
private captures and rejects code, metadata tag/length, tail and role changes.
The transport payload's preflight requires this identity; ordinary read-only
inventory can still capture an unknown build without granting runtime use.

## Linux initialization-data experiment

Build the candidate kernel/payload and the separate provider-enabled payload
above, using the J813 U-Boot with `CONFIG_NR_DRAM_BANKS=32`. From a fresh
RAM chainload, run `boot-linux.py --gpu --gpu-initdata --gpu-inventory`, then
`modprobe asahi m5_initdata_test=1` in the native shell. The loader verifies
both loaded firmware TEXT images and places the exact profile receipt in
`/chosen`; the driver checks it before touching GPU resources.

This mode starts GFX1 then GFX, builds and retains the M5 graph, submits
GFX1's root before GFX's root and waits for each exact acknowledgment. It
requests both CPU stops afterward and retains ownership until reboot,
including on failures after firmware start. Do not unload it, load the C
RTKit probe alongside it, or restart either processor in that boot.

For the next bounded stage, use `modprobe asahi m5_initdata_test=2` on a
fresh boot of the same payload. After both acknowledgments it sends one
native power-transition-end message to GFX, then GFX1, and checks both
control queues consumed their opening records. It adds no queue records
or GPU commands and retains the same stop-and-pin lifecycle.

For the queue-install stage, use `modprobe asahi m5_initdata_test=3` on a
fresh boot. It constructs all owned queue backing before publishing initdata,
then retires the control openings and publishes one tag-15 record through
CL priority 2 with doorbell `0x008300000000000a`. Scheduler and context
updates are disabled. `run-m5-queue-install-probe.py` captures the console
and requires both consumed counters and the firmware-owned queue record.
Reserved firmware DATA uses its normal WB RAM mapping with read-only cache
invalidation before observation; a WC alias of System RAM is invalid.
The same stop-and-pin lifecycle applies. This test submits no GPU job.

Mode 4 additionally sets `update_scheduler=true`. The firmware must store
the owned control pointer at its queue-indexed table, allocate the control's
two slot IDs, set state 4 and mark queue 11 in the control's bitmap. Run
`run-m5-queue-install-probe.py 4` after a fresh RAM boot. This qualified stage
still disables context updates and the optional event machine; it submits
no register program or SKSM kick.

Mode 6 constructs the retained initial pool and issues one controlled SKSM
compute kick. Run `run-m5-queue-install-probe.py 6` after a fresh RAM boot.
It requires all 1,024 exact results, unchanged inputs and retired command
queues, so the current stalled hardware result exits with failure. Do not
restart or unload this retained session. Capture its RAM with
`capture-m5-linux-compute-state.py INVENTORY_JSON OUTPUT_DIRECTORY` before
rebooting; `M1N1_DIR` may select the host's M5-capable proxyclient tree.

The firmware handoff page is observed to remain zero in macOS. The M5-only
no-peer handoff path has now passed initialization; it does not establish
working cache flushes, power management, command submission or rendering.

The latest complete cold capture matches both 160 KiB shared clusters and
the 0x178000 private arena, after excluding only independently identified
firmware-written fields. GFX1 stores status A+0x10/+0x14 through global
0x16f4e0 at firmware text+0x23588/+0x23604. These counters begin at zero;
they can already be populated when primary initialization follows GFX1.
PB/UMA descriptor pages and firmware-control storage are initially zero.
The blank sentinel at primary main+0x2d0 is initially unmapped, becoming
zero-backed later; it must not be confused with those descriptor tables.

`snapshot-m5-gpu-owners.py` walks page tables without reading device memory.
Its latest snapshot verifies all 58 I/O pages and 22 following guard pages,
plus 34 pages of low/high aliases sharing identical physical backing.
Pass its manifest to `verify-m5-gpu-codecs.py --owner-snapshot PATH` to
check these relationships alongside the codecs and 38 production unit tests.
Raw page tables, captures and firmware binaries remain private.

The M5 profile handles these differences explicitly:

- The live M5 host resets a 192-byte cached channel record, moving fields to
  +0x44, +0x48 and unaligned +0x9c. The older shared 128-byte reset is unsuitable.
- Register slot 39 carries distinct size fields, 0x10000 and 0x8000. The older
  serializer duplicated a single size and cannot reproduce this table.
- M5 requires four 128 KiB completion backings, including ordinals 1 and 3.
  The two-role capture found all four, and the 0xe40-byte QoS backing, zero
  before the respective initialization messages.
- Primary main-config fields +0x32c/+0x344 and secondary +0x300 differ.
  Region C contains different platform values and a pair moved by 0x20.
- Native allocation descriptors establish the hardware block size as 0x2708
  and the six following sizes as 0xc18, 0x1048, 0xe10, 0x6a30, 0x68 and 0x800.
  Applying the native 64-byte rounding places the next allocation at +0xc500.
- Shared policy +0xb75c is the host's `mach_absolute_time()` value; +0x8ed8
  and +0xb6a0 are caller-owned state/AON PTD pointers. None is a static value.
  The native AON PTD allocation is 0xe4 bytes. Its measured contents match
  both roles on the third cold boot and remain unchanged after Metal work;
  Linux must allocate and retain the backing through firmware shutdown.
- Native captures include both GFX1-first and GFX-first initialization.
  The qualified Linux sequence starts GFX1 then GFX. GFX1 takes root+0xb8 as its
  shared configuration and writes +0x670/+0x678/+0x680 after startup. These
  must begin at zero. A later primary capture can already contain secondary
  runtime state, despite preceding the primary's own root handoff.

The measured ten-core J813 policy is target-specific. Its opaque calibration
words have not been generalized to other M5 machines or derived from ADT.
Read-only page-table walks verified firmware VA 0xfffffc2180080000 maps to
physical SGX 0x580000000 and register slot 39 maps to 0x580e08000. The value
at hardware-header +0x28 is unmapped in the global and sampled context views;
it must not be treated as an observed MMIO mapping.

The M5 driver uses HAL300/SKSM queue classes. During the passing reference
workload, the primary ASC emitted endpoint-0x21/type-0x83 work doorbells,
including three `0x8300000000000a` compute kicks. The observed CPU FIFO at
SGX+0x4090/+0x4098 stayed quiet. This establishes a workload mailbox path;
it does not prove every M5 workload uses the same transport or that the Linux
path is interchangeable. Neo's HAL200 runtime remains a useful reference.

A hardware breakpoint in the exact 25G76
`AGXCLChannelSKSM::submitBuffer` captured a native CL tag-14 add-kicks record,
its retained host queue, and a stable 128 KiB kick ring. The queue has 256
entries with a 0x200-byte stride. The host writes tag 14 at +0, QID at +4,
count at +6, queue priority at +8, old stamp at +9, and CL data master 2 at
+0x11. Thus an SKSM workload can publish its kick notification through a
firmware channel; the mailbox path alone does not imply classic compute.
The KSM queue priority is separate from the outer work-channel priority.
`m5-metal-reference` optionally reports its input/output GPU buffer addresses
to stderr when `M5_TRACE_BUFFERS` is set, for private payload correlation.
Build this reference on the controller and copy it to the guest; compiling
inside one traced guest triggered an XNU page-table refcount panic.

`m5_compute.rs` encodes the measured 40-write M5 compute register program.
Three captured programs match all 480 bytes. One capture additionally saves
the native host descriptor and verifies its control inputs independently:
registers 0xd4a1 and 0xa599 depend on descriptor +0x530/+0x538, and 0x1a458
depends on +0x60a/+0x60b flags. These controls vary between workloads; they
are not fixed M5 properties. M5 also uses larger operand-state offsets.
`verify-m5-compute-registers.py` labels older captures whose control inputs
must be recovered from encoded output rather than a separate descriptor.

`m5_sksm.rs` constructs the complete 512-byte HAL300 entry, including the
two-bit dependency types at +0x180 and the M5 header's additional flag.
It checks queue capacity, generation wrap, packed addresses and dependency
bounds before producing a result. `verify-m5-sksm.py` matches four native
entries byte-for-byte, their packed 21-byte tag-14 records, and the separately
captured producer stamp/count. Variable command operands come from captures;
this validates serialization, not shader execution or Linux queue ownership.
The diagnostic runtime now constructs owned backing, binds its scheduler and
context, and publishes in native order. Hardware execution and completion
remain the next qualification boundary.

`trace-m5-sksm-submit.py` captures one such submission on an existing paused
M5 hypervisor owner. It checks native instruction bytes, bounds CPU reads to
DRAM, and reads the ring through the retained firmware alias and the recorded
SPTM global root. The saved CPU ring view can be unmapped at publication.
The helper removes its breakpoint on capture or failure and supports bounded
queue filters. `test-m5-sksm-capture.py` checks its mapping, profile, geometry,
EL0/EL1 translation and filter guards without hardware. Payload-page views
are observations, not a qualified command decoder. Raw captures stay private.

Controlled Metal references pass 3,072 compute values and 12,288 pixels across
three queues. Selected captures now include the program bytes and the same
descriptor's verified output at completion; background macOS work is filtered.
Linux M5 command submission reaches firmware accounting, but shader execution
and hardware completion remain unproven. Neither GPU rendering nor DCP has
been qualified on this M5 candidate.
