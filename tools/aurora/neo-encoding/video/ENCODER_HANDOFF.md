# Active AVE2 encoding work

New 2026-09-09 checkpoint: ProRes 422 also passes 60 moving frames in native
Linux. Read `OTHER_CODECS.md` first for the APR driver and current shared
machine state. The old native boot/client numbers below are historical.

## Current result: 60-frame H.264 and HEVC video, MP4 and Matroska

Both codecs passed on native Linux with the same module and boot. Each
encoded 60 moving 320x240 NV12 frames at 30 fps. Both containers hold two
seconds of video and decode to the same pixels as their elementary stream.
Minimum per-frame PSNR: H.264 60.18 dB, HEVC 63.41 dB. Stop/Close, all six
CPUs and Vulkan's 32 exact values passed after each session.

Authoritative results:
- `../artifacts/ave-linux-hevc60-20260908/validation.json` (client 3).
- `../artifacts/ave-linux-h264-transport60-20260908/validation.json` (client 4).
- `../artifacts/ave-video-qualified-20260908/` freezes the matching module,
  unstripped ELF, DTB, source, captures, logs, outputs and hashes.

Current target: native Linux run `../logs/20260909T033655Z-native-linux/`.
ProRes has 62 completed frames and remains healthy and pinned. H.264 client 1
and HEVC client 2 each passed three-frame regressions and closed cleanly.
A final ProRes frame also passed after those AVE sessions. KIS is free;
the next AVE client is 3. Read `OTHER_CODECS.md` before taking KIS. Do not reboot just to demonstrate it,
force-unload the pinned module, or open an m1n1 proxy against Linux.
The old macOS HV controller 55332 is closed. Its HEVC reference capture is
preserved; no new capture is needed to reproduce these fixed profiles.

Read `WORKING_LINUX_ENCODING.md` and `HEVC_COMMAND_NOTES.md`. Run
`encode-linux-video.py NEW_DIRECTORY --codec h264|hevc --frames 60`.
The implementation remains a fixed-profile experimental debugfs path;
standard V4L2/FFmpeg encoder registration, arbitrary resolution and a
continuous streaming API are unfinished. Timings include host transfers
and validation and are not throughput benchmarks. Persistent Stage 1 and
the macOS update state remain unchanged.

The qualified module SHA-256 is
`9432ecf841f2eb5398a56c8df2b4d04321b71e267b39f45361ddef3bbee15245`.
HEVC required 128 KiB command writes, 31 Start relocations, a 256-entry
tile-address table and four Process surface addresses. Slice headers and
CABAC payload are joined from separate live Linux DMA buffers. Channel-1
ACKs have zero payload length. The initial wrong ACK check was corrected
from saved native evidence before the successful boot. An extraction-only
fix reused captured Linux buffers, without repeating encoding commands.

Read the full `../../neo-update/PROGRESS_WEBHOOK_GUIDE.md` after compaction.
The first two-frame, 60-frame H.264, and final H.264/HEVC milestones have
HTTP 204 delivery records; do not duplicate them. Final record:
`../logs/ave-multicodec-webhook-delivery.json`. The frozen archive's 1,922
files (566,901,325 bytes) were read back and verified against its manifest.

## Historical first two-frame H.264 checkpoint (superseded above)

**Two different 320x240 NV12 frames have now been encoded by the Neo under
Linux and independently decoded correctly with FFmpeg.** Gray: 74.66 dB
PSNR; color bars: 70.27 dB. The second session reused the same boot. Config,
Open, Start, Process, Complete, Stop and Close all passed. Both sessions
closed cleanly. Six CPUs and Vulkan's 32 exact values passed after encoding.

Read `WORKING_LINUX_ENCODING.md` for the complete recipe and limitations.
`encode-linux-frame.py` is the reusable host-side command; it drives the Neo
hardware and saves exact packets, raw output, decoded pixels and validation.
The current target is native Linux boot `../logs/20260908T221213Z-native-linux/`,
with firmware configured and idle after client 2 closed. The module, DMA and
five encoder power-domain references remain pinned until reboot. KIS is free;
do not reboot merely to demonstrate success, force-unload, or open a proxy.
Another encode selects client 3 automatically and needs no build or reboot.

Evidence: `../artifacts/ave-first-linux-frame.h264`,
`../artifacts/ave-linux-color-bars-20260908/validation.json`, and the complete
frozen build/evidence bundle `../artifacts/ave-encoding-qualified-20260908/`.
This is a fixed-configuration experimental AVE path. Dynamic parameters,
continuous streaming, and V4L2/FFmpeg encoder registration remain unfinished.
Do not mistake the working dedicated AVE engine for an AGX shader encoder.

The green two-frame hardware encoding milestone was delivered HTTP 204;
`../logs/ave-encoding-webhook-delivery.json` records delivery. Read the full
progress guide after compaction and do not duplicate this milestone.

## Historical IPC/encoding campaign notes (superseded above)

**LATEST (supersedes intermediate notes below): Config and Open now complete.**
Boot `../logs/20260908T220034Z-native-linux/` with captured startup scalars
completed full IPC, Config and Open. Config reply exactly matches macOS;
Open matches after replacing its client token with Linux-local 1. See
`ave-config-captured-startup.log` and `ave-first-open.log`. Matching firmware
startup scalars (including page-aligned log size 0x24000) resolved Config's
assertion. This test changed the active-unit scalars too, so it does not
independently isolate log alignment as the sole cause.

The first relocated AVC Start was transported but firmware crashed on an
encoder-register access: FAR `0x285450004`, PC offset `0xfe870`, ESR
`0xbe000000`. No Process command/frame was submitted. Logs:
`ave-first-avc-start.log`, `ave-start-exception-and-health.log`.
Linux and the 32-value Vulkan compute test remained healthy. Live genpd
summary proved `venc0_pipe4`, `pipe5`, `me0`, `me1` all OFF while DMA/SYS were
ON (`ave-start-power-state.log`). The next candidate attaches/resumes all
five encoder domains by their existing Linux providers, with reverse-order
release only before firmware starts. It also corrects channel-2 echo flags
to zero, matching AVE_IPC::Send; incoming flags are still preserved in logs.

**CURRENT BOOT IN PROGRESS:** `../logs/boot-linux-ave-powered.log`; inspect its
run directory and completion before touching KIS. Builder passed module and
semantic DT validation. Existing DT changes are only three newly referenced
power-domain phandles plus corresponding panel phandle renumbering.

The running candidate now exposes `ave-boot-config` (64-byte readback),
`ave-arena` (bounded DMA staging), and `ave-poll` (one-second drain for later
firmware events, results read through `ave-command`). After this boot, push
the new module and test scripts, run cold lifecycle + IPC, Config then Open,
verify genpd engines ON, then regenerate AVC Start with this boot's arena
DMA base. `ave-relocate-avc.py` validates capture hashes and explicitly
relocates 37 Start fields / four Process fields into owned memory; neither
its Start acceptance nor Process/encoded output is proven yet. Input NV12
is 320x240, Y stride 320, Y bytes 76800, UV bytes 38400, arena offset 0x400200.
Do not reuse an old generated packet if its arena DMA base changes.

Full mailbox negotiation and both 64-bit command rings now work on native
Linux. Two boots reached `ipc_ready=1`, rings `IO` (992 slots, interrupt 1)
and `IO_T2H` (993 slots, interrupt 3). Firmware wrote and returned validated
descriptors; its ready flag cleared. First Config packets received channel-1
transport ACKs, then firmware asserted at `PlatformIOPIPCManager.cpp:291`:
`size != 0 && (size & (RTK_MMU_PAGE_SIZE - 1)) == 0`.
Config has NOT completed and no Linux frame has been encoded.

Do not assume the assertion specifically means zero: it also checks alignment.
Changing Config byte +0x4a back to its default 1 did not fix it. Static option
17 was incorrectly described as a cache/MCC switch; its meaning is not proven.
The independent MCC DSID remains zero in the experimental Config packet.

The successful macOS capture's `ipc-prefix-01.bin` supplies actual startup
values: device enum 29, active units 1/1, sub-ID mask 1, log mapping size
**0x24000**. Earlier Linux code used maximum capability values 2/2/mask3
and advertised **0x20200**, which is not page aligned. The current source
now matches every captured startup scalar; only the log DMA address changes.
The log's INTERNAL header usage remains 0x20200. Preloaded text SHA is identical
between macOS and Linux (`aeb4671e...7cc197`).

A fresh RAM boot testing these corrected startup values is underway through
`../logs/boot-linux-ipc-captured-startup.log`, run
`../logs/20260908T220034Z-native-linux/`. Inspect completion before opening
KIS. The last completed, asserted firmware boot was
`../logs/20260908T215313Z-native-linux/`. Never unload a started module or
repeat a failed command on its pinned buffers. Use a fresh guarded RAM boot.

New code: `apple-ave-ipc.h` (full negotiation, bounded synchronous root-only
packet transport, bounded DMA arena), `ave-make-command.py` (address-free
Config/Open packets), `test-ipc.sh`. `ave-command` accepts a SINGLE write per
packet (`dd bs=65536 count=1`); the module stamps its timeout at publication.
The arena is a 19.5 MiB region after all control allocations, with separate
DART and firmware bases in `ave-status`. No raw MMIO API was added.

Evidence: `../artifacts/ave-ipc-qualified-20260908/` freezes 35 files including
the first complete IPC result and subsequent Config failure. All six CPUs and
Vulkan passed after both failed Config experiments. Only incremental module/DT
builds have run; no kernel/Mesa rebuild. Persistent Stage 1 is unchanged.

The IPC campaign initial amber and a material progress update were delivered
HTTP 204. Read `/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md`
in full after compaction. Do not duplicate earlier hello milestone posts.

## Historical checkpoint: native Linux AVE firmware responds

This section supersedes the older DMA checkpoint below. The target is native
Linux 7.1.6-aurora-neo, boot `../logs/20260908T205404Z-native-linux/`.
**One firmware start succeeded on this boot. Do not start it again, force
unload the module, or open an m1n1 proxy connection to the Linux console.**

`linux-ave/apple-ave.c` is loaded with its memory and PM references pinned
until target reboot. Debugfs reports `state=firmware-response`,
`firmware_start_requested=1`, `start_result=0`. It received mailbox words
`(2, 0x1f280, 0x100, 0xc0000)` 4.23 ms after the Uranus start sequence.
The reply remains unacknowledged while the next IPC exchange is implemented.
Full channel initialization, hardware DMA payload tests and encoding remain
unfinished. There is no V4L2 encoder device and no encoded Linux frame.

Hardware verification also proved all 159 preloaded firmware pages and their
contents survived handoff, 1,284 IPC/queue mappings plus nine log pages, and
the cold module's load/unload/reload lifecycle. The first hello's post-check
passed all six CPUs and the Vulkan 32-value test. A repeat start was rejected,
unload was blocked, and the full saved dmesg contains exactly one start event.
See `../artifacts/ave-hello-qualified-20260908/validation.json` and manifest.

Implementation and exact static evidence: `linux-ave/STARTUP.md`. Firmware
comes from live iBoot reservations, with per-boot hashes and CRCs captured by
`ave-boot-preflight.py`. New `../artifacts/m1n1-linux-ave.patch` reuses m1n1's
ASC reserved-memory handoff; active native Stage 2 and symbols were rebuilt
incrementally in neo-builder. Persistent Stage 1 and macOS remain unchanged.
This continuation used one intentional native boot, one successful firmware
start, incremental module/DT and Stage 2 builds, and no full kernel/Mesa build.

Next: implement the remaining `AVE_HwC::StartUpIOP` exchanges against the
observed offer: time scratch fields, firmware heap, IPC memory/base and
channel descriptors. Use the saved disassembly in `../artifacts/ave2-25G76/`.
The current module intentionally has no arbitrary mailbox write command;
prepare the complete next experiment before its required RAM reboot.
Do not repeat macOS frame captures, DMA-only qualification, or cable/CDC work.

KIS is free for `../console.py` between commands. Controller remains hpm0,
RID0 on the M3 Max; another Mac is on a different controller and must not be
touched. `../j700-boot-linux.sh` performs live guarded Stage 2 placement;
never reuse a fixed base from historical logs. Read the full progress guide
at `/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md`
after compaction. The firmware-start campaign initial amber was already
posted. The green first-firmware-reply milestone was delivered with HTTP 204;
see `../logs/ave-hello-webhook-delivery.json`. Do not duplicate it.

## Historical checkpoint: AVE device and DMA mapping

The user specifically asked to avoid loops and wasted work. Reuse the
qualified firmware/ring captures below. This continuation has one native
boot attempt: `../logs/20260908T202709Z-native-linux/`, host exec session
48366 (finished exit 0), wrapper log `../logs/boot-linux-ave.log`. Linux is
running natively with all six CPUs. Both AVE DARTs initialized as v2.3,
16 streams, 16 KiB pages, unlocked, 42-bit input/output addressing. The
brief Vulkan test passed 32 exact writes. The prior
macOS HV controller 11194 was identified and intentionally terminated before
this boot. Its old SSH master was closed; macOS is not currently running.

Qualified evidence: `../artifacts/ave-linux-qualified-20260908/validation.json`
and its 22-file hash manifest, including matching module, DT, source and logs.
The native AVE test PASSED: 1,280 IPC pages plus 4 queue pages matched their
CPU mappings on both module loads, with an unload/cleanup check in between.
Post-test shell health passed and no DART fault, mapping mismatch or kernel
panic appeared in the captured log. The module is LEFT LOADED, state
`dma-ready`; debugfs `/sys/kernel/debug/285100000.video-codec/ave-status`.
It reports firmware_start_requested=0, hardware_dma_tested=0,
encoder_registered=0. No Linux encoding or firmware execution is claimed.
No controller owns KIS after these completed commands; use `console.py` for
the native Linux shell, never the m1n1 proxy client while Linux is running.

New component: `linux-ave/apple-ave.c`, an out-of-tree kernel module built
in neo-builder against the existing pinned kernel. `linux-ave/t8140-ave.dtsi`
adds only AVE and its two DART instances, using live IODeviceTree evidence.
The existing `venc0` power domains are reused. The module owns 20 MiB IPC
and 64 KiB queue buffers and checks every IOVA page against its CPU page.
It does not start firmware or register an encoder. Its mapping and lifecycle
checks are now hardware-qualified as described above.

Build and DT checks passed. `../scripts/build-ave.sh` builds only this module
and device tree; no kernel/Mesa rebuild. Bundle `../artifacts/linux-ave/`
links unchanged kernel/initramfs/symbols from the qualified baseline.
`linux-ave/check-dtb.py` confirms only three new device nodes and equivalent
panel references after dtc renumbering. The native helper now defaults to
guarded per-boot Stage 2 placement and takes an optional manifest-verified
Linux bundle; the original loader and all overlap checks remain unchanged.

Next: recover and implement firmware start/configuration and the IOP startup
handshake using the saved AVE firmware and AppleAVE2 disassembly. The kernel
now provides translated coherent buffers, so connect the recovered protocol
to this component rather than creating another byte-only harness. Remaining
unknowns include CTRR/firmware mapping, bootstrap register programming,
firmware readiness and channel descriptors, then full AVC parameters and
completions. Incremental module builds and RAM transfers can continue on this
Linux boot when DT changes are unnecessary. Recheck current state first.
Do not repeat the passed mapping test, macOS encode campaign, 1,000-submit
AGX test, or full kernel/Mesa builds without a relevant change or failure.

Read `../../neo-update/PROGRESS_WEBHOOK_GUIDE.md` in full after compaction.
The amber Linux-device/DMA campaign update and the green native Linux
buffer-mapping milestone were delivered (HTTP 204). Do not duplicate them.

## Current proven state: firmware extracted and Linux wire codec qualified

Read `/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md`
in full after compaction. The amber startup/ring update and the green
hardware ring-validation milestone were both sent successfully (HTTP 204).
Do not duplicate either post. Linux AVE encoding remains unimplemented.

Qualified evidence: `../artifacts/ave-capture-20260908/three-frames-rings/`.
`overall-validation.json`, `ring-validation.json`, `protocol.json`, and
`validation/validation.json` all passed. `manifest.json` hashes 59 files,
including the observer/codec source and the compiled Linux codec binary.
The three-frame hardware encode decoded 3/3 frames at 53.13391687569982 dB.
All 31 IPC/channel send pairs matched. The Linux C codec matched all 31
published 64-byte slots, including untouched padding and channel index
transitions. Both observed formats used 64-bit fields: mode 1 had 992 slots
(16 host commands), mode 0 had 993 (15 firmware-message echoes). Narrow
32-bit slots are statically inferred and have no hardware qualification.

Both preloaded firmware segments had two identical reads before guest start.
`ave-preloaded-firmware.json` records addresses, sizes and hashes; version
`AppleAVE2FW-9003.78.0.debug` appears in DATA. TEXT begins with an AArch64
branch to 0x204, followed by coherent exception-level setup; see
`firmware-inspection.json`. Linux has not executed this firmware.

The earlier controller 94626 was intentionally stopped to load an extended
observer. Attempt 3807 uploaded the kernel but stopped before guest start:
the firmware extractor incorrectly used BootArgs.phys_base as physical RAM's
lower bound. The read-only Stage 2 probe in
`../logs/ave-firmware-ram-bounds-probe.log` proves `/chosen/dram-base` is
`0x10000000000`, below that boot's usable allocation at `0x1000342c000`.
The two firmware segments lie in the reserved low RAM. No firmware reads
were made in the rejected attempt. The observer now uses dram-base/size,
cross-checks the hypervisor extent, and validates both segments before reads.
Regression checks accepted the reserved range and rejected out-of-DRAM and
misaligned ranges with zero reads.

The prior macOS reference boot was host exec session 74746, controller PID 11194,
`../../neo-update/logs/20260908-195036Z-kis-11194/artifacts/hv/`, top-level log
`../logs/boot-ave-rings-dram.log`. All 41 uploads have matching readbacks.
Both observers are DISARMED with zero pending sends; no capture errors or
limits occurred. Post-disarm SSH returned NEO_POST_RING_CAPTURE_ALIVE and
25G76. That controller was subsequently stopped for the Linux continuation
above. Recheck live process/port state before acting on any recorded address.

IMPORTANT: use the helper's default `--j700-safe-base` placement. The first
attempt reused the previous explicit base and the overlap guard correctly
rejected it before upload/ADT push. The working helper chose a base relative
to each boot's resident placement. Do not reuse a previous address blindly.
All overlap guards remain enabled. Reboot before another chainload whenever
the previous attempt pushed ADT, including a later pre-guest module failure.

`trace-j700-ave-rings.py` extends the qualified command observer:

* Extracts the two iBoot-preloaded AVE firmware segments before guest start,
  using `segment-ranges` records of `<QQQII>`: physical at +0, size u32 at
  +0x18. The other address fields and flags are recorded without asserting
  their semantics. Each bounded declared-RAM segment is read twice and must
  match. Outputs `ave-preloaded-{text,data}.bin` and firmware JSON manifest.
* Adds before-doorbell breakpoints to both 32- and 64-bit IOP send functions
  at exact `mov x17,#0x2abe` instructions, emulated in the saved context.
  Four breakpoints total including the existing IPC and wrapper observers.
* Captures channel metadata, each slot before send and after publication,
  and a 16 KiB prefix of each observed IPC surface. Ring memory must fall
  inside an already validated IPC surface. Limits: 256 command events,
  128 ring publications; errors/limits remove the observer's breakpoints.
* Arm/disarm using the current run's `ave-command-control.json` exactly as
  before. Then run a three-frame encode with a NEW output stem, verify the
  bitstream, disarm and wait for both logs to record disarmed, copy evidence.

New Linux userspace C codec: `ave2-wire.h`, `ave2-slot-codec.c`, compiled in
neo-builder to `../artifacts/ave2-slot-codec-linux`. It encodes the first
three fields of a 64-byte slot and preserves padding. This is not a Linux
AVE driver. `validate-ave-rings.py RUN CODEC NEW-REPORT.json` must run inside
neo-builder against the captured slots. It also checks producer/consumer
transitions and rejects malformed inputs. Its 64-bit encoding is now checked
against actual physical hardware publications, as described above. This
does not qualify DMA barriers, cache maintenance or Linux ring ownership.

New static disassembly: `../artifacts/ave2-25G76/firmware-ring-paths.asm`.
`disassemble-ave2.py` now accepts `--match` and `--output` selectors.
Key finding: the pinned Linux Apple ISP driver shares the observed AVE
readiness constant `0x8042006` and similar 64-byte three-argument IPC. Compare
`/home/mcruz.guest/neo/sources/linux/drivers/media/platform/apple/isp/` in
neo-builder. This does not establish identical startup/hardware support.
The current T8140 DTS has no AVE node; Apple AVD and ISP drivers exist.

Additional static references: `ipc-bootstrap-paths.asm` and
`chip-mailbox-paths.asm` in the same extracted-kernel directory.
`AVE_SVECtrl_GetReg` selects named Themis/Hypnos/Rhea register tables through
a chip-type-indexed table. Runtime selection has not been captured. Do not
assume that Linux ISP register offsets or startup are identical to AVE.

Next work: recover the firmware startup handshake/configuration and DART
mapping from AVE_HwC::StartUpIOP, AVE_IOP, AVE_SVECtrl and the firmware image;
implement bounded Linux firmware startup and DMA queue ownership; then map
complete AVC Start/Process inputs and output completions. Preserve the
qualified native Linux/AGX artifacts while gathering the remaining evidence.

The actual running hypervisor Python tree is `neo-update/m1n1-kis`, because
`j700-boot-kis.sh` overrides M1N1_DIR after sourcing env.sh. Inspect that tree
for runtime API behavior, not just the top-level org checkout.

## Prior qualified command capture (preserved)

Read `../../neo-update/PROGRESS_WEBHOOK_GUIDE.md` in full after compaction.
The authorized webhook is runtime-only. The encoder campaign initial amber
was sent successfully; do not duplicate it. The hardware H.264 milestone
was also sent (HTTP 204): one and 60 frames independently decoded and passed.
The qualified command-capture milestone was sent too: 31 validated IPC
send pairs during a successfully decoded three-frame hardware encode.

The qualified native Linux/AGX setup is preserved in
`../WORKING_NATIVE_BOOT.md`. The Neo was intentionally rebooted into the
known macOS 25G76 hypervisor for encoder reference captures. Do not treat the
earlier Linux shell as still running. Persistent Stage 1 remains unchanged.

## Earlier qualified command boot (controller subsequently stopped)

The revised command-capture boot SUCCEEDED in host exec
session 31063, controller PID 94626, which was later intentionally stopped. Inspect
`../logs/boot-ave-commands-mov.log` for its RUN_DIR and controller PID.
Run: `../../neo-update/logs/20260908-191206Z-kis-94626/artifacts/hv/`.
The observer was DISARMED; SSH health passed after breakpoint removal.
This uses normal guest CPUs, the qualified smaller MMIO survey, and
`trace-j700-ave-commands.py`. Arm its hardware execution breakpoints after
SSH returns by writing `{"op":"arm"}` to `ave-command-control.json` in
the run directory. `capture-xnu.py` checks that file during its first 15
paused diagnostics (5 seconds initially, then every 20 seconds). Exact
25G76 register-move instructions are verified before setting breakpoints.
The observer captures IPC send buffers and firmware channel send arguments,
bounded to 64 events and 64 KiB per command. No guest code/data is patched.
The two register moves are emulated in the saved exception context and PC
is advanced by four bytes, avoiding the unqualified single-step path.
Write `{"op":"disarm"}` before the diagnostic polling window ends to
remove the two breakpoints. Do not open KIS while the controller owns it.
The SSH helper consumes
the session-supplied credential from its environment, never a file.

Three frames passed hardware encoding and independent decode (53.1339 dB
PSNR). Captured 62 events: 31 IPC sends, each matching its channel send in
length and independently derived firmware address. Channel 1 contains 16
commands; channel 2 contains 15 firmware message buffers echoed from the
interrupt handler. There were three 25,560-byte AVC Process payloads and
one 12,672-byte Start payload. No capture errors or limits occurred.
All 41 compressed uploads in this boot had matching readback records.

Qualified evidence: `../artifacts/ave-capture-20260908/three-frames-mov/`.
Use `protocol.json` and `transport-validation-final.json`; the earlier
`transport-validation.json` was an interim check before disarm completed.
Full protocol evidence and remaining gaps: `AVE2_PROTOCOL_NOTES.md`.
The command observer is one-shot per boot. Do not try another workload with
it still armed: this three-frame capture used 62 of its 64 event budget.

SSH control path is `../../neo-update/.ssh-neo-control`, target account
`admin@admins-MacBook-Neo.local`. After a target reboot, close the old host
master with `ssh -S <control-path> -O exit <target>` before authenticating
again. A leftover master caused the initial connections to stall; closing
it and opening a fresh master restored access immediately. Current master
is working. Credentials remain runtime-only.

Next: decode the captured AVC parameter/buffer layouts, recover ring and
acknowledgment handling, firmware startup, and DMA mapping details needed
for a Linux driver. Linux AVE encoding is still not implemented. Native
Linux/AGX qualification remains preserved and reproducible separately.

The first command observer (run `20260908-185917Z-kis-87513`) failed:
256 identical Config buffers, no channel-send events, limit reached, and
the three-frame workload did not complete. Periodic disarm could not run
while it was trapped. The controller was terminated and the target rebooted.
This is not a usable command sequence. The exact failure cause is not proven;
the revised observer avoids single-step and immediately removes its own
breakpoints on a repeat guard/error/limit. Failed observer source and captures
are in `../artifacts/ave-capture-20260908/failed-breakpoint-capture/`.
That boot also had a recovered kernel-upload reply loss at image offset
`0x5c00000`; the stock recovery checked proxy response but did not print a
readback hash for that chunk. Do not claim every chunk in that failed run
was verified. The earlier successful encoding reference had full readbacks.

The third boot succeeded with six CPUs and 25G76. Both hardware encodes
passed with no trace errors/limit events. Its controller 80804 was then
deliberately stopped to add the command observer. Evidence is preserved in
`../artifacts/ave-capture-20260908/`, including hashes in `manifest.json`.
The run is `../../neo-update/logs/20260908-184105Z-kis-80804/artifacts/hv/`.

The second attempt in
`../../neo-update/logs/20260908-183054Z-kis-75821/artifacts/hv/` used `cpus=1`.
It avoided the earlier EL2 abort but stalled with the same 35,166-byte XNU
message buffer at PMP startup. Repeated snapshots found the sole guest CPU at
`0xfffffe000b3eb0c0` / `...c4`; disassembly confirms an unconditional WFE/back
branch loop. SSH never appeared. Controller PID 75821 was identified and
terminated for the next attempt. That SSH waiter has also exited.

Launch uses the saved KIS image and kernel, required MT7932 bootstrap before
AGX tracing, plus `trace-j700-ave.py`. Diagnostic boot argument `cpus=1` is
available through `J700_GUEST_SINGLE_CPU=1` but is not used by this attempt.
`J700_EXTRA_HV_MODULE` selects the additional
tracer. The launcher now selects the preverified Neo KIS endpoint explicitly
instead of requiring that only one Macintosh has a KIS device.

First trace attempt:
`../../neo-update/logs/20260908-182318Z-kis-71052/artifacts/hv/`.
It armed the survey and captured 15 boot MMIO events, including shared PMGR
windows, then hit EL2 page-table faults on secondary CPUs before user space.
Matching raw ELF maps PC offsets `0x2b944` to `mmu_pt_pending_value`
(`memory.c:235`) and `0x13560` to `hv_pt_map_l3` (`hv_vm.c:775`). The cause is
not proven. The encode workload was never started. The new attempt narrows
tracing to AVE regions 0 and 2, writes only; DART tracing is opt-in and shared
PMGR ranges are excluded. The normal guest CPU config is restored. These are diagnostic
scope changes, not a claimed hypervisor bug fix.

## Workload and evidence

- `neo-vt-encode.m`: compiled ARM64 macOS tool at `../artifacts/neo-vt-encode`.
  Uses 320x240 NV12 video-range input, H.264 Main, 1 Mbps, 30 fps, no frame
  reordering. Requires the hardware encoder and checks the VideoToolbox
  hardware-use property. Writes Annex-B SPS/PPS/NALs and a JSON result. It
  ran successfully on Neo: one frame (49.2063 dB PSNR) and 60 frames
  (65.0141 dB PSNR). Both reported hardware use and callback status zero;
  independent FFmpeg decoding verified dimensions and frame counts.
- Invocation: `neo-vt-encode INPUT.nv12 OUTPUT.h264 RESULT.json FRAMES`.
  Begin with one frame from the saved deterministic reference; output files
  must not already exist. Then decode/ffprobe the result in neo-builder,
  verify frame count/dimensions and compare NV12 quality with the source.
- Source clip: `../artifacts/encoding-reference-20260908/reference.nv12`.
- Tracer output: `ave-mmio.jsonl` and `ave-topology.json` in the HV run dir.
  Set that run dir's `ave-phase.json` to `{"phase":"encode-one-frame"}`
  before invoking the workload. The callback reads phase changes passively.
  Limit is 200,000 events; a limit/error event invalidates completeness.
  This tracer observes guest accesses only; it does not read or write AVE
  registers on its own.
- `extract-ave2.py` verified the 25G76 KC hash and extracted AVE2 segments and
  symbols to `../artifacts/ave2-25G76/`. Fileset offsets are relative to the
  whole KC; do not add the embedded header offset again.
- 2,776 demangled code symbols are in `functions.json`; selected driver/IPC
  paths are in `command-paths.asm`, generated by `disassemble-ave2.py` using
  Capstone 5.x in the task venv.

Static evidence: `AVE_HwC::SendFwCmd(void*, int)` calls `AVE_IPC::Send` with
channel 1, a command pointer and byte count. `AVE_IPC::Send` calls
`AppleAVEIOProcessorChannel::Send`, which selects `IOProcessorChannelSend`
or `IOProcessorChannelSend64`. The command interface has named config,
open/start/process/complete/stop/close/reset paths. These are source-binary
findings. Hardware output is now proven, but the Linux driver is not yet
implemented. All 1,004 captured register writes resolve to
`AVE_Reg::Write32` at PC `0xfffffe0008b26e08`; async write traces alone do
not reveal the full command buffers. The new observer addresses that gap.
