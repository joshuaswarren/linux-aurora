# N1 Bluetooth bring-up

Started September 9, 2026 after the completed Wi-Fi checkpoint was pushed.
All repositories now use the user's requested single `j813` branch.
The primary Wi-Fi driver is in linux-aurora at
`drivers/net/wireless/apple/centauri/`; its workbench is in `tools/aurora/j813/`.
Both m1n1 carrier sources are on m1n1-aurora/j813: native Linux at the root,
macOS tracing under `experiments/j813/macos-trace/`. The temporary checkpoint
branches were deleted after verifying their commits and source trees were
preserved. U-Boot and the AuroraSilicon source-location pointer also use j813.

## Current result (September 10 UTC)

BLE discovery works on J813. Native WriteBdAddr, InitDone and SetRandomSeed
before the first HCI reset remove the previous scan-abort boundary. Passive,
active, repeated and standard Linux management discovery all pass; the latter
found 26 unique nearby devices. Controller transport errors remain zero and
Wi-Fi passed 5/5 Google pings after each test. The address is temporary.

An opt-in ACL data path now builds, but connection and pairing qualification
remain pending. SCO/ISO and audio are not implemented. New Bluetooth changes
remain local on Linux `j813`; the completed Wi-Fi/repository work is already
pushed. The sections below preserve the earlier investigation chronology;
the old scan-abort statements are superseded by the native startup results.

## Starting baseline (before Bluetooth changes)

- Linux enumerates Beta as PCI function 2, Apple 106b:1903, class 0x0d1100.
  The existing `apple_centauri` module owns the function but exposes only
  generic status and ROM debug files. There is no HCI implementation.
- The running kernel has no Bluetooth stack enabled and no Bluetooth class.
- The current control bootstrap already supplies Beta's `mswb` memory and
  enables PCI mastering, but only Control and Alpha initialize host IPC
  contexts. Bluetooth therefore needs a Beta context and ring transport.
- Baseline Wi-Fi still passes Google DNS ping: 3/3 replies, zero loss.
  Private console receipt: `logs/bluetooth-20260909/baseline-linux.log`.
- Exact target-build CentauriBeta was extracted from the retained 25G76
  DriverKit cache and imported into `j813-n1-stock-25g76` for analysis.
  Earlier host-version protocol notes are useful leads, not exact-build proof.

## Completed transport sequence

1. Confirm Beta's exact command/event framing, IPC context and ring setup from
   the stock driver, keeping firmware and analysis binaries private.
2. Add a bounded transport probe for an HCI command/response and qualify it
   on hardware, with Wi-Fi retained as a regression gate.
3. Enable the Linux Bluetooth core, connect the proven transport to HCI, and
   validate controller information and a bounded discovery operation.

The isolated `apple_centauri_bt_probe` module built without W=1 warnings.
Beta IPC initialized successfully, both command/event rings opened, and
HCI Read Local Version Information returned a valid Command Complete:
`0e 0c 01 01 10 00 0e ff 00 0e 4c 00 00 10`.
Status was `version_valid=1`, TX posted/consumed 1/1, RX consumed 1.
The response reports HCI/LMP version 0x0e, revision 0x00ff, manufacturer
0x004c, and LMP subversion 0x1000. This is command/event transport proof;
pairing, ACL/SCO/ISO traffic and audio remain untested.

Wi-Fi passed 3/3 Google pings both after IPC initialization and after the
HCI exchange. Private evidence: `logs/bluetooth-20260909/probe-start-01.log`
and `probe-version-01.log`.

The next adapter is `linux/drivers/bluetooth/btcentauri.c`, with queued HCI
commands and bounded ring polling. Its initial W=1 build passed. Bluetooth
core and crypto dependency modules were built in a separate output directory,
preserving the Wi-Fi kernel build. BlueZ tools and modules were transferred
over the proven Wi-Fi connection with matching SHA-256.

Bluetooth core and dependency modules loaded successfully on the existing
kernel. Unloading the one-shot probe then exposed an unqualified teardown:
Beta entered `exec=3` (abort), with IPC still 2. The probe detected that stop
was not confirmed and retained its DMA arena. Wi-Fi still passed 3/3 pings.
Rebinding in this state was refused. Recovery uses a fresh, scoped M5 reboot
and the already verified RAM-only Linux image; load the HCI adapter directly
on the fresh Beta function. Clean Beta IPC shutdown remains unresolved.

The HCI adapter successfully registered `hci0` on a fresh boot. Its DMA ring
setup and message completion checks passed; Wi-Fi still passed 3/3 pings.
BlueZ cannot operate correctly on the original CONFIG_BT=n kernel:
`include/net/sock.h:sk_may_scm_recv()` gates AF_BLUETOOTH socket credentials
on IS_ENABLED(CONFIG_BT), so loading the core alone is insufficient.
`build-bluetooth-image.sh` builds a separate CONFIG_BT=m RAM-test Image;
`boot-linux.py --bluetooth` uses a separate private Bluetooth initramfs and
the proven Wi-Fi device tree. The known-good Wi-Fi artifacts are preserved.

The target remains in native Linux with working Wi-Fi. Resident Stage 1 and
installed boot files remain unchanged; candidate boot tests use RAM chainloads.

## Earlier controller initialization and scan-abort investigation

The CONFIG_BT=m kernel completed Linux HCI controller initialization. The
hardware adapter reported 40 command submissions and no transport errors;
the initial event counter was double-counted by the driver and HCI core.
Source now leaves event counting to the core.

The public Bluetooth address reads as all zero. Linux therefore rejects
power-on until an address is configured. The bounded management probe
`n1-bt-mgmt-probe.c` enables LE, disables BR/EDR for this experiment, and
assigns a random, volatile static BLE address without printing it. Standard
MGMT Set Powered succeeded. This does not provision the factory address.

MGMT Start Discovery also returned success, but Beta then entered exec=3
(abort), and Stop Discovery returned status 0x05.
No device-discovery proof was obtained at this earlier stage.
Wi-Fi still returned 3/3 Google pings after this failure. Evidence is
`logs/bluetooth-20260909/mgmt-discovery-03.log`.

The first HCI adapter opened only rings 1 and 7. The native Beta ring
inventory also contains ACL, SCO, LE, LEA, LLA, debug, LMP and TSI resources.
Whether any omitted resource caused the scan abort is unproven; inspect
native setup and firmware diagnostics before assuming that explanation.
Current source also stops polling if the firmware leaves OS/IPC state 2.

BlueZ 5.85 additionally needs EPOLL, SIGNALFD and TIMERFD. These are now
enabled in the separate Bluetooth build script. The complete matching Image,
modules and BlueZ bundle have now booted successfully. Both btmgmt and btmon
work on hardware; `hci-linux-boot-03.log` records the native shell verification.

One fresh association on the Bluetooth kernel failed DHCP twice. Reconnecting
the same station to the same AP restored DHCP and 3/3 pings without code
changes. Treat this as an unresolved initial-association issue, not evidence
that CONFIG_BT itself broke the Wi-Fi fix.

The M5 KIS console disappeared after the discovery test. The attempted
quiesce command did not execute because the port was absent. The Neo console
was still present and was not used. A scoped M5 DebugUSB re-arm was attempted;
recheck the current physical target/port state before any further operation.

The user reconnected the target and reported a timeout on its display.
A scoped M5 reboot restored its dedicated KIS console. The tested candidate
adds and arms native LE data rings 4/10 before HCI registration, without
claiming ACL delivery support. Its W=1 build passed. Hardware again completed
40 HCI commands/events at initialization, then 86 commands/events through
management power-on and scan enable. Discovery still aborted with LE rings
armed; no LE data packets arrived. Polling stopped immediately on firmware
abort. Wi-Fi passed 3/3 Google pings afterward.

The private `native-le-rings.btsnoop` capture shows successful completions for
LE Set Extended Scan Parameters (0x2041) and LE Set Extended Scan Enable
(0x2042), followed by the abort; no HCI hardware-error event was captured.
Linux requested active scans on 1M and Coded PHYs. This establishes the command
boundary but does not prove which omitted firmware resource or setup caused
the failure. Evidence: `le-rings-bind-01.log`, `le-rings-discovery-01.log`.

The management diagnostic now accepts `legacy-passive`, `legacy-active`,
`extended-passive`, or `extended-active` to test only 1M scanning for eight
seconds through a raw HCI socket after management power-on. The no-argument
mode remains normal Linux discovery. Each raw mode attempts to disable the
scan on exit; a firmware abort still requires a fresh boot. The static build
passes with `-Wall -Wextra -Werror`. Hardware qualification is in progress.

The first raw-scan tests found legacy scan parameters rejected with HCI status
0x0c, leaving Beta healthy. Extended passive 1M parameters and enable both
completed successfully, but Beta then aborted again. Thus active scanning and
Coded PHY are not required to reproduce the abort. No advertising reports
were observed. Evidence: `legacy-passive-01.log`, `extended-passive-01.log`.

The rejected stop command also exposed a host-side ownership bug: the HCI
send callback freed an skb on error, while `net/bluetooth/hci_core.c` already
frees it after a failed callback. The subsequent ping crashed in `icmp_rcv`
with a missing route pointer. The callback now retains ownership on error
and consumes an skb only after successful queueing. W=1 builds pass. The fresh
RAM image repeated the passive-scan abort and rejected stop command, then
passed 10/10 Google pings without a kernel crash. Evidence:
`ownership-passive-01.log`. This fixes the observed ownership bug; it does not
fix the separate Bluetooth firmware abort.

The shared Control driver now exposes a root-readable `firmware_crash`
debugfs file containing only the bounded `cras` entry from its secondary FTAB.
It does not trigger a firmware operation. Capture after an abort and keep the
contents private. The rebuilt Bluetooth bundle contains this reader and the
HCI ownership fix; `image-bundle-02.log` and `image-SHA256SUMS` record the build.
The 40,960-byte region remained all zero before and after this Beta abort,
so it provided no firmware failure reason in this test.

The host 26A5425a Bluetooth daemon contains N1-specific `_ACI_WriteBdAddr`
and other vendor initialization routines. It is being analyzed separately as
a lead in `j813-n1-host-26a5425a`; it is not exact 25G76 target-build evidence.
Its assembly sends opcode `0xfc06` with six address bytes. The diagnostic's
opt-in `extended-passive --native-address` mode writes a temporary address,
verifies it with standard Read BD_ADDR, and starts scanning only on an exact
readback match. Hardware established that the vendor payload is in network/
display byte order, opposite standard HCI. With this correction, target
readback matched the temporary address exactly. Passive 1M scanning still
aborted Beta afterward, proving that a missing public address is not the sole
cause. The fixed host remained stable and passed 5/5 Google pings. Evidence:
`native-address-passive-01.log`, `native-address-passive-02.log`, and private
`native-address-final.btsnoop`. Factory identity remains unresolved. Details
and source digest are in
`native-address-evidence.json` in the private Bluetooth log directory.

Next investigation: obtain the rest of the native N1 controller/radio startup
sequence and firmware abort diagnostics. Additional native ring resources
remain an untested possibility. The newer daemon also encodes Write Local
Host State as 0xfda0 with two payload bytes (boolean state, then zero), and
Olympic Write LE Settings as 0xfdad with one byte; their required values and
target startup ordering are not yet qualified. Neither command has been sent.
Do not replace this evidence with guessed vendor initialization writes.

The experimental HCI module was detached after the final capture. Published
Beta DMA remains retained until reboot. The target is left in native Linux;
a fresh firmware boot is required before binding Beta again. Resident Stage 1
and installed boot files remain untouched. All new Bluetooth work is local
and unpushed on the Linux repository's `j813` branch.

The latest Bluetooth bundle now includes EPOLL/SIGNALFD/TIMERFD, BlueZ tools,
the bounded management probe, and both Wi-Fi/Bluetooth modules compiled
against that exact kernel configuration. It uses a separate private initramfs,
preserving the original Wi-Fi Image and initramfs. Once booted, copy
`/opt/aurora-bluetooth` to `/run/bt` and use `/run/bt/load-core.sh`; the tools
no longer require a working network just to load the Bluetooth experiment.

## Continued resource and firmware-log investigation

The next opt-in candidate (`arm_auxiliary_rings=1`) opens all remaining rings
in the host native Beta inventory: ACL 2/8, SCO 3/9, LEA 5/11, LLA 6, debug 12,
LMP 13 and TSI 14/15, alongside the existing HCI 1/7 and LE 4/10 resources.
Only HCI command/event delivery is exposed to Linux. Other received frames
are bounded, counted and captured privately; this is not ACL/SCO/ISO support.

This requires 16 setup messages, so the message transport now wraps its
16-slot rings while using a separate completion tag. Larger footer lengths
use the native word-count/shift encoding. The auxiliary arenas end at 0x5d000
inside the existing 1 MiB allocation and do not overlap HCI, LE or scratch.

The root-readable `auxiliary_capture` debugfs file now stores up to 1 MiB of
records: little-endian u16 ring, u16 payload length, u32 sequence, then payload.
Completed diagnostic RX entries are copied after an abort without posting
new buffers to failed firmware. Inspect captured content privately.

The full inventory was accepted on J813: all 16 setup messages completed and
the message ring wrapped to zero. The debug ring delivered 78 records / 50,144
captured bytes before scanning. Passive 1M scan still caused Beta exec=3, while
Wi-Fi passed 5/5 Google pings. All other auxiliary receive counts remained zero.
This qualifies resource opening and firmware debug reception, not ACL delivery.
These definitions originated in host-build analysis, not exact target 25G76.

The first capture's 64 KiB limit dropped 156 records before the abort was fully
captured. The new 1 MiB buffer and write-only `capture_reset` (write `1`) allow
a fresh bounded capture immediately before a radio test. Reset only clears
the host capture; it does not send a firmware command. Debug payloads are
binary, without printable strings; decoding remains in progress.

## Native startup sequence

The earlier `OtherResources/n1/N1_BETA_FIRST_THREE_VENDOR_WIRE_CONTRACT_2026-07-22.md`
recovers the PROXIMA startup chain: WriteBdAddr `0xfc06` (6 bytes), InitDone
`0xff0b` (16 bytes of `ff`), SetRandomSeed `0xfe44` (24 random bytes), then
HCI Reset. Independent assembly checks on host 26A5425a confirm the two new
vendor packet formats: functions `0x1001392ca` and `0x1001395c0`.

The opt-in `native_startup=1` driver setup hook sends all three before the
Linux core's first reset. It waits for kernel RNG readiness, uses a temporary
locally administered diagnostic address in native byte order, clears its
local seed after use, and stops on any failed/malformed completion. A debugfs
step count records how many commands succeeded. Factory address provisioning
remains unresolved. Hardware qualification now passes:

| Test | Result |
| --- | --- |
| Native startup | All three commands succeed before the first Reset |
| Address readback | Both standard reads match the temporary native write |
| Passive 1M scan, 8 seconds | 304 advertising reports; stop succeeds |
| Active 1M scan, 8 seconds | 590 advertising reports; stop succeeds |
| Repeated passive scan | 407 reports; stop succeeds |
| Standard management discovery | 29 reports, 26 unique BLE devices; start/stop succeed |
| Transport after these tests | Beta exec=2 / IPC=2, zero TX/RX errors |
| Wi-Fi regression | 5/5 Google pings after each experiment |

The raw-scan sequence left management discovery busy (status 0x0a). A management
power-off/on cleared that state; the next standard discovery passed. This was
a host discovery-state transition, not another firmware abort. The HCI captures
are `native-startup.btsnoop` and `native-startup-management.btsnoop` under the
private Bluetooth log directory. The proven RAM image is preserved in
`artifacts/bluetooth/discovery-proven/`.

The raw scan probe now chooses public address type when the controller has
a programmed address and random address type otherwise. Raw BLE diagnostics
disable BR/EDR while unpowered. An independent opt-in `--native-host-awake`
sends the recovered `0xfda0` payload `00 00`. The standalone `native-host-awake`
mode sends only that command to an already powered controller. It was accepted
on hardware during the headset connection experiment; it did not unblock
standard Create Connection.

## ACL connection-data candidate

The newer host daemon's Airship writer `0x1000f57ba` constructs ordinary ACL
frames as handle LE16, length LE16, payload, without a packet-type byte. Its
connection-kind lookup selects separate BR/EDR and LE services. The native
service table maps their RX paths to HCI ACL packet type 2. The LE path with
PB bits 3 instead selects LEA and is deliberately not accepted by this candidate.

`enable_acl=1` requires both LE and auxiliary rings. It supplies separate
bounded BR/EDR and LE transmit queues on rings 2 and 4, validates HCI lengths,
selects the ring using the core's connection handle/type, and dispatches RX
rings 8 and 10 to the HCI core. Known optional Airship RX record sizes are
validated and excluded from the HCI packet. Rejected skb ownership remains
with the HCI core, and all three accepted-packet queues are purged on close.

The W=1 module and complete bundle build. A fresh-boot regression with ACL
enabled found 27 BLE devices, completed discovery start/stop, retained zero
transport errors and passed 5/5 Wi-Fi pings. ACL counters remained zero: this
qualifies the discovery regression, not the data path. No pairing, ACL transfer,
HID, audio or ISO result is claimed. The user chose a headset/speaker for the
first connection test; the subsequent inquiry identified their AirPods.

The bundle now includes `l2ping` and `sdptool` with matching loader/libraries
for post-pairing BR/EDR data tests. `analyze-bt-snoop.py` emits only aggregate
counters, opcodes/status, startup ordering and address-readback match counts.
It validates complete packet lengths and rejects truncated capture records.

A 1 MiB KIS verification read truncated during the first ACL image upload.
No guest boot was attempted after that failed verification. Scoped recovery
and 256 KiB verification transactions then read back every Image/initramfs/DTB
byte successfully and booted the candidate. Resident Stage 1 stays unchanged.

## Headset connection tests

The first headset test used native Linux with the ACL candidate bound and powered:
`arm_auxiliary_rings=1 native_startup=1 enable_acl=1`. Beta is exec=2 / IPC=2,
with 190 commands, 232 events and zero transport errors. SSP, Secure Connections
and bondable BR/EDR+LE settings are enabled; I/O capability is NoInputNoOutput
for the requested headset/speaker test. ACL TX/RX counts remain zero.

The first BR/EDR inquiry completed with HCI status 0 and no peers found. The later
Stop Discovery status 0x0b meant discovery had already ended, not a firmware
failure. A new user-requested scan identified their AirPods at strong signal.
A final Wi-Fi check passed 5/5 Google pings. The target monitor and
temporary host HTTP server were stopped; the KIS port has no active owner.
Do not reboot or rebind merely to demonstrate this result. Recheck live state
before continuing, because the user may change the target's boot state.

Private proof: `acl-candidate.btsnoop`, `acl-discovery-regression-01.log`,
`headset-discovery-private-01.log`, and `acl-final-state-01.log` under
`logs/bluetooth-20260909/`. The capture validates native initialization before
reset, three matching address reads, and zero HCI command failures.

The headset is now identified. Three pairing attempts against that exact peer
were rejected by N1 HCI Create Connection (`0x0405`) with status `0x0c` (Command
Disallowed), which Linux maps to management Busy (`0x0a`). This is a controller
command rejection, not evidence of stale management state. Management power
cycling and a successful native HostAwake (`0xfda0`, bytes `00 00`) did not
change it. The supported-command bitmap advertises Create Connection. No ACL
data was transferred and the controller never left exec=2 / IPC=2.

Private proof: `airpods-discovery-private-01.log`, `airpods-pair.btsnoop`,
`airpods-awake.btsnoop`, `airpods-host-awake-01.log` and
`airpods-final-state-01.log`. Use the console receipt for HostAwake acceptance;
the snoop's captured outgoing-command stream starts with Create Connection.

New native leads in host 26A5425a are `_ACI_HRBCreateConnection` at
`0x10012e224` (opcode `0xff7b`) and `_ACI_ConfigureCreateConnection` at
`0x10013853f` (opcode `0xfdac`). The HRB path has now been traced; the latter
command concerns connection filter accept-list tuning and is not used here.
`native-connection-leads.json` and `native-hrb-connection-evidence.json` hold
the private analysis and binary provenance.
Do not claim ACL data, pairing or audio success from discovery alone.
No new Bluetooth changes have been pushed.

### Native Proxima connection candidate

The native ACL creation function `0x100180edc` selects HRB Create Connection
when the Proxima family gate is set and proprietary feature bit 9 is present.
StackManager `0x10056fb08` enables that gate after its controller-family check
with value 3000. Native Read Proprietary Local Features is opcode `0xffa3`,
without parameters; its reply contains status and a little-endian 32-bit mask.
The live M5 accepted this read-only command and returned `0x00001676`, including
bit 9, with exec=2 / IPC=2 and zero transport errors.

`native_hrb=1` is an experimental driver option requiring native startup and
ACL transport. After ordinary HCI initialization it reads and validates that
capability. The outgoing standard Create Connection request remains intact in
Linux; only its DMA wire representation changes to opcode `0xff7b`, a 15-byte
payload. Native address byte order and the first 13 parameter positions match
standard HCI. The Proxima call uses tagging=false, role_switch=0 and two trailing
zero HRB fields. Only the matching vendor Command Status opcode is restored to
`0x0405` before delivery to the core; its real status and command credit remain
unchanged. The private diagnostic capture preserves the native request/status.
Native cancellation uses the ordinary Create Connection Cancel path.

The module builds with W=1, and the complete image bundle builds. The fresh
RAM boot passed complete kernel/initramfs/U-Boot/DTB readback verification.
The live controller accepted the native capability query both during initial
setup and management power-on. The read-only probe's standalone
`native-features` mode works on an already-powered controller without scanning
or changing management power. Its private proof is `hrb-native-features-02.log`.

The new BR/EDR inquiry completed successfully without discovering the headset.
One bounded pair request to the previously identified peer then exercised the
translation: the private wire capture contains opcode `0xff7b`, 15 parameters,
and Command Status **0x00**, credit 1. The core snoop contains the corresponding
logical `0x0405` status 0x00. Connection Complete later returned **0x04 (Page
Timeout)**. This gets past the earlier immediate Command Disallowed rejection;
it does not establish a link or demonstrate pairing. A timeout alone does not
prove why the peer was unreachable. The user has been asked to re-enter pairing
mode, and no readiness answer has arrived yet.

Private proof: `hrb-wire-private-01.bin`, `hrb-candidate.btsnoop`,
`hrb-pair-private-01.log`, `hrb-discovery-private-01.log`, and
`hrb-final-state-01.log`. The snoop verifies native first-three startup order,
two matching address reads, zero command failures, and zero ACL packets.
Controller state after the attempt: exec=2 / IPC=2, 92 commands, 94 events,
one native request/status, no pending native status and zero transport errors.
Wi-Fi passed 5/5 Google pings before and after the attempt. The monitor is
stopped. Current RAM image uses `native_hrb=1` alongside native startup and ACL;
module SHA-256 is `3d813d3b4d42f5894c612d8542a2a7e99a76db86d4810c16cdacb1eb1b9e266e`.
Cancellation, ACL exchange, pairing and audio remain unqualified. All new
Bluetooth work is local on `j813` and has not been pushed.

A final discovery regression found 25 unique BLE devices (30 reports), with
successful discovery start/stop, zero transport errors and another 5/5 Google
pings. Current counters are 96 commands / 138 events, still zero ACL TX/RX.
Proof: `hrb-discovery-regression-01.log`. The native connection-command acceptance
was reported to standup once, HTTP 204; payload/receipt are
`standup-native-connection-accepted{,-receipt}.json`.

### Headset radio connection and native link type

After the user confirmed flashing-white pairing mode, two BR/EDR inquiries
again returned no devices. A direct pair request to the previously identified
AirPods nevertheless produced **Connection Complete status 0**, handle 1,
native link type **0xf1**, encryption 0. The peer disconnected 8.6 seconds later
with reason 0x13. This is a successful physical connection, superseding the
earlier page-timeout-only result. It is not pairing or an ACL data-path pass:
Linux sent no remote-feature/authentication commands and ACL counters stayed zero.

Native host HCI flow `0x1001a5195` routes types 1, 0xf0 and 0xf1 to the same ACL
connection path. Handler `0x10017dd19` also accepts all three, storing additional
proprietary capability flags for the latter two. Linux instead looks up the
pending connection by the exact reported type and does not match ACL_LINK 1 to
0xf1. The new candidate normalizes 0xf0/0xf1 only in exact-length Connection
Complete events when the native firmware capability is confirmed, preserving
the original event in the private capture. It counts these translations in
debugfs. Hardware pairing with this additional normalization is pending.

Proof: `airpods-ready.btsnoop`, `native-hrb-linktype-evidence.json`, and
`airpods-radio-link-regression-01.log`. Beta stayed exec=2 / IPC=2 with no errors;
Wi-Fi passed 5/5 pings. The attempted host macOS reference inquiry did not start
and is inconclusive; its initial completion was caused by stopping at the
deadline, not a completed search. The diagnostic source now reports this timeout
explicitly. Do not interpret its empty result as evidence the headset is absent.

### Linux-recognized connection and ACL data qualification

The link-type candidate booted through complete readback verification and passed
the captured-event fixture (0xf1, 0xf0, disabled capability, short event,
unknown type, and preservation of every byte except the link-type field).
Module SHA-256:
`c21cc39e8acbe289cf64bc91192292b02cf25f2502df57e12418855229560fb4`.
The driver normalized the native 0xf1 completion to ACL_LINK 1. Linux then
issued successful remote-feature, extended-feature and remote-name requests,
followed by Authentication Requested, Link Key Negative Reply and I/O Capability
Reply. The name returned by the peer matches the user's confirmed AirPods name.

Pairing reached Simple Pairing Complete and Authentication Complete, both with
status **0x18 (Pairing Not Allowed)**. No link key was generated. Both host I/O
capabilities 3 (NoInputNoOutput) and 1 (DisplayYesNo) hit that status, with
otherwise successful HCI command responses. Native host-awake 0xfda0 has since
been sent successfully in this boot; another fresh pairing-window test is
pending. The user has been asked to restart flashing-white mode because the
previous window spanned a RAM reboot. No resets or unpairing of the headset
or phone were performed.

Basic BR/EDR ACL data now works. Three 32-byte L2CAP echo requests received
responses with matching identifiers. SDP browsing returned PnP, GATT, UARPS,
AAP, Handsfree, AVRCP Target/Controller and Audio Sink records. One recursive
browse connection was refused, so a focused `A2SNK` query was run afterward:
it returned the Audio Sink/A2DP record without errors. This confirms the audio
endpoint, superseding the earlier possibility that only a Find My endpoint was
selected. The user also confirmed that the advertised name was correct.

Echo integrity remains unqualified: each request received an empty echo response
followed by a non-matching data-bearing response. `l2ping -v` rejected the first
empty response when expecting 128 bytes. Do not describe this as a successful
byte-for-byte payload echo. The separate SDP transaction capture contains five
Service Search Attribute requests and five parsed responses, with ACL payloads
up to 259 bytes received. Capture has 20 ACL TX / 24 ACL RX packets; the later
focused audio query advanced the live counters further. All queues drained,
transport errors remained zero, and Wi-Fi passed 5/5 Google pings after the
connection/data experiments and after host-awake.

Private proof under `logs/bluetooth-20260909/`: `hrb-linktype.btsnoop`,
`hrb-acl.btsnoop`, `hrb-linktype-wire-private-01.bin`,
`hrb-l2ping-private-01.log`, `hrb-l2ping-verify-private-01.log`,
`hrb-sdp-private-01.log`, `hrb-audio-sdp-private-01.log`,
`hrb-acl-verify-01.log` and `hrb-linktype-host-awake-01.log`.
The snoop analyzer reports connection, authentication and pairing statuses,
disconnect reasons and link-key notification counts without exposing identities
or key material. At this boundary, pairing and encryption were unqualified;
the next section supersedes that result. New Bluetooth work remains local on
`j813`. The basic ACL milestone was posted to standup once (HTTP 204), in
`standup-acl-data-working{,-receipt}.json`.

### Pairing and link encryption qualified (2026-09-10)

The user's fresh white-light readiness reply was followed immediately by a
DisplayYesNo pairing attempt without rebooting or changing the kernel. It
succeeded: Simple Pairing Complete and Authentication Complete both returned
status 0; Linux received a type-0x07 link key (unauthenticated P-256, not a
numeric-comparison/MITM claim). Encryption Change returned status 0, enabled 2,
and Read Encryption Key Size returned 16 bytes. Management reported successful
pairing and a new link key with store hint 1. No confirmation prompt was needed.
The subsequent disconnect was initiated locally (HCI reason 0x16), not another
pairing rejection. All 11 HCI commands completed without error.

Both the fresh pairing window and the previously accepted Native HostAwake
were present. This establishes a working sequence, but does not isolate which
condition explains the earlier 0x18 failures. No headset reset, phone unpairing,
resident Stage 1 change or further kernel change was required.

Private capture `hrb-freshwhite.btsnoop` and private pair log
`hrb-freshwhite-pair-private-01.log` preserve the evidence. Captures now contain
link-key material: keep them local with restricted permissions, never publish
their raw contents. The analyzer reports only key type, encryption state and
key size. Wi-Fi passed 5/5 Google pings. A subsequent focused audio SDP query
and three ordinary L2CAP echoes also succeeded without pairing again; the
`hrb-reconnect.btsnoop` capture has 12 ACL TX and 15 ACL RX packets. That SDP/echo
test did not itself request encryption and does not prove encrypted reconnect.

Current handoff: the same RAM-booted Linux candidate remains active with native
startup, ACL and HRB enabled, I/O capability 1 and Native HostAwake accepted.
The link key exists in the current controller/kernel session. Persistent bond
storage across reboot is not implemented by these standalone management tests.
Encrypted audio-service reconnection is now qualified. The bounded
`n1-bt-audio-probe.c` requests BT_SECURITY_MEDIUM on the AVDTP L2CAP channel,
then validates matching Discover and Get All Capabilities responses. Hardware
reported security level 2 and a 16-byte key, three available audio sink
endpoints, and SBC capabilities on the first sink. The capture contains a
successful Link Key Reply (0x040b), Authentication Complete status 0,
Encryption Change status 0 / enabled 2, and 16-byte key-size readback, with
no new pairing or link-key notification. Thus the existing session key was
reused. This is encrypted AVDTP signaling, not audio streaming.

Proof: `hrb-encrypted-avdtp.btsnoop` (12 ACL TX / 13 ACL RX) and
`hrb-encrypted-avdtp-01.log`. The probe compiled with `-Wall -Wextra -Werror`,
was uploaded as a public RAM-only artifact with SHA-256 verification, and
exited 0 after both accepted signaling transactions. Run it with the staged
loader and libraries, passing only the verified selected peer address. It
does not print peer identity or key material and does not store bonds.

Latest verified counters: 145 commands / 256 events, ACL TX 55 / RX 63,
all queues empty, zero transport errors, and Wi-Fi 5/5 pings after the secure
service test. All capture and pair clients have ended. The current kernel
session retains the paired key. Audio playback, persistent bond storage,
LE ACL data and SCO/ISO remain unqualified.

This pairing/encryption milestone was posted once to agents standup, HTTP 204,
in `standup-encrypted-pairing-working{,-receipt}.json`. The next audio boundary
is AVDTP stream configuration and actual decoded playback; discovery of an
SBC-capable endpoint alone does not satisfy it. Preserve the current RAM boot
and paired kernel state while testing that boundary.

## Standup contract

The initial Bluetooth campaign update and the HCI/scan-boundary update were
delivered once each (HTTP 204). The latter payload and receipt are
`logs/bluetooth-20260909/standup-hci-scan-boundary{,-receipt}.json`. Before
resuming after context loss, read the full user-supplied webhook guide at
`/Users/mcruz/.codex/attachments/0687450c-4282-4386-85fd-d65ce5f9259a/pasted-text.txt`.
Use only the explicitly supplied runtime webhook. Never store its value.
