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
translated SIO resource. Neo automatically returned to normal Linux #6,
boot `b18d501c-90b9-4c3b-87c5-17c32d1a332e`; no DebugUSB reset was sent.
The macOS panic report has not been retrieved. Future register
tools must derive and validate the physical resources from live IORegistry,
including the complete parent translation, before reading.

SIO's native ADT `asio-ascwrap-tunables` contains two 24-byte entries:
offset 0x40 / width 4 / mask 0xff00 / value 0x400; offset 0x80c /
width 4 / mask 0x60000001 / value 0x60000001. These are a configuration
lead to compare with Linux, not yet sampled register values or a power result.

## Linux SIO clock-gate isolation

Correctly addressed read-only #6 admission returned:

| Register | Value |
| --- | --- |
| SIO_CPU PMGR | 0x0f0000ff |
| ASC configuration +0x40 | 0x000a0000 |
| ASC CPU control +0x44 | 0 |
| ASC CPU status +0x48 | 0x6a |
| ASC +0x80c | 0 |

Thus the inherited SIO CPU is **stopped**, not an adopted running RTKit
instance. Native SIO's startup history cannot be assumed to describe Linux's
firmware state. No SIO firmware or DMA mapping was started for this experiment.

One-shot experimental kernel #47 boot
`eee02c00-3ca6-4a36-a191-c205f7127e30` repeated the admission. The new
`overlay/neo-sio-idle.c` rejects a DT owner, changed inherited configuration,
running CPU and unsafe PMGR state. It defaults to read-only and restores on
a bounded delayed work item or unload. It never requests power-off.

Two 15-second AUTO/minimum-CLKGATE trials, with and without the native ASC
masks, retained actual ACTIVE at every delayed PMGR read. Both restored all
original policy/configuration and passed gateway pings. The first qualification
captured only the cached immediate `during` parameter; the repeated
`qualify47b` adds live PMGR observations and is the residency evidence.

A separate manual target-4 trial, with no ASC tunable writes, **did reach
CLKGATE** (0x0f000244), stayed there at all interval samples and restored
ACTIVE (0x0f0002ff). Sticky history accounts for the differing 0x200 bit.
This is confined to the admitted stopped SIO CPU, not general permission to
gate running coprocessors or the still-faulting DCP CPU register.

The following awake, associated, LCD/backlight-off battery comparison used
120 seconds per interval, FAST PS, no Wi-Fi ownership loop:

| State | Whole-machine W |
| --- | ---: |
| Original SIO active clock | 1.148368 |
| SIO clock-gated | 1.149309 |
| Original policy restored | 1.157872 |

All nine gateway probes passed with no loss. Both gated endpoint checks
confirmed actual4, and unload restored actual15. This resolves **no useful
SIO clock saving** and does not reach the <1 W awake Wi-Fi target.
Module SHA256 `ff6a3ddec145cd1cbf8c82c330e69546fcd0311082b869f68413dab58e9a2169`.
Full traces/controller/raw battery samples: `sio-native-compare/`.

Final cleanup left SIO at its original policy with the module unloaded,
display restored, station FAST PS on, ownership IRQ experiment off, battery
74% discharging. Automatic lid/system sleep remains inhibited. Normal #6
is still GRUB's default; #47 was selected only for this boot. The installed
asahi-bless cannot select macOS: its NVRAM device is absent, there are no
nvmem devices, and EFI runtime services are disabled. No boot-volume change
was made by those failed read-only list/get attempts.

The next native capture extends the PMGR RAM reader to decode the **live
device descriptors and PS groups**, not just mapped region bases. This is
needed to verify the actual DCP CPU register selected by getDevicePSRegOffset;
matching region bases alone does not prove the device lookup is unchanged.
That extension has passed syntax/code review but is not yet Mac-qualified.

## Fresh native boot: device lookup and Wi-Fi software state

Boot `34CB72A9-4E8A-49ED-982C-2A528D19A6B2` remained healthy during these
read-only captures. Automatic system sleep remains disabled. Artifacts are
`macos-live-20260916/boot34CB/` in the parent workspace.

The device lookup extension is now Mac-qualified. Its initial check rejected
the packed DeviceData pointer's four-byte alignment before reading it; the
reader now distinguishes that from eight-byte object alignment. It validates
the native ID round trip and mapping bounds as well. All five live devices
match our archived addresses: DISP_SYS 0x300700280, SIO_CPU 0x3007002a8,
DISP_FE 0x300700318, DISPEXT0_CPU 0x300700328, DISP_CPU 0x300710000.
There is no different device/group lookup explaining Linux's DISP_CPU fault.

Both generic `_setPwrGateRetentionV2` lookup indices are -1 for all five
devices. Therefore that generic helper has no retention record to execute
for them on this boot. This does not remove the separate T8140-specific
DISP_CPU retention workaround. Fresh DCP trace/mailbox histories and five
domain status words were retained. No corresponding panic report from the
previous incorrect-address read was found in the normal report directories.

`macos-wifi-state.py` uses task_for_pid and Mach read-only copies to inspect
the running DriverKit process without suspending it. The installed arm64e
binary differs from the earlier archived binary: SHA256
`990d66919bdb39d3a2ec30a0cd441c1a5c2f171d337baf02369d03061cbb653d`, UUID
`CDE06805-403D-32ED-AE4F-1E1DE57F9A2C`. Existing ownership field offsets match
the reviewed current disassembly; calls/string locations differ. Do not
silently apply old whole-binary identity claims to this build.

Live ownership/accessor instruction comparisons passed. The global
g_prAdapter is NULL; the actual adapter comes from gprWdev -> wiphy ->
wiphy+0x842a8, matching wlanGetWiphy and AppleSunriseWLAN::getAdapter.
The reader follows that chain afresh for every sample and fails if it changes.
At adapter+0x123aa the software firmware-own flag was 1 in **294/574 samples**
over 120.052 seconds at approximately 5 Hz. The IPC ownership selection flag
at +0x197941 was 1 throughout. This is sampled software state, not a hardware
residency counter or a power measurement; observation and SSH traffic can
affect it. No MMIO or firmware commands were issued by the reader.

The native cached BSS0 power profile at adapter+0x32248 is `[0,2,0,0]`, with
the cached vote word zero. Thus native FAST PS agrees with Linux's current
setting; a different profile value is not the missing awake-power fix.
The earlier 10-second exploratory capture called +0x123bc a reference count;
the retained helper now labels it own_enable_word to avoid asserting an
unverified field name. All state remains read-only.


## Continuous capture prepared for native wake comparison

`macos-live-objects.py --kind mailbox --registry-id ID --output NEWDIR
--watch-seconds 30 --interval .02` adds a bounded observer of the existing
128-entry native log. It validates the same build, code, class and retained
object first, then checkpoints status=watching before the test controller
triggers a managed display wake. No kernel hooks, MMIO writes or display
commands are performed by this collector.

Every raw frame retains monotonic_ns, 32-byte header, two ring copies and a
32-byte trailing header. Only matching copies with unchanged headers and
ordered timestamps contribute deduplicated records; racing frames remain in
the binary evidence. The frame size is recorded in result.json. Polling can
miss ring wraps between samples and perturbs execution, so this is transition
capture, not power measurement or proof of a complete message history.
Syntax and CLI validation passed locally; native watch qualification awaits
macOS availability. The previous one-shot capture behavior remains the default.

The next comparison must capture managed display-off -> on -> off, including
DCP/DCPEXT mailbox transitions and their corresponding PMGR CPU/SYS/FE state.
Prior Linux native-panel + endpoint teardown + returning-mode full IOP idle
still failed AP wake and remained about 1.09 W. Do not repeat CPU-domain
writes that already faulted; determine the native wake/ownership sequence.
