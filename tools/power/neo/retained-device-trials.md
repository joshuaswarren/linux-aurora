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
