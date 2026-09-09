# T8140 AVE firmware startup evidence

## Current result: native hardware H.264 and HEVC encoding

Full IPC, codec command submission, DMA output retrieval and independent
decoding are now proven. H.264 and HEVC both have verified 60-frame MP4 and
Matroska outputs, including a same-boot switch back to H.264. See
`../WORKING_LINUX_ENCODING.md` and `../HEVC_COMMAND_NOTES.md` for the current
recipe and evidence. The implementation owns five power domains, uses the
actual captured firmware startup configuration and a 0x24000 log mapping,
and accepts single command writes up to 128 KiB. Channel-1 ACKs have zero
payload length and the original command address. Started resources remain
pinned until reboot. The older checkpoint below is historical.

## Historical first-hello checkpoint

Native Linux now executes the iBoot-preloaded encoder firmware and receives
its first startup message. Full IPC negotiation and encoding are unfinished.
Do not label firmware mapping, a host-written scratch magic, or a successful
module insertion as encoding.

Hardware proof: `../../logs/ave-first-hello.log`, native boot
`../../logs/20260908T205404Z-native-linux/`. One firmware start returned in
4.23 ms: mailbox status 0 -> 1, words `(2, 0x1f280, 0x100, 0xc0000)`, core
control 0 -> 0x10 and status 0x6a -> 0x6c. All six Linux CPUs and the Vulkan
32-value compute check passed afterward. The initial message is deliberately
unacknowledged while the next IPC exchange is implemented.

**The loaded module is pinned with its DMA buffers and PM reference until
target reboot. Do not force-unload it or repeat a start.** The KIS console is
available to `../../console.py`; the target is native Linux, not an m1n1 proxy.

## Pinned source and target dispatch

The source binary is AppleAVE2 from the verified 25G76 kernelcache, SHA-256
`db55e48ac9e9c0f0db8e0e6e4d17efbfe8c077676f9b5eac5dd0c5f7a5f035fd`.
Disassembly and extracted symbols are in `../../artifacts/ave2-25G76/`.
`AVE_DevCap_Find` (0xfffffe0008a7ce38) indexes records of size 0x48.
Record 29 at 0xfffffe0007e55f40 starts with device ID 29, device type 27,
chip type 16 and references `gsc_sAVE_DevCap_CEntry_8140`.
T8140 uses the Uranus IOP routines, not the adjacent Themis/Aion routines.

`AVE_IOP_Config_Uranus` (0xfffffe0008b110ec) writes a 64-bit value to register
region 1 + 0x50000: `(fw_base & 0x3fffffff800) | 0x0102000000000000`.
However `AVE_IOP::Config` skips this call for the preloaded/CTRR image path
on this chip. The existing protected firmware is the path being preserved.

`AVE_IOP_Start_Uranus` (0xfffffe0008b11324) writes these 32-bit values to
register region 1 in order:

| Offset | Value |
| --- | --- |
| 0x600808 | 1 |
| 0x600044 | 0 |
| 0x600400 | 0x10000 |
| 0x600044 | 0x10 |

The live ADT's region 1 is 0x285800000, size 0x800000. The Linux DT now names
this dedicated resource `coproc`. It does not claim shared PMGR or SRAM.
`AVE_IOP_CheckIdle_Uranus` reads region 1 + 0x600048. Its return is not a
documented hardware reset sequence; a safe stop/reset path remains unresolved.

## Firmware memory path

`AVE_FwImg::InitCTRRImage` (0xfffffe0008abea18) maps the two iBoot physical
segments consecutively in the encoder's address space. `GetBaseAddr` returns
zero for this path. `RestoreCTRRData` (0xfffffe0008abfd84) copies a saved DATA
image back to its mapped location; it does not replace protected TEXT.
For T8140's CTRR path, `AVE_FwImg::Init` at 0xfffffe0008ac0c84 sets the total
extra-allocation argument to zero before calling InitCTRRImage. Its generic
3 MiB buffer default does **not** imply a missing fixed firmware heap here.

The confirmed ADT layout is TEXT size 0x14c000 at alias 0x10000000000, then
DATA size 0x130000 at alias 0x1000014c000. Physical addresses must be read from
each boot. TEXT SHA-256 is
`aeb4671ebd6fd7d55b1fa8caca7ee8ed20c5b6f9344bfc0d576e2902db7cc197`.
DATA contains `AppleAVE2FW-9003.78.0.debug`; its runtime fields are not assumed
identical across boots.

`../ave-boot-preflight.py` validates exact layout, iBoot reserved-memory
bounds, nonoverlap, two matching reads, TEXT hash and DATA version. It saves
both segments with SHA-256 provenance and passes per-boot CRC32 values under
`/chosen`. `../../artifacts/m1n1-linux-ave.patch` uses m1n1's existing ASC
reserved-memory helper to add `no-map` regions, `memory-region` links and
translated `iommu-addresses`. The Linux IOMMU core owns these mappings and
reserves their IOVAs from the DMA allocator. The AVE module checks all 159
firmware pages and the complete contents against the boot CRCs. No host or
module code writes the firmware during this qualification.

## Configuration and mailbox evidence

`AVE_HwC::MakeFwCfg` (0xfffffe0008aefb80) stores:

| Byte offset | Width | Source |
| --- | --- | --- |
| 0x00 | 32 | HwC member +0x48: SVE ID 0 |
| 0x04 | 32 | GetDevID: 29 |
| 0x08 | 32 | GetDevNum: 2 |
| 0x0c | 32 | GetDevNumPerGroup: 2 |
| 0x10 | 64 | GetDevSubIDFlag: 3 |
| 0x18 | 32 | GetDevRevision: 0 |
| 0x20 | 64 | Firmware log DART address |
| 0x28 | 32 | Firmware log size |
| 0x30 | 32 | AVE_Cfg_Get member +0x30: default DPM flags 0x77 |

`AVE_FwLog::Init` defaults to a power-of-two 0x20000-byte log plus a 0x200
header. The experiment allocates 0x24000 bytes (nine 16 KiB pages), advertises
0x20200 bytes and initializes header words `(0x20200, 0x200, 0x20000)` as
`AVE_FwLog::Reset` does. Its default filter fields remain zero. Logging output
is not yet qualified. The start configuration occupies the first 0x40 bytes
of the separate queue allocation; log, queue, IPC and firmware ranges differ.

The Themis mailbox table used by chip types 13–21 gives offsets 0x0c and
0x10 followed by a count of 64 scratch registers starting at 0x18, stride 4.
Mailbox region 2 is 0x285050000. `SetIOPFlag` writes 0x08042006 into a scratch
slot. `CheckIOPFlag` returns 1 for that host-written magic, 0 for zero and -1
for other values. Magic alone is not firmware readiness evidence.

The full startup routine also negotiates firmware heap and channel memory.
The single-attempt `ave-start-once` debugfs command verifies the cold register
state, prepares configuration and log memory, pins resources, writes the
Uranus sequence and waits at most five seconds for a mailbox response. It
does not acknowledge the message, guess the next exchange, or free resources
after a timeout. Unload is supported only before starting the firmware.

The hardware-validated `firmware-mapped` predecessor is archived separately
under `../../artifacts/ave-firmware-mapped-20260908/`; the current module also
contains the hello experiment. The unchanged kernel and initramfs need no
rebuild. Prepare the remaining protocol before the next intentional RAM boot.
# Hardware encoding qualified

The powered-domain candidate encoded two 320x240 NV12 frames under native
Linux. Independent FFmpeg decoding passed at 74.66 dB (gray) and 70.27 dB
(color bars). Two sessions completed through Stop/Close without rebooting;
six CPUs and Vulkan remained healthy. See `../WORKING_LINUX_ENCODING.md` and
`../../artifacts/ave-encoding-qualified-20260908/validation.json`.
The sections below preserve the earlier bring-up boundaries and evidence.

# Earlier hardware boundary

Full IPC, Config and session Open are proven on native Linux. The successful
startup config is copied from the actual 25G76 `ipc-prefix-01.bin` capture:
active units 1/1, mask 1, log mapping 0x24000. Static DevCap values 2/2/mask3
describe capability, not the captured active configuration. The log's own
ring header uses 0x20200 while the mailbox config advertises the full 0x24000
page-aligned allocation. Earlier Config assertions tested size AND alignment;
the assertion alone did not establish a zero-size failure.

First AVC Start was acknowledged, then firmware faulted at physical codec
register 0x285450004. The live Linux power summary showed all pipe/ME domains
OFF. The candidate now attaches `dma`, `pipe4`, `pipe5`, `me0`, `me1` by name
and holds a runtime PM reference on each, using the existing PMGR dependency
tree. Cold cleanup releases references in reverse order; started firmware
still pins all resources through success or failure. This candidate was
subsequently qualified by the two encoding sessions described above.

# Full IPC continuation (hardware qualified)

The matching 25G76 `StartUpIOP` identifies the first reply as channel count,
channel memory bytes, descriptor stride, and separate firmware heap bytes.
The hello `(2, 0x1f280, 0x100, 0xc0000)` is not a protocol version.

`apple-ave-ipc.h` implements the next sequence with five-second phase timeouts:
acknowledge the hello; synchronize the monotonic microsecond clock through
scratch 4/5; send the full IPC allocation's DART address and size; accept the
firmware virtual base; send a populated 80-byte handshake; validate returned
channel descriptors and ranges; initialize host-owned ring slots; publish the
scratch-3 readiness flag and wait for firmware to clear it.

The 64-bit handshake's heap address at +0x1c is unaligned. `CreateFwHeap` owns
a separate allocation at HwC+0xa8, distinct from the optional surface at +0x50
referenced by the Config command. Do not substitute one for the other.

Descriptors contain name[64], direction at +0x40, interrupt index at +0x44,
slot count at +0x48, and an unaligned 64-bit firmware ring address at +0x4c.
`IO` direction 0 becomes host mode 1; `IO_T2H` direction 1 becomes mode 0.
The Linux implementation validates both complete rings inside the offered
channel allocation and checks that they do not overlap before initializing.

Evidence: `ipc-negotiation-helpers.asm`, `ipc-ring-init.asm`,
`ipc-send-receive.asm`, and `command-paths.asm` under
`artifacts/ave2-25G76/`. `ave-asm-flow.py` is a static control-flow filter with
logging disabled, not a CPU emulator or a substitute for hardware validation.

The root-only `ave-command` experiment writes one packet (72..65536 bytes),
updates its common timeout with Linux monotonic time, publishes channel 1,
and polls both transport acknowledgements and channel-2 messages. Replies are
copied before echoing their slots to the firmware. A timeout prevents command
buffer reuse for the rest of the boot. Read back a 24-byte little-endian header
`{u32 channel, bytes, flags, reserved; u64 firmware_address;}` plus reply bytes.
This is bring-up transport, not an encoder API. No Linux frame is encoded yet.
