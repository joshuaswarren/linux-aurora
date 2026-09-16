# J700 firmware display power command

Offline analysis of 25F84 AppleDCP-1041.120.7~1815-t8140dcp.RELEASE.
Original `dcp.macho` SHA256
`091e9d9320818360d422c938bde5e4fd5c4e83ccb6b44e2134fac907b975ae9f`;
analysis-only resolved-pointer copy SHA256
`97f0d0a9961c6d7a067356c18b8c9a9e22cfc2ef91a9b68572a7999fc576c8aa`.
Neither image was modified or installed during this investigation. Captures:
parent workspace `artifacts/neo-suspend/wifi-idle/dcp-iboot-*.log` and `.dis`.

The existing m1n1 `src/dcp_iboot.c` describes endpoint 0x23, EPIC service
`disp0-service`, subtype 0xc0. Its command header is four little-endian u32
words: operation, total byte length, zero, zero. Power operation 2 has one
payload byte, 0 or 1, hence total length 17. This is confirmed against the
actual J700 firmware, rather than assuming an older firmware ABI:

- 0x42e07f4 dispatches opcode 2, requires more than the 16-byte header and
  checks supplied length against the header's total length.
- 0x42e15b8 logs `setPowerRequest`, reads the byte at request+0x10 as boolean,
  locates the display instance, and calls 0x41c3a88 through its work loop.
- 0x41c3a88 checks current display power at pipeline+0x4004, then schedules
  0x41f2d94 if the requested state differs.
- 0x41f2d94 builds the internal power request and invokes the pipeline's
  virtual method at +0x768.

This establishes the request format and dispatch path. It does not prove
that the current inherited display endpoint is ready for a new host, that
all display state will survive power-on, or how much power this saves.
No command was sent. Full DCP CPU power gating is a separate transition.

The missing integration is AFK/EPIC transport ownership. Both existing m1n1
and Linux AFK implementations allocate a ring buffer on GETBUF, then command
TX/RX buffers using the owning DART mappings. The current retained-DCP helper
owns only management/OSLOG and intentionally refuses new DMA allocation.
Do not advertise arbitrary physical RAM as an IOVA, recycle an apparently
unused inherited buffer, or reset the live display DART to get past this.
First establish a reserved, mapped allocation or a properly retained DART
owner, then qualify endpoint startup and a read-only display query before
bounded off/on commands. Restore a known frame and verify power separately.

The archived region-50 capture contains initialized DATA at the expected
mapping, but extrapolating that linear mapping into heap addresses yielded
zeros for the display-service object. This is not proof that the object is
absent. Resolve the firmware VA through its actual mappings before further
object inspection.

## Owned-buffer integration, 2026-09-16

The opt-in retained helper now has an explicit DMA trial. `scratch_dma=cycle`
reserves one **empty entire L1 branch**, allocates its own coherent leaf table
and 1 MiB pool, publishes only that branch, flushes SID23, verifies every new
translation, and removes it again. All 2048 inherited root entries and the
original TTBR/TCR must match on restoration. It claims the DART MMIO resource
and rejects another Linux owner/shared root. It never frees an advertised
buffer; failed flush/restore leaves backing RAM pinned until reboot.

The bootloader root at 0x101ffed8000 is reserved NOMAP RAM (pfn_valid=1,
virt_addr_valid=0). The first candidate correctly rejected using DMA API
linear-map cache maintenance on it, before any PTE write. The revised helper
cleans the modified cache line through its WB memremap address using DC CVAC
and DSB, following arm64's dcache_clean_poc operation. New DMA allocations
still use the normal coherent DMA API and physical addresses from sg_table.

On #47 boot `10ebbaec-e3f8-4135-bbe5-341e08c0389f`, two scratch cycles passed:
slot128 / IOVA0x100000000, 64 new pages verified, original root restored,
all six gateway pings returned. Module SHA256
`473159405601a07ffc4cdc604a612ad7fd203f13abcba15411357d4ece439ee2`.
This proves map/restore mechanics, **not firmware DMA or display sleep**.

The subsequent endpoint start returned -EINVAL before sending a packet:
RTKit adoption originally marks only the system endpoints known. Kernel #48
adds `apple_rtkit_start_adopted_ep`, requiring an adopted, running session and
explicit client responsibility for firmware/endpoint ABI validation. The DCP
client checks the pinned 25F84 DATA hash/version before using endpoint0x23.

The AFK helper captures a bounded service announcement, structurally validates
EPICProviderClass=disp0-service, and exposes a read-only HPD query. Display
power cycling remains gated on a valid query response and an explicit 1..300s
request, with an unconditional power-on attempt after the hold. Protocol
errors/timeouts poison the command session, and buffers stay pinned. System
sleep is refused while the AFK experiment is active.

The extracted announcement parser passed a valid-class/truncation/wrong-class
check and 9,031,553 libFuzzer runs with ASan+UBSan in 11 seconds. These are
parser checks, not a substitute for the pending native endpoint and power
qualification. Captures/build/controller files are in the parent workspace's
`artifacts/neo-suspend/dcp-owned-dma/`, including kernel48's exact manifest and
isolated initramfs installation record. No DCP power register write is added.

## Native ring layout qualification

On #48 boot 750129b6-517e-4195-bdf7-9dbfcabd0f51, firmware accepted the
owned IOVA and wrote both ring headers. The old 192-byte layout guard stopped
at INIT_TX, before START or display commands. Read-only coherent-pool capture
showed size 0x1e80 and format word 0x00070006 for each 0x2000 extent.

Firmware 0x404198c selects config bytes {128,128,0} at 0x4373775 for INIT
mode 0. The constructor 0x403f234 reserves three alignment blocks, puts the
read/write pointers at +128/+256 and payload at +384. Packet alignment uses
the second config byte (0x404137c), also 128. Mailbox sizes stay in 64-byte
units. The helper now requires this pinned format and layout, rather than
accepting arbitrary size differences. Pool/DART captures and constructor
disassembly are retained in artifacts/neo-suspend/dcp-owned-dma/.

The next boot (a92fd6c5-6860-4b5d-a590-915da6dbddf2) completed both rings
and START, then captured the new compact announcement: QE payload begins
`0d 00 03 00 54 00 00 00`, with `disp0-service` and interface-id 3.
This is not the older EPIC v2 header. Firmware selection 0x404198c explicitly
recognizes INIT low16=8, sets endpoint flag bit2 and selects {64,64,0}.
The message paths test that flag and translate compact/legacy headers
(e.g. 0x4040154 and 0x4040a84). The helper now explicitly requests mode 8
and checks its 192-byte/64-byte layout and legacy header before commands.
The successful 128-byte capture is retained; it is not silently interpreted
as the older protocol.

Compatibility mode on boot a79ac4d4-e6a0-4c59-ae97-5e4f89ab1d8f correctly
announced channel 3 and the legacy EPIC v2 format. Native iBoot identifies
itself by exact name plus a 36-byte interface-id=3 dictionary, now structurally
validated. The HPD reply reached RX with type NOTIFY, category REPLY and
subtype 0xc0. m1n1's afk_epic_command explicitly accepts NOTIFY or REPLY
outer types; the helper now does too, retaining all outstanding-command,
channel, subtype and owned-RX-buffer checks. No power command was sent on
this boot. A root-only, read-only owned-pool attribute replaces ad-hoc
private-layout probes for subsequent captures.

## Zero-payload power replies

On boot 7e38d0fb-31bb-42a4-bdaf-35f4912d5484, HPD passed with the expected
20-byte response, HPD=1 and one timing/color mode. SET_POWER then caused
`ASSERT!AFKEPCommandContext.cpp:204`; Linux/Wi-Fi stayed alive. No power
measurement or successful off/on cycle is claimed for that boot. The crash
and owned buffers are retained under dcp-owned-dma/power-compare48.

Desliding the client stack by 0x321000 locates the assert at 0x404c224:
the reply context attempts to copy a zero-length response into the supplied
RX buffer and asserts when the copy routine returns zero. The power handler
0x42e16d8 explicitly sets response length to zero, including on success.
Unlike HPD, SET_POWER must advertise RX address/length zero. The helper now
requests only its EPIC status reply for power commands. This fixes the reply
contract rather than modifying firmware assertions or guessing a longer
request. The captured request's operation=2 and length=17 matched dispatch.

## Native display shutdown and AP notification

On #48 boot 2aa3e354-45a7-48a5-9b16-6a153345f5ce, the status-only reply
contract passed repeated two-second and 180-second power cycles. Native
SET_POWER(false) lowers gP16/GPIO22 itself. The old panel-GPIO helper rejects
this already-off state, so it must not be layered over native shutdown.
Measure that interval directly with sleep-power.py and check the GPIO before
and after. SET_POWER(true)'s acknowledgement precedes panel detection:
poll HPD until connected before testing or taking a restored reference.

The separate `epic_ap_cycle` attribute adds AP IDLE after native display
shutdown, retaining IOP ON, transport, clocks and DMA. It uses the same
bounded 1..300-second hold and restores AP ON before SET_POWER(true).
Status includes separate AP idle/wake results; failed transitions latch the
trial failed. This combines two previously qualified operations, but the
combined path still requires its own short-cycle and power qualification.
It does not claim DCP processor sleep or enable system suspend with AFK active.
