# Neo AVE2 protocol evidence

This is an incomplete reverse engineering record for the verified macOS
25G76 AppleAVE2 driver. The eventual Linux encoder must manage firmware,
DMA, command submission and completion; a valid macOS H.264 capture does
not by itself implement that driver.

## Proven hardware reference

`../artifacts/ave-capture-20260908/manifest.json` records the reference
artifacts and hashes. On physical Neo under the KIS hypervisor:

| Workload | Encoded / independently decoded | NV12 PSNR |
| --- | --- | --- |
| One frame, 320×240 H.264 | 1 / 1 | 49.2063307582 dB |
| 60 frames, 320×240 H.264 | 60 / 60 | 65.0140926198 dB |
| Three frames with command observer | 3 / 3 | 53.1339168757 dB |

Both required and reported hardware encoding, with callback status zero.
FFmpeg ran separately in the builder VM to decode and compare the output
with the deterministic source. These times are not native performance
measurements. No AVE Linux encode has run yet.

The qualified passive survey captured 1,004 32-bit writes: 129 at boot,
319 in the one-frame phase and 556 in the 60-frame phase. It reported no
callback errors or event limits. All write PCs were
`AVE_Reg::Write32 + 0x20` (`0xfffffe0008b26e08`). Only AVE regions 0 and 2
were traced; this is not a complete hardware bus trace.

## Static transport evidence

Evidence: `../artifacts/ave2-25G76/command-paths.asm`, extracted from the
SHA-256-verified 25G76 kernelcache. Addresses below are unslid and must be
validated against the live guest before use.

* `AVE_HwC::SendFwCmd(void*, int)` at `0xfffffe0008ad7b34` submits to
  `AVE_IPC::Send` with channel 1, a kernel pointer, and a byte count.
* `AVE_IPC::Send` at `0xfffffe0008b17d98` translates that pointer through
  `Kernel2FwAddr`, then calls `AppleAVEIOProcessorChannel::Send` at
  `0xfffffe0008a227ec` with the firmware address, byte count, and zero flags.
* The channel wrapper selects `IOProcessorChannelSend64` or
  `IOProcessorChannelSend` using bit 0 at wrapper offset `0xc`.
* The IPC object holds its command surface at `+0x28` and its firmware
  mapping base at `+0x38`. `Kernel2FwAddr` bounds-checks against the surface
  and applies the offset within it to the firmware base.
* `AVE_Surface::GetKernelAddr` at `0xfffffe0008b409c4` requires bit 3 at
  surface offset `0xf1`, then returns the address at `+0x98` plus its offset
  argument. `GetSize` at `0xfffffe0008b40b4c` requires a nonzero pointer at
  `+0x68`, then returns the 32-bit size at `+0xe0`.

## Partial command header

The named builder functions supply this initial map. It remains subject
to live buffer confirmation and does not specify every field.

| ID at byte 0 | Builder |
| --- | --- |
| 1 | Config (32-bit store) |
| 2 | Halt |
| 3 | Open (16-bit store) |
| 4 | Close (16-bit store) |
| 6 | Start, shared by AVC and HEVC (16-bit store) |
| 7 | Stop (16-bit store) |
| 8 | Process, shared by AVC and HEVC (16-bit store) |
| 9 | Complete (16-bit store) |
| 10 | Priority (16-bit store) |

Open/Start/Process copy the builder's 64-bit argument to `+0x08`, channel
manager `+0x38` to `+0x10`, 32-bit channel-manager `+0x34` to `+0x18`,
and device field `+0x270` to `+0x1c`. Their timeout object contributes 16
bytes at `+0x30`. These are data-flow observations; semantic names for
those fields have not yet been established.

AVC Start stores two DART addresses and sizes at `+0x48/+0x50` and
`+0x58/+0x60`. Its structured configuration starts at `+0x68`. AVC Process
clears a `0x63d8`-byte object before filling it. Extracting its surface,
reference-picture, frame-control and output fields is still required.

## Capture boundary

`trace-j700-ave-commands.py` adds two hardware execution breakpoints to the
qualified MMIO survey. It waits for an explicit host control-file arm
request, validates exact function entry bytes, captures bounded IPC send
buffers and channel arguments, and leaves guest instructions/data intact.
It must be run after the required MT7932 and AGX boot modules. The command
observer and its address-translation checks are now qualified as described
below. The first revision captured 256 copies of Config and never reached the channel
send, then hit its limit. It is explicitly excluded as a working command
sequence. The next revision observes ordinary register moves, emulates
those exact operations, advances PC, and avoids hardware single-step.

## Qualified command capture

The revised observer passed on physical Neo in
`../../neo-update/logs/20260908-191206Z-kis-94626/artifacts/hv/`.
The three-frame workload completed and independently decoded correctly.
All 41 logged compressed uploads had matching readback records in this run.
The observer recorded 62 events, then removed both breakpoints. SSH health
passed afterwards. There were no capture errors, repeated-entry guard hits,
or event limits.

All 31 IPC sends matched the immediately following channel send in both
byte count and firmware address. The expected address was independently
computed from the live IPC surface's kernel base, firmware base, and pointer
offset. All payload file hashes and lengths passed validation.
See `../artifacts/ave-capture-20260908/three-frames-mov/protocol.json`.

| Channel | Observed buffers | Interpretation |
| --- | --- | --- |
| 1 | 16 | Host commands: Config, Open, Start, three Process, six Complete, Priority, Stop, Close, Halt |
| 2 | 15 | Firmware message buffers echoed by the interrupt handler before host processing |

Channel 2's interpretation is supported by the captured caller PC
`0xfffffe0008aec6a8` and `AVE_HwC::ProcessIntr_IPCCh`: it receives a buffer,
echoes the same pointer/length on channel 2, then calls `ProcessIntr_Cmd`.
Incoming channel 1 acknowledgment contents were not captured. Do not treat
every channel-2 message as the corresponding channel-1 transport ack.

Confirmed payload lengths for this H.264 workload:

* Config: 120 bytes (`0x78`).
* Open, Complete, Priority, Stop, Close, Halt: 72 bytes (`0x48`).
* AVC Start: 12,672 bytes (`0x3180`).
* Each AVC Process: 25,560 bytes (`0x63d8`), exactly three captured.
* Echoed firmware messages: 72 bytes, except Process messages at 80 bytes.

All channel-send flags were zero. The observed IPC surface was 20 MiB with
firmware base `0xffffffff80024000`. Addresses and allocator placement are
run-specific; a Linux implementation must allocate and map its own buffers.

## Qualified ring publication and firmware extraction

Evidence: `../artifacts/ave-capture-20260908/three-frames-rings/`, originating
from `../../neo-update/logs/20260908-195036Z-kis-11194/artifacts/hv/`.
All 31 channel sends reached the added publication observer. The three-frame
hardware workload decoded successfully at 53.13391687569982 dB PSNR. The
observer disarmed with no pending sends; post-disarm SSH health passed.

The publication hooks observe exact register moves at narrow
`0xfffffe0008b84cc4` and wide `0xfffffe0008b87454`, after stores, barrier,
optional cache callback and index update, before notification. All observed
publications used the wide path; narrow layout remains static inference.

| Slot byte offset | Observed 64-bit little-endian field |
| --- | --- |
| 0 | Firmware buffer address OR (mode XOR 1) |
| 8 | Byte length, zero-extended from 32 bits |
| 16 | Flags, zero-extended from 32 bits |
| 24 through 63 | Preserved padding |

Stride is 64 bytes. Receive code masks the low two address bits. The C codec
requires four-byte alignment and models only byte encoding/decoding, without
publication barriers, cache maintenance or ownership management. Its full
64-byte output matched every captured publication. Invalid alignment, width,
mode and narrow-address overflow were rejected by the Linux harness.

The observed channel object holds mode at +0x10, slot count +0x14, ring base
+0x18, consumer index +0x20 and producer index +0x24. Empty/full indices may
be -1. After a send, an empty consumer becomes the old producer; producer
advances modulo slot count, or becomes -1 on collision with consumer. Every
captured transition matched this rule. Observed mode 1 had 992 slots and
mode 0 had 993. Both used `AVE_IPC` notification callback
`0xfffffe0008b16b58`. Their ring offsets within the IPC surface were 0x28200
and 0x37a00 respectively; these are capture observations, not Linux constants.

Firmware extraction uses `/chosen/dram-base` and `dram-size`, cross-checked
against the hypervisor RAM extent. BootArgs.phys_base describes usable boot
allocation and excludes the low reserved firmware; it is unsuitable as the
physical RAM lower bound. Both segments are validated before either is read.
Two reads of each segment matched, with sizes TEXT 0x14c000 and DATA 0x130000.
The manifest records raw addresses and other segment metadata without
assuming meanings for the alias and flag fields. DATA identifies
`AppleAVE2FW-9003.78.0.debug`. TEXT's AArch64 entry branches to 0x204 and sets
up exception levels and VBAR_EL1. Firmware has not run under Linux.

Next evidence needed: firmware startup/handshake, firmware acknowledgments,
doorbell register selection, DART mappings, and complete AVC parameter
layouts before attempting Linux submission.
