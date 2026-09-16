# J700 native mailbox and mapping capture, 2026-09-16

macOS 26.5.2 / 25F84, boot `90D7BEA7-7BBE-4073-BFA8-3709051E157D`.
Raw evidence: parent workspace `artifacts/neo-suspend/macos-live-20260916/`;
remote originals `/var/tmp/neo-pmgr-native/boot90D7-*`.
No upstream synchronization or push.

## Awake reference

With system sleep disabled, lid open, display-off log unchanged, battery
discharging and Wi-Fi reachable before/after, a fresh 120-second interval
gave **0.230670 W** from the pack-current accumulator and **0.229895 W**
from the system-load accumulator. See `baseline-summary.json` and the full
`boot90D7-idle/result.json`. These are internal battery estimates. This is a
new boot and lid condition, not a controlled comparison with the earlier
closed-lid ~0.13 W result.

The native Wi-Fi owner is a DriverKit process:
`/System/Library/DriverExtensions/com.apple.AppleSunriseWLAN.dext/com.apple.AppleSunriseWLAN`.
Its IOService class is IOUserService, not a kernel AppleSunriseWLAN object.
No process memory, Wi-Fi registers or policy were changed.

## Existing coprocessor mailbox rings

`macos-live-objects.py --kind mailbox` reads AppleASCWrapV6's inherited
AppleIOP mailbox log through DevMem ABI 7. The service may be attached but
unregistered, so discovery falls back to IOService-plane iteration; matching
only registered services incorrectly reported it missing in initial captures.
Class/vtable checks, live code hashes, retained registry anchors, bounds,
double-copy stability and timestamp ordering are required.

The matching AppleA7IOP functions establish the layout:

- AppleIOP::getStructuredLog, `0xfffffe000891e7f8`: inline header at +0x90.
- AppleIOPMailboxLog::init, `0xfffffe000891e990`: buffer pointer +0,
  capacity in u64 words +8, next-word index +0x10, payload words +0x14,
  role +0x18. AppleA7IOP allocates 128 records.
- logMsg, `0xfffffe000891ea18`: absolute ticks, continuous ticks, event,
  followed by two mailbox words. Events 0/2 are receive/send; 1/3 are
  interrupt bookkeeping (1 also includes timeout records).

All three final captures passed. Existing DCP history ends with:

```
TX EP0 0x00b0000000000201  AP retained idle
RX EP0 0x00b0000000000201  AP acknowledgement
TX EP0 0x0060000000000201  IOP retained idle
RX EP0 0x0070000000000201  IOP acknowledgement
RX EP0 0x00c0000000000000  idle-gated notification
```

DCPEXT ends with the same exchange. DCP first quiesces AFK endpoints 0x20
through 0x2c and flushes OSLOG. The ring does not contain their shared-memory
payloads or later physical clock writes. It confirms the management sequence
already used by Linux; it does not solve the PMGR CPU-write access fault.

SIO's ring retains startup, AP/IOP ON 0x20, endpoint 0x20 setup and DMA
channel commands. It has no later retained-idle exchange within the retained
window. The last records are around boot+8.55 seconds. Thus a special recent
SIO sleep command is not demonstrated by this native awake reference.

## PMGR mappings

`--kind pmgr` validates AppleT8140PMGR and reads the existing getRegMap table:
this+0x29d0, 137 IDs, stride 0x60 per ID and 0x30 per die. One die was live.
initRegMap stores its ADT index at +8, map object +0x10, kernel VA +0x18,
physical base +0x20 and size +0x28. Both copies agreed.

RegMap 0 resolves to **0x300700000**, size 0xcc000; RegMap 1 to
**0x308280000**, size 0x80000. RegMap 100 also starts at 0x300700000,
size 0x18000. There is no different physical alias in those live mappings.
This narrows the Linux DISP_CPU fault investigation; it does not authorize
another CPU power-register write.

## Correct SIO addressing; failed separate read

The live SIO IODeviceMemory resource is **0x385e00000**, length 0x88000,
with its second resource at **0x385850000**, length 0x4000. The live arm-io
ranges translate the first child region with parent base **0x210000000**.
The prior working note's SIO base 0x375e00000 assumed 0x200000000 and is
wrong. Do not reuse it or the correspondingly derived DART address.

After the successful RAM captures, an attempted separate MMIO read at the
incorrect 0x375e00040 made macOS unreachable, with no returned bytes. The
following three queued reads and PMGR snapshot produced no output. This
is not a captured SIO register value or proof of a fault at the correctly
translated SIO resource. Recovery/panic evidence is pending. Future register
tools must derive and validate the physical resources from live IORegistry,
including the complete parent translation, before reading.

SIO's native ADT `asio-ascwrap-tunables` contains two 24-byte entries:
offset 0x40 / width 4 / mask 0xff00 / value 0x400; offset 0x80c /
width 4 / mask 0x60000001 / value 0x60000001. These are a configuration
lead to compare with Linux, not yet sampled register values or a power result.
