# Encoding expansion campaign — 2026-09-09

Read `../neo-update/PROGRESS_WEBHOOK_GUIDE.md` in full after any resume. The
runtime endpoint and SSH password must never be written to artifacts.

The user approved the full format-coverage plan. Implement the remaining
ProRes variants, JPEG, additional HEVC modes and hardware audio discovery;
keep H.264/HEVC/ProRes 422 regressions. See `A18_ENCODING_TARGET_AUDIT.md`.

## Active reference boot

Before the switch, native Linux was healthy: kernel 7.1.6-aurora-neo, all
six CPUs, APR started with 62 completed frames and failed=0. Target identity
was read from the Linux DT as apple,j700/apple,t8140. KIS was unowned.
The scoped hpm0/RID0 VDM reboot succeeded. Another Macintosh is on RID1.

Reference boot started with `trace-j700-coverage.py` and pinned 25G76:
`../neo-update/logs/20260909-050007Z-kis-44161/artifacts/hv/`.
Historical controller shell session 25407 has exited; it no longer owns KIS.
The script is RAM-only and verifies source/image/symbol/kernel hashes.
`logs/coverage-reference-boot-20260909.log` records its outer status.

That controller exited 1 before user space: secondary CPU startup hit an EL2
data abort at relative PC 0x64f70 (`tmalloc_large`, malloc.c:4475), FAR
0x10007d3c008, ESR 0x96000002. No codec probe ran. Full kernel readback passed;
this is not an encode failure or a proven cable problem. The next reference
attempt uses the helper's existing `J700_GUEST_SINGLE_CPU=1` setting to bypass
the failing secondary-start path. Native Linux still requires six-CPU checks.

The single-CPU attempt `20260909-050746Z-kis-49268` stalled before networking;
15 snapshots remained at the same early I/O startup state. Controller 49268
was terminated before the scoped VDM recovery reboot. The next attempt uses
the documented working six-CPU configuration and explicit Stage 2 address
`0x10003b74000`, retaining all guards. Neither failed boot ran a codec probe;
the allocator failure's cause remains unproven.

The six-CPU, guarded explicit-base attempt reached macOS user space and SSH:
`../neo-update/logs/20260909-051934Z-kis-53742/artifacts/hv/`, controller 53742,
shell session 31752. The reference matrix is now running with audio first;
output `artifacts/coverage-reference-20260909/`. Do not open another proxy.
The combined tracer opts into continued phase polling after the 15 bounded
XNU snapshots, and the matrix disarms both tracers at its end.

JPEG candidate module and combined DT build successfully. The DT semantic
check preserves all APR/AVE nodes and only adds the observed JPEG/DART nodes.
No JPEG DMA has been submitted from Linux. `make-jpeg-packet.py` refuses
preparation unless captured input matches the reference and guest status
reads establish the completion contract. JPEG DART is `dart-jpeg0-h17p`.

## Prepared implementation

- `audio/neo-audio-probe.m`: target-only dynamic format/class inventory;
  explicit hardware AudioConverter selection; ten seconds of deterministic
  PCM per advertised encoder. Decode and engine proof remain later gates.
  macOS hides the manufacturer constants, so the documented `aphw`/`appl`
  values are used directly. Do not confuse software registration with offload.
- `video/neo-vt-codec-probe.m`: compatible legacy command plus optional JSON
  config; 10-bit, alpha, depth/disparity and MV-HEVC input modes; explicit
  hardware requirements; raw samples, source pixels, MOV and property results.
- `video/coverage-pixels.h`: tight layouts, variable rows and low-bit patterns.
  All eight layout checks pass on host CoreVideo. This is not an encoder test.
- `video/trace-j700-coverage.py`: one owner combines guarded AVE command
  observation with APR/JPEG DMA snapshots. Phase acknowledgement prevents
  unlabelled experiments. First APR frame per phase, at most 24 captures;
  first JPEG frame per phase, at most eight. Existing RAM/DART bounds remain.
- `video/capture-coverage-reference.py`: prepared matrix, audio first, then
  ProRes variants/JPEG/precision/HEVC. Uses the existing SSH control connection.
- `video/make-prores-packet.py` and `video/encode-prores-linux.py`: generalized
  capture offsets, optional captured alpha, explicit source/profile metadata,
  row-wise plane shifting and independent profile/decode checks. Offline
  preparation reproduces the legacy first packet and first three moving
  inputs byte-for-byte. Tiled input is refused for synthetic motion tests.

Builds: `artifacts/coverage-probes-20260909/` (ad-hoc signed, warnings as errors).
Video binary SHA256 `bb55fb118948fb1e8c3fb27386cbdd2576623ec10453de8d6005dc17e426f1b9`.
Audio binary SHA256 `714c58b2cdac94541cf371c15403e23119767f391c32bbe1a7aff024396ed985`.

No new format has passed native Linux yet in this campaign. Preserve all
previous frozen qualification archives. Initial campaign webhook delivered
HTTP204. Do not duplicate it.

## Reference results and current transition

`artifacts/coverage-reference-20260909/guest/` contains the completed batch.
All six ProRes variants, ProRes alpha/12-bit inputs, HEVC Main10/Main42210,
depth/disparity and multiview returned three encoded frames. FFprobe confirms
ProRes profiles and 12-bit alpha output, Main10 4:2:0 and HEVC Rext 4:2:2 10-bit.
Depth/disparity and multiview still need channel-aware independent decoding.
HEVC alpha rejected PreserveAlphaChannel (-12900); its supported properties
advertise EncodesAlpha and accept AlphaChannelMode. That is concrete evidence
for a codec-specific property correction, not permission to ignore alpha.

Audio inventory reports 16 formats, 15 software class descriptions, zero
hardware advertisements. A forced AAC `aphw` request encoded ten seconds, but
the negative control ALSO accepted a nonexistent `zzzz` manufacturer. All
three manufacturer requests reject the old hardware-resume property. This
AAC encode is NOT hardware evidence; `AudioConverterNewSpecific` does not
enforce the requested manufacturer here. The audio probe now fails closed
when the nonexistent-manufacturer control succeeds. Results are saved under
`artifacts/audio-hardware-explicit-20260909/`. No hardware audio is qualified.

JPEG captured its real full-range NV12 input (macOS converts the submitted
video-range input); first frame timed out with status 2, subsequent frames
show status 1 and compressed counter 0x1020. Do not submit the initial JPEG
packet builder unchanged: it currently refuses this conversion/status case.
The old successful JPEG reference uses the same source pixels. Conversion
and cold register configuration must be matched before native submission.

The APR ring resets between macOS sessions without a zero head write. The
collector source is corrected for later boots. A gated second submission in
the SAME session recovered HQ/LT/Proxy planes successfully. The larger 4444
snapshot recovered luma/chroma/alpha but timed out that reference encode
(-536870186); no further APR submissions were made in that boot.
`artifacts/prores-planes-reference-20260909/guest/` preserves those attempts.
The 4444/XQ initial successful reference descriptors use exactly the same
source pixels and linear plane layout; their packets reuse the complete
captured 4444 planes with explicit source/layout checks. All 32 wrapper table
registers were identical across the captured profiles.

Native-ready packets: `artifacts/prores-coverage-packets-20260909/`:
`apch-first.bin`, `apcs-first.bin`, `apco-first.bin`, `ap4h.bin`, `ap4x.bin`.
Use these first-submission descriptors (the earlier HQ/LT/Proxy packet names
without `-first` contain second-submission flags). No constant-row recovery.

The macOS guest was verified as Mac17,5 / six CPUs / 25G76 before closing SSH
and terminating controller 53742. KIS was unowned. Native boot session 18830
now runs the combined `artifacts/linux-jpeg` bundle; outer log is
`logs/coverage-native-boot-20260909.log`. Stage 1 remains unchanged. The JPEG
module is a candidate and must not submit DMA until its preparation gates pass.

## Native qualification milestones

All five additional ProRes profiles passed one frame and 60 moving frames.
Minimum PSNR: HQ 35.51, LT 35.48, Proxy 33.42, 4444 39.42, XQ 39.42 dB.
The first compressed payloads match macOS exactly after its zero padding to
a 16-byte sample boundary. The HQ frame needed 14 padding bytes, revealing
that the old two-byte-only validator was too narrow. Raw image2 ProRes lacks
a codec tag and FFmpeg decodes it as 10-bit; correctly tagged MOV references
are required for 4444/XQ 12-bit comparison. Both validator issues were fixed
and the already saved first-frame results rechecked offline, without resubmitting.

Same-boot H.264, HEVC and Standard ProRes three-frame regressions passed;
six CPUs and Vulkan 32-value checks passed. APR completed 308 frames with
failed=0. AVE clients 1 and 2 closed successfully. Frozen archive:
`artifacts/prores-all-profiles-qualified-20260909/`, 1744 files, 354039456 bytes.
Its five profiles use 8-bit UYVY sources; 4444/XQ carry opaque alpha. Variable
alpha/12-bit-source qualification remains separate. Green milestone delivered
HTTP204. Original reference probe sources were reconstructed and rebuilt:
both binaries are byte-identical to the ones used on the Neo.

MV-HEVC reference was independently decoded using FFmpeg view_ids 0 and 1:
three distinct-eye frames each; minimum PSNR 43.88 and 42.48 dB. VCL layer
IDs are 0 and 1. `artifacts/mvhevc-reference-validation-20260909/` holds the
evidence. Native multiview remains unimplemented.

JPEG packet preparation now combines the old successful cold configuration,
the new captured full-range DMA input paired with identical original source
pixels, and the observed completion reads. Every non-address configuration
register matches. Do not use an approximate video/full-range conversion;
the captured converter output differs in some chroma rounding. Prepared
packet is `artifacts/jpeg-coverage-packet-20260909/frame-000.bin`.

First native JPEG submission wrote 3486 bytes, with raw wrapper IRQ status
0x80000000, but the polling diagnostic timed out on register 0x24 (zero while
its mask was disabled). Result is NOT qualified; module failed=1 and pinned.
Evidence: `artifacts/jpeg-first-linux-20260909/` and
`logs/jpeg-first-failure-state-20260909.log`. No second submission was made.
The candidate now uses the observed IRQ1040, reads 0x24 with the captured
mask, acknowledges 0x102c with zero and waits on a completion. Rebuild log:
`logs/jpeg-build-interrupt-20260909.log`. REBOOT before testing this replacement.

The interrupt candidate boot `logs/20260909T060232Z-native-linux` completed,
but inserting the module triggered an asynchronous SError before any JPEG
submission. Its new probe-time writes to 0x20/0x102c preceded the captured
initialization sequence. The latest candidate removes ALL probe-time JPEG
MMIO access, reserves IRQ1040 with IRQF_NO_AUTOEN (AIC remains masked), keeps
the captured hardware interrupt mask and polls raw wrapper status 0x102c bit31.
It then checks error cause 0x24 == 1, size, SOI and EOI before acknowledging.
Build `logs/jpeg-build-wrapper-poll-20260909.log` passed. Another guarded RAM
boot has started, outer log `logs/jpeg-wrapper-native-boot-20260909.log`.
Previous SError evidence is `logs/jpeg-interrupt-initial-20260909.log`.

HEVC Main10 and Main42210 complete command phases were separately validated
and frozen before the later MV capture limit. Archives:
`artifacts/ave-hevc-main10-capture-20260909` and
`artifacts/ave-hevc-main42210-capture-20260909`. The new bounded relocator
`video/ave-relocate-hevc-precision.py` handles 50 Start and 260 Process fields.
Scratch extents are 0x560000 and 0x680000; input is separated at 0xa00200
inside the existing 19.5 MiB arena. Main42210 tile offset is 0x398000.
Offline construction passes; no precision HEVC command has reached Linux.
`ave-extract-frame.py --hevc-profile` selects their observed output offsets.

The corrected JPEG candidate passed on native boot
`logs/20260909T061225Z-native-linux`: first frame and 60 changing frames,
Baseline JPEG, 320x240, 8-bit 4:2:0, minimum PSNR 55.03 dB, exact decoded
first-frame macOS match. All six CPUs and Vulkan passed. Module SHA256
`8fe6bfddb6e7c20cb1d7d9e928d0eaa7293c389b18072c789583fffe22962bec`.
Evidence: `artifacts/jpeg-wrapper-{first,moving}-linux-20260909`.
H.264/HEVC/ProRes same-boot regressions are next, followed by the frozen
`video/qualify-jpeg-coverage.py` archive. No persistent boot changes.

`video/encode-hevc-precision-linux.py` now prepares bounded Main10/P010 and
Main42210/P210 submissions, shifts full 16-bit input rows, checks lower source
bits, closes sessions before offline decode and supports `--validate-only`.
Both three-frame preparation cases passed. Native precision tests remain
pending; prepared dummy-arena packets must never be submitted to hardware.

JPEG same-boot H.264/HEVC/ProRes regressions passed. Frozen archive
`artifacts/jpeg-mjpeg-qualified-20260909`: 442 files, 57737531 bytes, all hashes
verified. Green milestone delivered HTTP204.

HEVC Main10 first frame passed exact decoded macOS matching, 10-bit 4:2:0,
42.65 dB PSNR, closed session, six CPUs and Vulkan. Evidence:
`artifacts/hevc-main10-first-linux-20260909`. A host MP4 timestamp issue was
corrected by assigning the known 30 Hz timestamps; saved outputs were rechecked
with `--validate-only`, without another hardware submission.

Main10 moving attempt failed waiting for the second Process reply. The first
frame and Complete replied successfully; command_failed=1, resources pinned.
No firmware CRIT or DART fault appeared in the saved diagnostic. The reference
permits reordering (SPS advertises two delayed pictures); the synchronous
diagnostic cannot queue future dependencies. Latest candidate sets the captured
force-keyframe field for every changing input. This is a concrete scheduling
test, not a proven timeout cause. Reboot is required before testing it.
Failure evidence: `artifacts/hevc-main10-moving-linux-20260909` and
`logs/hevc-main10-moving-failure-state-20260909.log`.

Fresh native boot `logs/20260909T062612Z-native-linux` passed. Force-keyframe
Main10 passed three frames and then 60, minimum PSNR 42.65 dB, exact first-frame
reference decode and real 10-bit 4:2:0. Main42210 passed one and 60 frames,
41.32 dB minimum, exact first frame and real 10-bit 4:2:2. Both sessions closed,
six CPUs and Vulkan passed. These sequences use forced keyframes; inter-frame
scheduling remains unqualified. Evidence:
`artifacts/hevc-main10-keyframes-{three,moving}-linux-20260909` and
`artifacts/hevc-main42210-{first,moving}-linux-20260909`.

Main42210 extraction initially mistook metadata counter bytes for a second SPS.
Those bytes have forbidden_zero_bit set. The extractor now checks valid NAL
headers, and all 60 saved outputs were revalidated offline without another
hardware submission. H.264/HEVC/ProRes same-boot regressions are running.

Prepared next reference batch: `video/capture-auxiliary-reference.py` runs only
missing ProRes alpha12, HEVC alpha/depth/disparity/MV and guarded audio discovery.
`video/trace-j700-ave-commands.py` adds opt-in bounded existing-DART input-plane
reads (first two Process inputs per phase). Set `J700_AVE_CAPTURE_INPUTS=1` and
`J700_AVE_COMMAND_LIMIT=512` with `trace-j700-coverage.py` for this session.
No guest mapping initialization or DMA writes are added by the observer.

Next macOS probe bundle is `artifacts/coverage-probes-auxiliary-symbols-20260909`.
HEVC alpha uses advertised EncodesAlpha with readback; the previous unsupported
PreserveAlphaChannel attempt remains recorded. A separate AVAssetReader decoder
targets auxiliary float pixels and BGRA. All probes have archived source/object
files and verified nonempty matching dSYMs. Decode probe rejects this host with
exit 3. Earlier one-step builds generated empty dSYMs after temporary object
deletion; the builder now preserves object files before linking.

Independent macOS ProRes reference decode confirms alpha equals source >> 4
for every pixel at 12-bit decode precision. Updated native ProRes validation
requires all low 12-bit source values, variable alpha and exact alpha at that
precision. Missing converted DMA planes are still needed before submission.

HEVC precision regressions all passed. Frozen archive
`artifacts/hevc-precision-qualified-20260909`: 2039 files, 709366255 bytes,
all hashes verified; green milestone delivered HTTP204. The generated target
ledger `ENCODING_TARGETS.json` records 39 targets, including the 16 dynamically
enumerated audio formats. `video/update-encoding-targets.py` derives qualification
states from archived evidence.

Native JPEG/ProRes/AVE diagnostic CLIs now accept explicit profile, input
layout, bit-depth and alpha constraints while retaining defaults. Main10 and
Main42210 are also dispatched from `encode-linux-video.py --codec hevc
--profile ...`. Unsupported profile/layout combinations were rejected before
device access. These CLI additions postdate frozen qualification source copies.

The next pinned macOS reference boot is RUNNING in host exec session 1033,
controller PID 90488. Outer log `logs/auxiliary-reference-boot-20260909.log`;
HV directory `../neo-update/logs/20260909-063952Z-kis-90488/artifacts/hv`.
It uses six CPUs, fixed Stage2 base 0x10003b74000 with guards, 25G76, combined
coverage tracer, AVE input capture enabled and a 512-command bound. hpm0-only
VDM reboot was completed after native sessions closed. Do not open another
KIS proxy; use SSH after macOS finishes booting. Then run
`video/capture-auxiliary-reference.py` with that HV directory and
`artifacts/coverage-probes-auxiliary-symbols-20260909`, writing a NEW output
directory (suggest `artifacts/auxiliary-reference-20260909`).

After any compaction, reread the full webhook contract at
`/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md`
before another action. Milestone posts for ProRes, JPEG and 10-bit HEVC were
already delivered this session; do not duplicate them.

First auxiliary reference batch stopped after the ProRes 4444 alpha12 first
callback timed out (-536870186); subsequent two frames encoded. All three
input planes were completely recovered (153600/307200/153600 bytes). The first
614400 source bytes match the earlier successful reference exactly. Evidence
`artifacts/auxiliary-reference-20260909/guest` and the 063952Z HV directory.
No retry was made in that guest; SSH and controller 90488/session1033 closed.

The audio selection guard ran on the Neo and explicitly refused encoding:
selection_enforced=0 and nonexistent-manufacturer creation returned success.
Inventory still exposes 16 formats and zero hardware classes. This completes
the public API discovery test without reporting software conversion as hardware.

`artifacts/prores-alpha12-packets-20260909/{ap4h,ap4x}.bin` combines original
successful descriptors apr-07/apr-08 with recovered identical apr-01 planes.
`SOURCE_RELATIONSHIP.md` records the separate capture and input witness.
Both 60-frame `--prepare-only` runs passed (`ap4h/ap4x-alpha12-prepared-20260909`).
Native alpha12 remains pending. The packet builder now accepts a separately
captured plane set only with matching successful references, identical source
bytes, an explicit input witness, matching layouts, verified hashes and bounds.

CURRENT reference boot: host exec session19452, controller PID93104,
`../neo-update/logs/20260909-064910Z-kis-93104/artifacts/hv`, outer
`logs/auxiliary-ave-reference-boot-20260909.log`. It uses the same pinned
6-CPU/25G76/fixed-base configuration, 512 command bound and AVE input capture.
After SSH becomes ready, run the auxiliary capture with BOTH
`--skip-prores --skip-audio`, using a NEW output such as
`artifacts/auxiliary-ave-reference-20260909`. Keep one KIS owner. Session87577
(previous capture) has exited1; no old controller should be reused.

ProRes alpha references were independently validated into
`artifacts/ap4h-alpha12-reference-validation-20260909` and the ap4x equivalent:
three frames, all4096 alpha values, changing transparency, exact source>>4
alpha and minimum color PSNR77.80/78.96dB. This is reference validation only.

The 064910Z macOS boot failed before codec probes: session19452 exited1 after
EL2 allocator/page-table data aborts and USB retirement. With the MATCHING RAW
ELF, PC+0x64f70 is tmalloc_large (malloc.c4475), +0x13560 is hv_pt_map_l3
(hv_vm.c775), +0x66234 internal_memalign. The compressed ELF gives misleading
symbols and must not be used for runtime addresses. FARs0x10007700008 and
0x100077059e0 lie near this boot's heap base0x1000750c000. Prior successful
063952Z boot used heap0x10006300000. Fixed Stage2 base alone does not prevent
the fault; no root cause is proven and no repeated blind macOS retry was made.

Work moved to qualifying the already prepared native ProRes alpha inputs.
Current native boot outer log `logs/prores-alpha12-native-boot-20260909.log`,
combined `artifacts/linux-jpeg` bundle. Once boot completes and KIS is free,
load APR plus asahi, qualify ap4h-alpha12 first then60 and ap4x first then60,
then AVE H.264/HEVC and Standard ProRes regressions. Use packets from
`artifacts/prores-alpha12-packets-20260909` and original successful references.
On fresh APR omit --warm for the first run; use --warm subsequently.

Host independent auxiliary decoding is now proven: new decoder bundle
`artifacts/coverage-probes-host-decode-20260909` supports explicit
`--allow-host-decoder` while preserving the default Neo guard. It decoded the
Neo depth/disparity references to float32 on Mac15,9 (host build26A5425a).
`video/validate-depth-reference.py` reports PSNR55.71/54.80dB and maximum
relative errors1.95%/2.49%; both reference codecs are lossy, not bit-exact float.
Evidence `artifacts/hevc-{depth,disparity}-host-reference-decode-20260909`.
This makes host validation of future native auxiliary output possible without
another target macOS boot. No host encoding is counted as Neo hardware proof.

`video/repack-hevc-reference-mov.py` preserves the reference sample description
and remuxes up to60 independently decodable length-prefixed HEVC samples. It
validates NAL headers and IDR/CRA types, updates durations/sample tables and
removes stale reordering/edit tables. One depth keyframe was repacked and
independently decoded to bytes exactly matching the original first frame.

Further ProRes precision verification: all recovered Y/Cb/Cr/alpha DMA words
equal the corresponding AYUV16 source words exactly (planar Y, interleaved UV,
planar alpha). No conversion approximation is needed for this layout.
The host decoder gained explicit y416 output in
`artifacts/coverage-probes-alpha16-decode-20260909`. It decodes the original
Neo 4444 reference with ALL65536 alpha values preserved exactly. FFmpeg's
yuva444p12 output truncates alpha to12bits; this is a decoder output limitation,
not evidence of a12-bit encoded alpha limit. Native ProRes validation now
requires both the independent FFmpeg12-bit check and bit-exact AYUV16 alpha
from the host decoder. The latter is explicitly a decode-only host operation.

Native boot `logs/20260909T065946Z-native-linux` completed; boot session82275
exited0. APR/asahi loaded successfully. ProRes4444 alpha12 FIRST FRAME PASSED
in `artifacts/ap4h-alpha12-first-linux-20260909`: exact compressed payload
except6zero alignment bytes, exact reference decode, 12-bit source color,
all16alpha bits preserved, minimum PSNR77.01dB, six CPUs and Vulkan. Current
60-frame run is `artifacts/ap4h-alpha12-moving-linux-20260909`, outer
`logs/ap4h-alpha12-moving-20260909.log`, host exec session46348.
No other KIS client may run until it exits.
Then qualify ap4x first and60 with --warm; run baseline regressions and archive.
The native validator now also verifies every visible AYUV16 plane word against
the corresponding source component before submitting DMA.

ProRes4444 alpha12 moving60 passed (PSNR77.01dB, exact16-bit alpha throughout);
session46348 exited0. XQ first frame passed (77.70dB, exact compressed/decoded
reference and full alpha), `artifacts/ap4x-alpha12-first-linux-20260909`.
Current XQ60 run is `artifacts/ap4x-alpha12-moving-linux-20260909`, outer log
`logs/ap4x-alpha12-moving-20260909.log`; use the actual tool-returned process ID
for its completion before another KIS command.

The large4444 input archive was25MB, dominating transfer time. New static
`video/prores-packetgen.c` only creates rotated packet files on Linux; it never
accesses DMA. It validates dimensions, lengths, plane offsets, strides and
relocations before writing exclusive output files. Its120 generated warm/cold
files matched the host packets byte-for-byte in the build VM. Bundle
`artifacts/prores-packetgen-20260909` includes source, binary, unstripped symbols
and validation. `encode-prores-linux.py --target-packetgen` uploads that verified
helper and one621KB template instead of the25MB expanded archive. XQ60 is the
first native use; existing defaults remain available.

All ProRes alpha12/16 tests and baseline regressions passed. Frozen archive
`artifacts/prores-alpha-precision-qualified-20260909`:744files,450691496bytes,
all hashes verified. XQ target packet generation succeeded on native Linux.
No KIS client is active after the completed regression session97807.

A concrete shared-state bug was found in the pinned hypervisor source:
the resident hv_hvc_handler calls SPTM emulation BEFORE hv_exc_entry/bhl,
while that emulation calls hv_map/memalign on the same shared allocator and
stage-2 page tables as ordinary locked exceptions. No SPTM-internal lock exists.
This is a source-level race; whether it causes the observed boot abort still
requires hardware validation. An isolated candidate worktree was created at
`../neo-update/m1n1-kis-sptm-serialized`, same pinned commit and the same four
existing KIS local modifications. The original worktree/images remain unchanged.

Candidate adds the existing bhl around the resident handler, clearing the
CPU's in-guest bit before lock acquisition and restoring it after unlock, as
the normal entry/exit path does. It preserves the fast path's register/time
behavior. Build is running with GNU aarch64-elf, CHAINLOADING=1,
T8140_KIS_PROXY=1. Outer log `logs/sptm-serialized-stage2-build-20260909.log`.
It must be separately hashed with matching RAW ELF symbols and tested only
through the guarded RAM chainloader; never install it as persistent Stage1.

The isolated SPTM serialization build passed. Frozen RAM-only candidate is
`../neo-update/artifacts/kis-sptm-serialized-20260909`, with667 tracked source
files, source.patch, build_cfg.h and all three matching image/symbol files.
Image SHA256 `21f7f34e6ed2c00c58322ddc4f71d64d97bd99575a8c4cfde526c162475c7bef`;
RAW ELF `aa08e498d959f5a38db1680ae50c7d83b11d7e1dd3fa88cf9a149e5e75ea1c8b`.
`j700-boot-kis.sh` preserves its defaults and accepts explicit J700_KIS_SOURCE
and J700_KIS_BUNDLE with extra candidate source/patch/configuration checks.
Manifest is copied into each boot log. Original artifacts remain unchanged.

Native Linux identity/CPU0-5 were rechecked before scoped hpm0 reboot. Candidate
boot is active in `../neo-update/logs/20260909-072623Z-kis-11184/artifacts/hv`,
host session43188, PID11184, outer `logs/sptm-serialized-reference-boot-20260909.log`.
The guarded chainload and all host checks passed; pinned25G76 is uploading.
This is the only KIS owner. Do not start another proxy or native console.
After SSH appears, use remaining auxiliary AVE batch with --skip-prores
--skip-audio and input capture enabled. Candidate runtime qualification is pending.

ProRes alpha12/16 milestone was posted once, HTTP204. Coverage guide now includes
AYUV16 selection and the optional verified target packet generator.

New bounded `video/inspect-hevc-reference.py` extracts MOV descriptions and
independently probes elementary HEVC. Depth and disparity references both use
10-bit monochrome HEVC Rext (gray10le), with a separate13-byte disc payload;
float32 API pixels are reconstructed by the auxiliary wrapper. MV reference
has VCL layers0/1, 8-bit420 base layer and an lhvC extension. Evidence is in
the three `artifacts/hevc-*-sps-inspection-20260909` directories. These are
reference inspections, not native auxiliary qualification.

Candidate session43188 exited1: CPU5 EL2 data abort during guest secondary
startup, PC+0x13560=hv_pt_map_l3 at hv_vm.c775, FAR0x100079febd8,
ESR0x96000002. RAW ELF disassembly shows a read of the existing L3 entry,
not an allocation at this PC. Stage1 base0x100059ec000, live heap0x100077dc000;
Stage2 heap allocations include the faulting table0x100079fc000. The candidate
did not resolve the boot failure and is NOT qualified or promoted. The target
automatically rebooted and the controller exited. No codec probes ran.

To avoid expanding into unrelated HV debugging, the next bounded reference
attempt uses the original verified KIS bundle with J700_GUEST_SINGLE_CPU=1.
This changes only macOS reference CPU count. Native Linux qualification remains
CPU0-5 plus Vulkan and all baseline codecs. Remaining AVE capture flags and
25G76 stay pinned; resident Stage1 stays intact. If this fails, inspect concrete
evidence instead of repeating the same boot configuration.

KIS was retired after the fault; the first single-CPU launcher stopped before
contact/chainload because its Neo port was absent. Scoped macvdmctl reboot
debugusb restored it (session34443 exited0). The actual single-CPU reference
boot is `../neo-update/logs/20260909-073502Z-kis-14052/artifacts/hv`, host
session27303/PID14052, outer `logs/auxiliary-single-cpu-reference-boot-kis-restored-20260909.log`.
It uses the original verified KIS image and guest cpus=1, with missing AVE input
capture and command limit512. SSH opener session10647 waits at most180seconds
for the network before opening the protected control connection. Do not start
another KIS owner. Remaining batch should use `coverage-probes-alpha16-decode-20260909`,
NEW output `auxiliary-ave-reference-20260909`, --skip-prores --skip-audio.
The batch now records guest CPU counts and verifies copied probe binary hashes.

Offline protocol audit confirms depth/disparity each use the existing50 Start
pointer fields, surface span0x4d4000 from base0x100002a8000, stream offset0xa0000
capacity0x48000, metadata0x21c000 and tile table0x26c000. Process has one plane,
153664bytes/stride640. Do not assume which64bytes supplement the pixels until
the input snapshot is available. MV has extra Start pointer fields and a
surface span exceeding the current19.5MiB arena; existing50-field relocation
must NOT be reused unchanged for MV.

Prepared separate native AVE driver candidate with immutable ipc_mib=20(default)
or32 only, rejected before allocation for other values, and arena seek bounds
derived from the actual allocation. `scripts/build-ave.sh` now accepts a bounded
NEO_AVE_BUNDLE_NAME and archives sources beside symbols. Candidate
`artifacts/linux-ave-32m-candidate-20260909` built with W=1 and passed DTB checks;
moduleSHA639d939ba1e2b0735c02484924769f7dcf96f482484b8ec461e02b7d4ec1e996,
unstrippedSHAfd81702a18c9e4b5901eb09d51cb5a7fbcdd513dc2eac6787498ed5f901a2650.
Runtime is UNTESTED. Original qualified AVE module still has SHA9432ecf8...
Use the original20MiB module for auxiliary paths that fit; test32MiB only for
MV after complete protocol evidence, with all baseline/CPU/Vulkan gates.

The cpus=1-only reference attempt stopped advancing at XNU PC0xfffffe000b3eb0c4.
Six snapshots over100seconds were identical44602-byte msgbufs; disassembly shows
an unconditional WFE/back-branch loop. SSH opener10647 ended1 after its bounded
wait. Controller14052 was explicitly terminated; session27303 exited143. This
configuration is not a successful reference boot. Its exact terminal cause is
not yet decoded; do not call it an ordinary idle loop or a proved CPU-limit bug.

The next bounded correction uses run_guest.py's existing -C4 option: remove
unused guest ADT CPU nodes before SPTM bootstrap, leaving the live boot CPU4.
It does not set cpus=1. `j700-boot-kis.sh` exposes this as J700_GUEST_CPU_SET=4.
Scoped VDM reboot completed (54367 exited0). Current sole KIS owner is
session44226/PID17613, `../neo-update/logs/20260909-074619Z-kis-17613/artifacts/hv`,
outer `logs/auxiliary-cpuset-reference-boot-20260909.log`. The log confirms
cpu0/1/2/3/5 removed. Original verified Stage2 and25G76 remain unchanged.
J700_DIAG_CONTEXT=1 adds bounded read-only register/frame-pointer snapshots via
capture-xnu.py; every followed pointer must translate to guest RAM. If the same
known terminal WFE recurs, it also preserves a candidate debugger-state/format
string, clearly labeled until verified. This should resolve a further stall
without blind repetition. No SSH opener is currently running.

`video/freeze-ave-phase.py` freezes hashes and paired transport records for a
closed phase. --require-inputs checks each expected plane for the first two
Process commands against the actual descriptors, including size/stride/IOVA.
Old depth/disparity closed protocols froze successfully (27commands each),
`artifacts/ave-hevc-{depth,disparity}-protocol-20260909`. Negative checks rejected
missing input snapshots and the old truncated MV phase without producing a
completion manifest. These protocol-only archives do not qualify native output.

The -C4 reference boot also failed, earlier in startup. Context-04.json captured
a backtrace through _panic (return0xfffffe000bd10f44) and caller0xfffffe000b9eab44.
Register19 points to static string0xfffffe00070693aa, decoded from pinned kernel:
`%s: VA->PA translation failed for va %p @%s:%d`. This is a confirmed XNU
translation panic; the exact failing VA was not captured. Guest msgbuf stopped
at2188bytes. No codec probes ran. Do not repeat these reduced-CPU configurations
or claim they fixed six-CPU HV startup. Existing six-CPU successful references
remain the valid codec evidence; collecting further layouts is now blocked on
a stable reference boot. The attempted SPTM lock is also unqualified.

SSH opener20612 exited1. Controller17613 was terminated and session44226 exited143.
Returning to native Linux: boot session14403, outer `logs/ave-32m-native-boot-20260909.log`,
run `logs/20260909T075420Z-native-linux`, original qualified linux-jpeg bundle.
Wait until session exits0 and NEO_LINUX_AND_VULKAN_READY before KIS access.
No other KIS owner is active. Remaining useful validation is the prepared32MiB
AVE allocation change: check default20MiB pre-firmware lifecycle, safely unload
only before firmware start, then32MiB lifecycle/IPC/config and baseline H264/
HEVC/ProRes plus CPU0-5/Vulkan. This does not qualify MV encoding by itself.

test-native.sh now supports an explicit second20/32MiB argument and checks
the resulting owned page count. Omitting it keeps compatibility with the old
fixed20MiB module. The prepared candidate/source copy contains this script.

FINAL STATE for this pass: native boot14403 exited0 with
NEO_LINUX_AND_VULKAN_READY. Pool test72676 exited0. Both default20MiB and
explicit32MiB cold allocation/load-unload-reload checks passed before firmware
start;32MiB firmware IPC and Config succeeded. Last-byte access worked and an
out-of-bounds seek was rejected. Three-frame H264, HEVC and ProRes regressions
passed, then CPU0-5/Vulkan and final health passed. AVE session is closed and
idle; APR/asahi remain loaded. No KIS client remains active. Do not unload the
submitted modules; use a fresh RAM boot for another configuration.

Frozen `artifacts/ave-pool-qualified-20260909`:239files,580983829bytes, all hashes
verified. Includes source, kernel/module symbols, configurations, input/output
regressions and boot evidence. It explicitly does NOT qualify MV encoding.
The original AVE artifact was preserved; full10-bit qualification recipes still
refer to that original module. The live module is the32MiB candidate639d939b...

`ENCODING_EXPANSION_RESULTS.md` is the current concise handoff. The39-target
ledger now explicitly records missing auxiliary captures as blocked by reference
boot failures; audio includes the actual guarded negative-control evidence.
No further reference boot attempts should repeat the failed configurations
without a concrete correction. Priority1/2/3 and ProRes precision/alpha from
priority4 are hardware qualified; HEVC alpha/depth/disparity and MV remain
unfinished. Hardware audio remains unconfirmed, not proven absent.

Webhook updates: ProRes alpha16 milestone HTTP204, then one amber reference-boot
blocker update HTTP204. No further duplicate blocker post is needed. Contract
for future resume: reread `/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md`
in full before any other action; webhook endpoint stays runtime-only.
The test runner used module639d939b... from /tmp/apple-ave.ko and preserved the
original qualified artifact. Linux and the memory change have now been checked;
remaining HEVC auxiliary targets are blocked, with saved panic and EL2 evidence.

## Depth/disparity conversion recovered; native first frames pass

Host VideoToolbox static analysis recovered the depth wrapper's binary32/half
lookup tables. The reconstructed gray10-to-float32 decoder matches all 460800
values from the saved Neo depth/disparity reference frames exactly. CoreVideo's
L010 allocation probe independently establishes stride640,153600 visible bytes
at offset0 and153664 IOSurface allocation bytes: the additional64bytes trail
the pixels. Frozen Neo Process descriptors independently give the same153664
size. The native input uses owned zero padding, never a guessed leading header.
Evidence: artifacts/host-depth-static-20260909 and
artifacts/depth-companding-validation-20260909. Host is Mac15,9 /26A5425a;
this static/allocation/decode work does not claim host encoding as Neo hardware.

The extended guarded relocator keeps50 typed Start pointers and259 Process
pointers for the single-plane auxiliary layouts; scalar lookalikes are not
relocated. Invalid arenas, unaligned bases and invalid client IDs are rejected.
The diagnostic wrapper accepts depth/disparity, grayf32le and32-bit input,
converts through half-float to MSB-aligned L010, and preserves disc metadata.
No persistent image or kernel changed. Existing Main10/Main42210 layouts retain
their50/260 relocation counts and original defaults.

Native first-frame depth and disparity both passed, with bit-exact reference
float32 decoding and agreement between VideoToolbox and FFmpeg+recovered LUT.
Both sessions closed; CPU0-5/Vulkan and firmware health passed. Actual AVE module
is32MiB639d939b..., now explicitly checked and recorded by the runner.
One-frame evidence: artifacts/hevc-{depth,disparity}-native-one-20260909.

The first60-frame depth row-wrap stress run completed all firmware commands
and independent decoding, but FAILED the existing5% maximum relative-error
quality gate.15frames have isolated edge failures; worst19.346% (six pixels in
that frame) occurs at the artificial near/far row-wrap discontinuity. Overall
PSNR stays above54dB. Preserve artifacts/hevc-depth-native-moving-20260909 as
failed quality evidence; do not relax the gate or count it as qualified.
A smooth depth ramp, translated by1/256 per frame (inverse for disparity), is
being checked with the same5%/40dB gates and unchanged first reference frame.
All inputs still exercise fractional precision; discontinuity behavior remains
a documented lossy-codec limit requiring future quality/configuration work.

Depth/disparity smooth sequences now PASSED60frames each. Minimum normalized
PSNR54.3026/51.5058dB; maximum relative error3.2975%/3.7909%. Both independent
decoders agree for every value, first frame equals the Neo macOS reference,
all60adjacent frames differ, timestamps/dimensions/chroma/precision pass.
Three-frame H264, HEVC and warm ProRes regressions passed. A ProRes cold-state
invocation was rejected before DMA because APR was already started; corrected
--warm invocation passed. The rejected read-only attempt is preserved.
Final health ran SHA256 work pinned separately to CPUs0,1,2,3,4,5 and verified
each inherited affinity mask and identical digest; Vulkan32values and firmware
health passed. artifacts/hevc-auxiliary-final-health-20260909 holds evidence.

Frozen artifacts/hevc-auxiliary-qualified-20260909:3101files,1560040608bytes,
every hash verified. It includes the failed row-wrap quality stress, source,
matching AVE/kernel symbols, static framework/layout evidence, all references,
and successful native inputs/outputs/commands. DEPENDENCIES.json pins existing
boot/firmware archives. ENCODING_TARGETS.json and ENCODING_EXPANSION_RESULTS.md
now mark depth/disparity Linux validated with the explicit lossy edge limitation.
Green webhook milestone delivered HTTP204. No KIS owner remains; Neo is healthy
native Linux, AVE32MiB idle and APR/asahi loaded. Never unload submitted modules.

Next concrete MV finding: the old trace is incomplete overall but contains the
first paired views (ipc245/247 Process, subtypes0x15/0x16) and their successful
replies before truncation at256. It also contains Start241 and successful reply.
Do not relabel it a closed full capture. Start has85typed allocation pointers
(50existing+35extra); three pointer-like scalar words must stay unchanged.
Required scratch span extends to0x1b4c000 relative toOLD_BASE, so inputs must
move beyond that span in the32MiB arena, never use the precision helper's10MiB
input offset. Input descriptors show NV12 with320stride,76800Y+38400UV for
each eye, matching the original owned reference pixel format.
The existing kernel ave-command API blocks for a firmware reply before accepting
the next command. The reference submits both view Process commands before the
first reply. A bounded paired-command API and additional typed relocations are
therefore a concrete next implementation path; no blind existing-API submission.
No MV DMA has been attempted. Freeze/analyze the partial prefix explicitly and
require first-frame independent two-eye decoding before any moving-frame test.

MV prefix inspection is now frozen in artifacts/mvhevc-prefix-reference-20260909.
Its protocol report remains transport_validation_passed=false and closed=false
because of the known limit, with12/12matched sends and no unmatched events. The
inspector permits only that exact incomplete-prefix shape; it does not change
the strict full-phase freezer. It records Start85typed allocations, both first
Process packets/replies, NV12 inputs, original3-frame hardware reference, and
every source hash. Tile tables contain256pointers each with1024stride, beginning
at scratch offsets0x188c000/0x1894000 (observed overlapping unused tail entries).
Stream offsets0x168000/0x1a8000, metadata0x3e8000/0x3f8000. Inputs must use a
separate region beyond scratch end0x1b4c000, e.g.0x1c00200 in the32MiB pool.
View selectors atProcess+0xac84/+0xad34 are0/1; frame index+0xacf8 is0/1 for the
first paired frame, and both timestamps+0xad40 are0. Preserve this distinction.

New candidate driver adds ave-command-pair through apple-ave-pair.h. It validates
an AVP2 header plus exactly2x45504-byte Process and2x72-byte Complete packets
before submission. It uses four separate64KiB slots in the existing command
region0x40000..0x80000, queues both views, and sends each matching Complete only
after that view's successful Process reply. It retains all four replies and
checks all four transport acknowledgments; duplicates, malformed replies or a
timeout mark command_failed, requiring reboot. Existing ave-command is unchanged.
This follows the observed Complete-after-first-Process ordering and avoids
assuming that the second view can always finish before the first Complete.

Build W=1 PASSED in neo-builder with no warnings, separate candidate only:
artifacts/linux-ave-pair-candidate-20260909. ModuleSHA
e1a0ca3649093490440b34d0e2b4b5e4dccb1697bd5480da8ac21514b91e0769,
unstrippedSHA4f93f02d91e417550ddf89bca067f20a7737e28759515302346de86a833ffa35.
It is NOT hardware validated or loaded. The actual Neo still uses639d939b...
from the completed auxiliary qualification. Next: host fault/order simulation
for paired API, typed MV relocation/payload preparation, then guarded fresh
Linux boot for candidate pre-firmware checks/baselines and first paired encode.
Do not unload the current live AVE/APR modules. No KIS owner or build process
remains active after this bounded preparation.

## Continuation: MV host tests, then user-directed desktop priority

The actual paired driver implementation passed17host test scenarios under
AddressSanitizer and UndefinedBehaviorSanitizer with halt-on-error enabled.
Tests cover reversed views, second-view dependency on first Complete, missing
view timeout, malformed/duplicate replies and acknowledgments, failed second
submission, invalid packet shapes/clients/request IDs, and missing privilege.
Initial harness-only mailbox typing and sanitizer-platform-option issues were
corrected; the final log contains only the17-pass result. LeakSanitizer is not
available on this macOS host; no leak-check claim is made.
Source: video/linux-ave/test-pair-host.c; evidence: logs/ave-pair-host-test-20260909.log.

video/ave-relocate-mvhevc.py and video/encode-mvhevc-linux.py prepare bounded
Start85-pointer and paired520-pointer packets. Both owned NV12 eyes are placed
after scratch at0x1c00200. The validator preserves reference hvcC/lhvC only after
matching native parameter sets, then independently decodes both views. The
one-frame payload prepares successfully; no native MV command has been issued.
run-command.sh accepts an optional pair mode while retaining its single default.

The user changed priority to latest Neo GPU work plus an Omarchy desktop
installation in the Neo's free internal space, then explicitly requested a
commit of encoder work before continuing. They supplied
https://github.com/omacom/omarchy-mac as the preferred installation reference.
Preserve the broader encoding goal; resume MV hardware tests after desktop work
unless directed otherwise. Do not allocate or format any disk without first
identifying the Neo's actual unallocated extents; preserve macOS and Stage1.

A RAM Linux boot had already started before the priority change. Active handle
12576 runs j700-boot-linux.sh with the original linux-jpeg bundle, log
logs/mvhevc-candidate-native-boot-20260909.log, run logs/20260909T090713Z-native-linux.
The target is positively J700/T8140, native Stage2 watchdogs disabled; the latest
log is still transferring the verified Linux Image over KIS. This is a live
process, not a failed boot. Wait/poll that handle; do not open another KIS owner
or reboot merely because the observation wait expires. The new paired AVE
candidate has NOT been loaded. No partition changes have occurred.

The user specified the destination: lowercase `j700` in
`aurora-silicon/linux-aurora`, with the encoder changes pushed there. The initial
local neo-linux/main staging repository has no commits and will not be published.
A separate sparse kernel worktree preserves the existing asahi/j813 checkouts
and the remote uppercase J700 branch. The new branch uses the qualified
drm-agx3 kernel base 4ac2664258f370cf7b03327da742b5885f91ca0b; encoder sources,
diagnostics and qualification records live under tools/aurora/neo-encoding.
Large artifacts/logs remain local; QUALIFICATION_ARCHIVES.json indexes eight
qualification manifests. The source scan found no session password, webhook
endpoint or private key. README documents the separate build workspace.

RAM boot handle12576 subsequently exited0 with NEO_LINUX_AND_VULKAN_READY.
The paired AVE candidate has not been loaded and no disk changes were made.

Resume contract: read `/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md`
in full before any other action. Runtime endpoint remains outside artifacts.
