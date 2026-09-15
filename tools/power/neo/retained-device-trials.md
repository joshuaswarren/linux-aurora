# J700 retained-device trials, 2026-09-14/15

Local branch `eryk_neo_deepsleep`; no upstream fetch or push. These are
experimental opt-in paths, not qualified replacements for the panel-off,
Wi-Fi firmware-ownership sleep path. Captures are under the enclosing
workspace's `artifacts/neo-suspend/`.

## GPU isolation

On kernel #26, boot `41ed496a-acec-4bee-9f4d-44ad6990ac35`, SDDM stopped
and no DRM clients remained. The matched 180-second battery-counter pair:

| State | Elapsed seconds | Whole-system watts |
| --- | ---: | ---: |
| GPU bound, panel off, Wi-Fi firmware ownership | 180.9184 | 0.78435 |
| GPU unbound, GFX actual power state zero | 181.0911 | 0.78540 |

No saving is resolved. Do not assign the 1 mW difference to GPU power.
`gpu-isolation/comparison.json` retains the calculation. This trial needed
a reboot: GPU rebind failed on a stuck PMGR transition. Thus it characterizes
power isolation, not working GPU suspend/resume. After the failed restore,
logical genpd said on but PMGR still read `0x0f002b00` (actual zero).
Read-only snapshots on the subsequent healthy boot showed normal idle GFX
`0x1000230f`: auto enabled, target active, actual off. Logical domain ownership
alone does not establish physical GPU residency.

## PMP retained RTKit transition

Both AP and IOP acknowledge IDLE `0x201`. That is insufficient to qualify wake.

* #27 (`pmp-idle/`): IOP-first retained wake timed out.
* #28 (`pmp-idle-apfirst/devices-ready/`): AP-first wake also timed out;
  TX changed from `0x21101` to `0x101201`, with no expected response.
* #29 (`pmp-idle-stop/`): clearing/restoring ASC RUN after idle ACK did not
  fix wake. DART also vetoed normal suspend because its client CPU was already
  stopped. No valid sleep measurement resulted.
* #30 moves the transition to late suspend/early resume, and polls native
  ASC idle status before stopping RUN. Qualification is pending.

On failure the driver keeps DMA pinned and host policy paused. The PM helpers
now reject a returned PM write if PMP reports `failed=1`; a sysfs write's
success alone previously hid a failed complete callback. The #27 awake
calibration overlapped a diagnostic PM test and must not be used.

Native evidence uses J700 25F84 kernelcache SHA256
`c4de7d65a016ff68f6ac2a335517da82c9be966175c80afd8a738dd00faa1c61`.
Symbols were regenerated from each actual extracted binary. In ASCWrapV6,
`_isIdle` at `fffffe00089189c4` checks CPU_STATUS bits 1:0; `_runCPU` at
`fffffe0008918a64` clears RUN then bit 5. AppleA7IOP::stopCPU at
`fffffe000891ad30` waits for idle before clearing RUN. This informs ordering;
it does not establish that the complete J700 PMP retention contract is met.

## Inherited DCP ownership

Contrary to an old device-tree comment, current firmware leaves DCP running:
CPU_CONTROL `0x10`, CPU_STATUS `0x6d`. Display SYS, FE and CPU PMGR states
are active. Linux has no bound DCP/mailbox/display-DART owner in this boot.
The panel-off hook does not stop firmware or scanout.

The opt-in `neo-dcp-retained` module adopts only the management transport,
without resetting firmware or changing existing mappings. On #29, an awake
AP-ON request produced a 0x26000-byte shared-buffer request and timed out.
The module rejects new buffer ownership and latches failure. The later
version records the requested DVA for investigation. It must not free inherited
DMA or pretend that this handshake qualified firmware sleep. Reboot removes
this pinned diagnostic module.

A separate explicit power-isolation option in `neo-display-sleep` pauses a
hardware-idle, unowned DCP and requests PMGR retained clock state 4, preserving
all memory/mappings. The first depth-1 platform-stage test on #30 lost host
connectivity and required DebugUSB reset. No wattage or successful restore
can be claimed. Leave `pause_cpu=N` and `clock_depth=0` for normal operation.
The exact hang boundary awaits retained logs; do not infer that a clock
transition completed from the requested value.

## Measurement boundary

Wi-Fi's previously qualified marginal saving is approximately 75 mW; its
600-second screen-off confirmation was 0.7625 W total. No independent PMP
or DCP wattage is established by these failed transitions. No macOS sleep
measurement has been made. Battery-counter comparisons are whole-system
marginal deltas, not additive per-rail meters.

The first DCP clock trial boot used `no_console_suspend`, leaving framebuffer
and dockchannel console output live. This is an admission defect, not proof
of the hang's precise cause. Both display clock gating and DCP firmware sleep
now require `console_suspend_enabled`; captures include its runtime value.
Set `/sys/module/printk/parameters/console_suspend` to `Y` before qualification.
The previous boot's journal ended at module probe because journald was frozen;
`display-clock-isolation/platform/result.json` remains `running` and is retained
unchanged. Recovery returned to the normal #6 kernel, battery 21%.

The console-suspended depth-1 repeat also lost connectivity and required
DebugUSB reset. Console suspension is necessary isolation hygiene, but did
not resolve this hang. No deeper display clock depth has been attempted.

## PMP sleep-mode discovery

The #30 late/early test reached both idle ACKs, CPU_STATUS `0x6e`, and
RUN clear; DART retention checks succeeded. AP wake still timed out after
RUN restoration. Capture: `pmp-idle-late/platform/`. Thus moving callbacks
and waiting for idle corrected admission/order but did not fix firmware wake.

Disassembly of the exact PMP image identifies power context `0x104ff30`.
Its word at +0x64 (`0x104ff94`) is 1 in the image and also 1 in a bounded,
aligned, read-only live capture at physical `0x30054ff94`, translated using
`apple,resident-segments`. The text maps wire `0x201` to internal state 8.
At `0x1024ab4`, state 8 with mode 2 uses the returning idle implementation
`0x1027068`; other modes branch through `0x10270ac` to the non-returning
WFI loop at `0x100214c`. The interrupt PM callback at `0x102a9a0` also checks
mode 2 and bypasses the mask-reset path. This explains a plausible cause of
an acknowledged idle with unconsumed wake messages; the live program counter
was not captured, so it is not a direct PC trace.

`pmp_v2.fw_wfi_s2idle=1` is a boot-only, default-off experiment selecting the
existing mode 2 before CPU RUN is first set. It requires the exact 25F84
UUID/TEXT digest, writable resident translation, stopped CPU and original
DATA word 1, then verifies the value 2. It does not change firmware TEXT,
DMA ownership, frequency/voltage policy or unknown firmware revisions.
Hardware qualification remains necessary; the switch is not a claim of a
complete native sleep contract. Do not patch the word in a running firmware
session. Decompiler output incorrectly merged the non-returning helper with
its following function; use the retained instruction disassembly for this
control-flow conclusion.

The user observed 1TR after a DCP trial and manually returned Neo to Omarchy.
DebugUSB reset returning successfully does not prove automatic OS recovery.

Kernel #31 successfully booted with mode 2. Its platform-stage test acknowledged
IDLE, then AP wake returned success with AP=0x20 and IOP=0x20. The generic
trial helper nevertheless sent IOP INIT and timed out. `pmp-wfi-mode/platform/`
retains the failed overall test; this is progress in wake protocol, not yet
successful sleep qualification. The follow-up recognizes IOP ON after the
fresh AP wake acknowledgement and does not issue the redundant INIT route.

Kernel #32 (`9b3b5ed75`, Image SHA256
`9bb66cce1e7515fb9798dd5241e8d6bb2a30c5216ab8a7e620cb213a0ad7ac07`)
passed the platform-stage test and a real 10-second RTC sleep. All six CPUs
entered s2idle; the panel restored; PMP remained `failed=0` with its DMA epoch
retained. No ASC RUN stop or PMP domain gate is used with this returning mode.

The first 180-second pair on the same #32 boot measured 0.78922 W with PMP
retained normally and 0.76722 W with acknowledged firmware idle. The initial
22 mW difference is provisional pending a reverse-order pair. Calibration was
3.59453555 raw counter units/uAh. Both tests passed RTC, CPU and panel checks.
PMP's PMGR word remained `0x1f0f03ff`; firmware WFI is not a full domain power
gate. GFX actual remained zero; Wi-Fi and PMP IRQ counts stayed unchanged
across the noirq intervals. Captures: `pmp-wfi-apwake/`.

The next 180-second PMP-idle repeat woke via RTC and acknowledged AP/IOP ON,
but DART retention admission returned -EBUSY and latched the epoch failed.
The helper correctly rejected power for this run and skipped the reference
repeat. Therefore the 22 mW reduction remains provisional, and #32 is not yet
qualified for reliable repeated sleep. Both PMGR gates read ACTIVE afterward
(PMP `0x1f0f03ff`, PMP-DART `0x1f0003ff`).

The follow-up polls the always-accessible PMGR for up to 10 ms when an
ACTIVE-target gate has not reached ACTIVE. It retains the ACTIVE requirement,
rejects unexpected targets/reset/disable, does not write power registers,
and only then reads DART/APF state. Captures now include PMP-DART's gate.
A timeout still fails the epoch. This tests a transient gate hypothesis;
the initial failed sample was not logged by #32.

Kernel #33 (`aa1d9a9e7`, Image SHA256
`f193cee4338ec23e92363cc260e8b6c2e46d874311998ccd610514acd6a9f4b3`)
passed six consecutive 10-second RTC cycles with PMP firmware idle. All six
CPUs entered s2idle, the panel restored, and PMP retained health after every
cycle. None of these six cycles needed the new gate wait, so they do not yet
prove that the intermittent gate failure is fixed. Longer paired measurements
are required. Captures: `pmp-gate-wait/short-00` through `short-05`.

## DCP crash capture and missing logging endpoint

The #32 awake-wake trial's requested buffer at DVA `0x10008784000`, size
`0x26000`, was captured read-only through the inherited DCP DART at
`0x412340000`, SID 23. No DART control, table or firmware writes were made.
The three display PMGR gates were ACTIVE before inspection. The exact capture
SHA256 is `934a0535c617927c2ca130ac3cab7f9c36bc5c64434e269bbfdf76322c0dcbed`.
Translation and binary are retained under `dcp-retained/`.

The HGLC container includes a version-0x303 CLHE record at offset `0x1000`:
`ASSERT!platform_logging_common.cpp:406 MBI_ENDPOINT_BUILT_IN_OSLOG not enabled!`
The Cver UUID matches the analyzed DCP firmware; its explicit slide is
`0x3b0000`. Subtract that slide from captured stack addresses before looking
them up. The final entry in the primary mailbox history is our AP-ON request
`0x00b0000000000020`. The firmware function at unslid `0x040179c4` checks
the first AP transition from internal 0x20 to 0x21 and asserts unless the
endpoint handle at `0x044d03d8` (endpoint 8) is enabled. This is an awake
handshake failure, independent of attempting a DCP clock gate.

`native/rtkit-crash-sections.py` retains bounded nested record offsets, UUIDs,
slides, messages and raw mailbox history. The trial module now requires an
explicit `start_oslog` write before AP wake or sleep; it queues endpoint 8
only. It still refuses new shared buffers and latches failures. A queued
endpoint start is not evidence of completed buffer negotiation or healthy
firmware wake. Test these while awake before any sleep/clock experiment.

The OSLOG-start probe on #33 removed the assertion: a fresh AP-ON ACK returned
with AP/IOP 0x20. Firmware then advertised an existing OSLOG buffer at
`0x100087b4000`, size `0x5ff8`. The old diagnostic deliberately rejected it,
so its overall result remains failed. Capture: `dcp-oslog-awake/`; no clocks
were stopped. The follow-up maps only already-translated, physically contiguous
buffers through the inherited SID23 tables, checks the exact observed DART
layout and ACTIVE display gates, and refuses allocation requests. It writes
no DART control or PTEs and never frees firmware backing pages. The module
remains pinned until reboot. Awake and sleep qualification are still required.

## Longer PMP repeat and measurement duration

All four 180-second intervals on #33 completed with RTC, six-CPU, panel and
PMP checks passing. Alongside the six short tests this is ten returned cycles,
eight with firmware idle enabled. None exercised the gate-wait fallback, so
the original intermittent failure was not reproduced and its fix is not proven.

With a fresh same-boot calibration of 3.600894309 raw units/uAh, the sequence was:

| PMP firmware idle | Duration (s) | Calibrated battery power (W) |
| --- | ---: | ---: |
| enabled | 181.613 | 0.780460 |
| disabled | 180.804 | 0.769984 |
| disabled | 180.747 | 0.770269 |
| enabled | 180.822 | 0.764663 |

The paired reductions were -10.48 mW and +5.61 mW. The earlier provisional
22 mW reduction is **not reproduced**: no repeatable saving from this PMP WFI
mode is resolved at the current precision. Do not label the capacity-gauge
estimate (3.827 W for the first interval) as measured sleep consumption.
Captures and comparison hashes: `pmp-gate-wait/{cal60,pair1.json,pair2.json}`.

The user requested **120-second power intervals** for subsequent comparisons.
Use short functional cycles first; do not default back to 10-minute runs.

The existing-buffer follow-up (`2b1b654be`, module SHA256
`ca6947c25d4cb57b55e55f397ee7f0378665e8fcb44b391c263981c7003cd5b0`)
passed awake wake with `failed=0`. The two-page OSLOG host mapping translated
to PA `0x101eb488000`; the original SID23 tables remained unchanged.

The subsequent platform-stage test acknowledged both IDLE states (0x201),
then sent management type 0x0c and stopped responding to AP wake. CPU_STATUS
was `0x4e` afterward, RUN remained set. Linux, panel and Wi-Fi returned, but
DCP remained failed/touched and the helper rejected the run. There was no
real timed sleep or power measurement. Captures: `dcp-shmem-awake/platform/`.

Native RTBuddy identifies type 0x0c as `_handlePowerIdleGated`: it sets an idle
latch and wakes the waiting host thread, without sending a reply. Thus do not
invent an ACK for this message. The next bounded trial uses the already-present
ASC idle-status poll and RUN stop/start, with clock_depth=0. Generic helpers
now capture DCP status/parameters and reject latched failure or an unrestored
transition, rather than accepting a returned PM write as DCP recovery.


## DCP AP-only follow-up and OSLOG flush

The ASC stop/start repeat after full IOP IDLE also failed AP wake with -62,
without any display clock gate (`dcp-asc-stop/platform/`). Restoring RUN alone
is insufficient. On #34 (`2e36424e0`), AP-only IDLE, keeping IOP ON, passed the
platform-stage cycle with DCP sleeps=1/wakes=2 and failed=0. The next real
10-second attempt timed out waiting for AP IDLE before any CPU entered
s2idle; rollback AP wake also timed out. Retain the failure under
`dcp-ap-idle/short-10/`; there is no valid power estimate.

Between those attempts DCP emitted endpoint-8 message `0x0200000000003418`.
RTKit ignored this as unknown. Native RTBuddy's OSLOG `_messageHandler`
(`fffffe000adf86b0`) dispatches type 2 to `_handleFlushRequest`
(`fffffe000adf9460`). That handler copies the coalescer to a temporary buffer,
then replies `(message & 0xf0000000ffffffff) | (2ULL << 56)` and advances its
consumer position. Types 3/4/5 convey source metadata. This is a plausible
unhandled logging backpressure dependency, not yet proven to cause the hang.

The follow-up adds an explicitly installed OSLOG callback, separate from the
existing external-module ops layout. The diagnostic admits only coalescer 0,
a mapped buffer of at most 1 MiB and a producer offset within that buffer.
It copies the entire ring before acknowledging, retains the latest snapshot
in a root-readable sysfs binary attribute, and counts successful replies.
Unknown protocol forms or allocation/send failures latch the trial failed.
No ring contents are interpreted as format strings. Other RTKit users retain
their existing behavior. Hardware qualification is pending.

Firmware `FUN_04034dac` confirms the reply mechanism: type 2 updates the
consumer at coalescer +0x10, clears the outstanding-flush bit in flags +1,
and wakes the +0xb0 waiter. The OSLOG lifecycle callback `FUN_040346c8`
waits on that same waiter while a flush is pending. Thus ignoring the flush
can block a power transition; the next boot tests this causal hypothesis.


Kernel #35 (`20f69df97`, Image SHA256
`0d26d2e79dd757d4d277ad150c77257a1f52e6057bb7f37c5449233dbfd5592a`)
passed the platform-stage cycle and three consecutive 10-second real RTC
sleeps with AP-only DCP idle. Every real cycle entered s2idle on all six
CPUs and restored the panel. DCP reached sleeps=4/wakes=5, failed=0,
touched=0, log_flushes=3. This reproduces the previously failing repeat with
log acknowledgements in place. Raw ring snapshots and explicit before/after
DCP counter checks are retained in `dcp-oslog-flush/`. Full IOP idle and
clock gating remain unqualified; these passes concern AP-only notification.


The same #35 boot completed four **120-second** power intervals in ABBA
order, with freshly calibrated raw-counter scaling 3.639572725 units/uAh:

| DCP AP state during sleep | Elapsed seconds | Calibrated whole-system W |
| --- | ---: | ---: |
| ON (reference) | 120.719 | 0.782937 |
| IDLE | 120.819 | 0.768238 |
| IDLE | 121.515 | 0.779303 |
| ON (reference) | 120.784 | 0.768734 |

The paired savings are +14.70 mW and -10.57 mW: **no repeatable saving is
resolved**. AP-only notification does not establish DCP processor or display
clock power-down. These are internally calibrated battery-counter estimates,
including transitions, not external-meter measurements. Capacity-gauge
compensation produced misleading 4.6–6.8 W estimates and is retained separately.
The control itself uses an adopted, AP-awake DCP; it is not proof of identity
with the loader's original quiesced state.

Every interval passed RTC wake, six-CPU entry, panel restoration and firmware
health checks. Across the platform test, three short cycles and four power
intervals, the boot completed eight PM cycles: six exercised DCP AP idle and
two left it awake. Final DCP status: sleeps=6, wakes=7 (including initial
adoption wake), failed=0, touched=0, log_flushes=7. Source identities, counter
comparisons, DCP counter deltas and ring snapshots are retained under
`dcp-oslog-flush/`. The measurement script disabled DCP sleep afterward;
Neo remains awake on #35. PMP firmware idle, DCP CPU stop and display clock
gating remain disabled. No upstream fetch or push occurred.


## DCP returning-mode investigation

Full IOP IDLE on #35, with OSLOG draining working, still timed out on AP wake:
`dcp-oslog-flush/full-idle-platform/`. It reached AP/IOP 0x201 and management
notification 0x0c, CPU_STATUS 0x4e, RUN set. The OSLOG fix does not resolve this
separate full-processor wake path. A normal one-shot reboot restored #35.

The original `dcp.bin` includes a separate low-level monitor in its first
64 KiB. It has a non-returning WFI loop at offset 0x38a0, ordinary scheduler
WFI at 0xb530, and a context-saving WFI/restore path at 0x2bb4. Do not look only
for WFI instructions in the extracted client Mach-O.

In client code, state 8 at 0x4030810 selects 0x401db04 when the power context
word at +0x64 is 2; otherwise it follows 0x401d9f8. The existing setter
0x402fac0 performs one aligned store of 2 to VA 0x445d90c. No direct caller
was found. The management PowerNap request changes a keep-awake counter;
it is not this mode setter.

A read-only capture of the ADT's boot memory region 50 (PA 0x101ea7d4000,
size 0xcbc000) located the original DATA page at capture offset 0x13000.
The first 1 KiB exactly matches the original Mach-O page at VA 0x445d000
(SHA256 0a93751f089999a9b94145988a4e6a5b4e4168ec40d5adcc715debaca54a5885).
Its client string at +0x87c matches AppleDCP-1041.120.7~1815-t8140dcp.RELEASE.
The live mode at PA 0x101ea7e790c was 1. This is a 4 KiB client page within
the retained boot allocation; assuming a 16 KiB CPU-page offset missed it.
Capture SHA256: cc9e99ea75dc7dbaf73f60c03d9502e085e2a9bcc52489c43b7d9ebbc6c91dac.
Artifacts and bounded read-only helpers: `dcp-monitor/`.

The diagnostic adds an explicit `returning_idle=2` sysfs write. It requires
J700 adoption, one fresh AP-ON acknowledgement, no prior sleep, no failure,
sleep disabled, RUN set and all display gates active. It checks that exact
DATA-page digest, client string and original mode 1 before an aligned store
of 2 and readback. This mirrors the native setter without altering TEXT,
DART tables or CPU control. The module remains pinned until reboot; no reset
of a failed epoch is attempted. Hardware qualification is pending.


The first mode-2 probe selected the guarded DATA value successfully, then
erroneously sent AP ON again while DCP was already awake. The retained CLHE
explicitly reports `Invalid AP power: 33`; this is a probe defect, not a
failed sleep/wake qualification of mode 2. Crash SHA256
`d98b33805ccefaf01794ea06522aa2f8d5ce9f72543868083ac54a2ad1a7c964`;
`dcp-returning-mode/crash-sections.json`. The diagnostic now rejects duplicate
awake-adoption writes with -EALREADY. PM wake after a touched IDLE transition
continues to use a fresh AP acknowledgement. Reboot is required after this
assertion; no in-place recovery is claimed.


The corrected mode-2 probe passed awake admission and the duplicate-wake
guard. Full IOP IDLE still timed out on AP wake at the platform stage
(`dcp-returning-mode-v2/`). A separate read-only uncached capture confirmed
the DATA word retained 2 and the context's current/target states were 8.
Thus the mode switch alone is insufficient; CPU restart/domain sequencing
remains to be qualified. No real sleep or wattage was measured.

A subsequent register diagnostic lost connectivity. Inspection found it used
byte accesses in the ASC register window; it had reused a RAM-dump loop.
Output was buffered, so the exact failing read was not retained. Do not infer
that mode 2 or a particular register caused the hang. The helper was corrected
to aligned 32-bit reads with unbuffered output; no repeat is scheduled. One
DebugUSB reset returned Neo to normal #6, followed by a one-shot #35 reboot.
The next bounded trial adds ASC RUN stop/start to returning-mode idle, leaving
all display clocks active.


Returning mode with ASC RUN stop/start also failed AP wake
(`dcp-returning-stop/`). The native expert's `require-force-wakeup` property
selects PA 0x308008000, values off=0/on=0x10. One aligned 32-bit read found
it already asserted. An explicitly guarded 0 -> 0x10 pulse after the failed
mode-2 sleep left CPU_STATUS 0x4e; it did not recover the epoch
(`dcp-force-wake/pulse.log`). This is not proof that force-wake sequencing is
irrelevant during a complete power transition.

An aligned 32-bit read of ASC +0x40/+0x44/+0x48 passed while awake:
0x000a0401/0x10/0x6d. The subsequent mode-2, stop-CPU, clock-depth-1 trial lost
connectivity. Its platform result was still running, so no successful clock
transition, wake or wattage is claimed. One DebugUSB reset returned to #6.

The follow-up addresses transport ordering before a CPU-domain gate. Native
RTBuddy waits for management type 0x0c (idle gated), then disables mailbox
interrupts before provider power changes. The previous Linux diagnostic kept
its mailbox RX IRQ and workers active during clock removal: an IRQ could
access a gated register window. This is an identified admission gap, not yet
a proven cause of the hang. New retained-transport APIs wait for a fresh 0x0c,
drain work, stop/synchronize RX, and drain the final work while clocks are
still active. They preserve endpoints, mappings and the firmware epoch.
The DCP trial restores clocks and RUN before restarting the transport and
requesting AP wake. This ordering still needs hardware qualification.

## Retained transport qualification (#37)

Kernel source `1a0ca0ee2`, Image SHA256
`16ab05ddea2ed974dad97006eba510ff27c70b0e87e1ff65d1da26c6aebde8da`.
The mode-2/stop-CPU/no-clock trial acknowledged idle-gated and paused its
transport successfully, but AP wake still timed out with CPU_STATUS 0x4e.
`dcp-transport-gate/platform/` records failure, not sleep power. A fresh boot's
clock-depth-1 trial again lost host connectivity (`dcp-transport-clock/`).
A single Fedora Left-back reset returned Neo to normal #6. Stopping RX closes
an ordering gap but has not fixed or localized the clock-trial hang.

An explicit `awake_cycle=1` diagnostic now exercises the same DCP path while
leaving host storage and networking available to persist stage markers. It
admits only a fresh healthy mode-2 epoch, stop-CPU with clock depth 1, and a
built-in tty0 and optional retained dockchannel boot consoles. It serializes
against system sleep, suspends those static consoles, and restores them only
after all touched clocks
have been restored. Display clients must be stopped and the qualified panel
helper must hold LCD-enable low. Markers around clock read/write/poll are
separated by 200 ms only in this diagnostic. The userspace collector must
poll /dev/kmsg and fsync records; this awake test is not a wattage experiment.
Hardware qualification of this diagnostic is pending.

The first awake diagnostic rejected the additional dockchannel console before
any PM operation (EBUSY, sleeps=0, failed=0). Its console admission now handles
that retained static boot console as well as tty0; all other active consoles
remain rejected. Capture: `dcp-awake-trace/`.
