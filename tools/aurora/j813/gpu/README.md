# M5 / J813 GPU bring-up

The native Linux diagnostic candidate identifies the physical T8142 GPU as
**G17G revision C0, one die, one cluster, ten active cores**. It does not yet
execute GPU work or expose a render node. DCP presentation is a later,
independent qualification; neither a software desktop nor encoder success
is evidence of GPU rendering.

## Hardware evidence, 2026-09-10

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

The firmware capture identifies an observed build; it does not grant runtime
admission. G17G firmware initialization, both ASC lifecycle paths, UAT
ownership, submission and completion still need M5-specific implementation.
The existing source admits the Neo's G17P runtime only.

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
three fresh Metal command queues. Six macOS runs each passed 3,072 exact
compute values and 12,288 RGBA pixels. This is a macOS reference, not evidence
that Linux submission, rendering or DCP works.

## Source construction and M5 differences

Linux now has explicit J813/25G76 serializers for both roots and main objects,
the complete hardware-data block, four completion queue descriptors, the
shared policy allocations, region C and primary/secondary status objects.
The root version is `0x0c8aa322039e04c0`; both loaded firmware
images independently compare that value. The existing Neo runtime continues
to use `0x0c8aa322039604c0` and rejects the M5 version.

`verify-m5-gpu-codecs.py` rebuilds the configuration objects and all 53
register-table slots against three independent cold-boot captures. It checks
the seven native allocation sizes in the pinned host binary, validates
pointer rebinding and rejects overlapping backing before modifying output.
It also emulates the exact pinned
M5 host reset routine with synthetic allocations and compares its output
against the Linux channel codec across 32 cases. All comparisons pass.
The driver/module build passes; these codecs are not wired into an M5 runtime.

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
- The firmware roles can start in either order. GFX1 takes root+0xb8 as its
  shared configuration and writes +0x670/+0x678/+0x680 after startup. These
  must begin at zero. A later primary capture can already contain secondary
  runtime state, despite preceding the primary's own root handoff.

The measured ten-core J813 policy is target-specific. Its opaque calibration
words have not been generalized to other M5 machines or derived from ADT.
Read-only page-table walks verified firmware VA 0xfffffc2180080000 maps to
physical SGX 0x580000000 and register slot 39 maps to 0x580e08000. The value
at hardware-header +0x28 is unmapped in the global and sampled context views;
it must not be treated as an observed MMIO mapping.

The M5 driver contains HAL300/SKSM queue classes. During the passing reference
workload, the primary ASC emitted endpoint-0x21/type-0x83 work doorbells,
including three `0x8300000000000a` compute kicks. The observed CPU FIFO at
SGX+0x4090/+0x4098 stayed quiet. This establishes a workload mailbox path;
it does not prove every M5 workload uses the same transport or that the Linux
path is interchangeable. Neo's HAL200 runtime remains a useful reference.
Firmware ownership, the remaining channel graph, M5
submission and completion remain open. The latest Neo reference also still
has a first-draw GPU fault in Xorg; neither target is yet DCP-only.
