# J700 residual sleep power: DCP, GPU, Wi-Fi and PMP

Investigation on 2026-09-14, local branch `eryk_neo_deepsleep`, kernel #19,
boot `eee6042d-cc10-4c2d-8cf4-66e01a884832`. No upstream synchronization,
kernel replacement, firmware replacement or device unbind was performed.
Raw evidence lives in the enclosing workspace's
`artifacts/neo-suspend/components/`. Read `display-power.md` and
`sleep-power.md` for the qualified panel controls and counter calibration.

## Subsequent Wi-Fi implementation

The later kernel #26 implementation now acknowledges HIF quiescence, hands
ownership to radio firmware and recovers ownership/HIF service on wake.
The measured same-kernel three-minute pair is 0.856 W HIF-only versus
0.781 W with firmware ownership. L1.2 adds no measured saving (0.787 W).
A ten-minute screen-off confirmation averaged 0.7625 W and woke successfully.
See [Wi-Fi sleep qualification](wifi-sleep.md) for code, the retired TX16
loader-ring fix, recovery evidence and longer measurements. The sections
below retain the original #19 investigation; they are not a claim that the
Wi-Fi sleep path is still missing. Other component wattages remain unknown.

## What can actually be assigned watts

The machine still consumes approximately **0.81 W in display-off s2idle**.
We do **not** yet have a validated wattage for each of the four components.
Neither a powered genpd nor an interrupt count is a component power meter.
The remaining total also includes DRAM, other SoC logic, USB/storage support,
regulators and the always-on wake infrastructure.

| Capture | Battery interval | Calibrated accumulated-counter power |
| --- | ---: | ---: |
| `sleep-60-result.json` | 61.033 s | 0.875 W |
| `sleep-180-result.json` | 181.549 s | 0.809 W |
| `irqs-60-result.json` | 60.879 s | 0.816 W |

All three returned through the RTC wake path, entered s2idle on all six CPUs,
and restored the display. Transition overhead matters more in the short run.
`counter-summary.json` retains input hashes, signed capacity deltas, raw
counter deltas and the calculation. The original 60-second capture is marked
`error` by the old measurement helper because remaining capacity **rose**
19 mAh; the nested sleep qualification succeeded and accumulated discharge
continued normally. The original capture was not rewritten. The helper now
retains such measurements and marks the capacity estimate unavailable.

Scale: 3.600772452250379 raw units per microamp-hour, from the previous
60-second awake current-integration calibration on this same kernel/boot.
This is an internal consistency calibration, not an external meter result.
Even a *positive* capacity drop can be misleading: the IRQ test's 15 mAh
capacity drop implies 3.414 W while the accumulated counter gives 0.816 W.
Use the calibrated counter for sleep comparisons.

## SMC telemetry: useful, but not a component breakdown

`overlay/neo-smc-power.c` reads only metadata-qualified, read-only float keys
starting with `P`, through the bound SMC transport. It does not write keys or
touch MMIO. `smc-power-sample.py` retains raw bits, decoded floats, battery
values, boot identity and timestamps. The 150-second capture contains 90
awake samples of 45 keys, all reads successful; userspace sampling stopped
during sleep. The longest complete batch took 9.77 ms.

The first sample after the sleep gap reported:

| Key | Value | Interpretation boundary |
| --- | ---: | --- |
| `PBat` | 0.77875 | Consistent with contemporaneous battery telemetry |
| `PSTR` | 0.79478 | Existing SMC driver calls this system power |
| `PDCP` | 0 | Awake value is a fixed 0.605; follows LCD state despite retained DCP |
| `PDTP` | 0.000880 | Awake value around 1.966; not an isolated DCP processor measurement |
| `PPMC` | 0.59648 | Existing driver labels it main PMU; not PMP processor power |
| `PPSM` | 0.47724 | Closely follows 0.8 times PPMC; cannot add these as independent rails |

These are **post-resume readings**, plausibly retaining the last sensor
update during sleep, not time-integrated sleep measurements. Sensor update
latency and individual rail labels remain unqualified. `PPMC + PPSM` alone
exceeds total system power, demonstrating why adding all the keys is invalid.
The `PZ*` channels were zero even while awake; that does not prove their
associated hardware was off. Do not publish guessed names for `PP*b`/`PP*l`.

## Wi-Fi: the recurring interrupt source is now measured

`overlay/neo-sleep-irqs.c` snapshots per-CPU IRQ counts in `suspend_noirq`
and `resume_noirq`. This avoids counting normal disconnect/reconnect traffic
as sleep activity and needs no periodic polling or IRQ handler changes.
It uses the running kernel's descriptor layout and rejects IRQ spaces above
its bounded 256-entry capacity. These are noirq callback endpoints, including
small noirq transition costs; they are not exact CPU WFI instruction bounds.

Over **59.780729777 seconds** (`irqs-noirq-60.txt`):

| Interrupt | Delta |
| --- | ---: |
| MT7932 MSI, IRQ 58 | **169**, about **2.83/s** |
| Architectural timer, IRQ 33 | 13 |
| SMC mailbox, IRQ 38 | 0 |
| GPU mailboxes, IRQ 59/61 | 0 |
| PMP mailboxes, IRQ 65/66 | 0 |
| NVMe mailbox/completion, IRQ 63/67 | 0 |
| USB, IRQ 71 | 0 |
| PMU RTC, IRQ 73 | 1 |

The 180-second userspace endpoint capture independently saw 551 Wi-Fi IRQs,
versus 204 in the first 60-second capture. CPU s2idle entry counts also scale
with the interval. This identifies recurring activity, not Wi-Fi's watts or
the energy cost of each interrupt. IPI counters in the module are also
retained; they are secondary CPU wake/scheduling activity, not device rails.

The current `mt_pm_suspend()` disconnects and flushes work but deliberately
retains D0, DMA and `IRQF_NO_SUSPEND` service. It never returns ownership to
firmware. Both ends advertise PCIe L1.2, but the default policy leaves it
disabled; basic L1 is already enabled. Do not equate an enabled L1.2 bit
with measured physical residency.

`pcie-power-step.py` tests Linux's coordinated `powersupersave` policy and
restores the original policy. It admits only J700 with the two expected PCI
functions and MT7932 identity, and retains both ends' PCI configuration.
The awake comparison completed with L1.2 enabled and no meaningful saving:
3.51007 W before, 3.51375 W during, 3.51420 W restored (paired difference
-0.00161 W). The original policy and disabled L1.2 state returned correctly.
This does not qualify arbitrary LTR, clock-request or firmware-ownership writes.

The subsequent RTC/display-off comparison also completed and restored the
original policy and every recorded link attribute. Requested 180 seconds,
battery endpoints 181.033622015 s, raw discharge 39023 units, mean voltage
3.8445 V: **0.82853 W**, versus baseline **0.80866 W**. Thus this experiment
demonstrated no power reduction; the roughly +20 mW difference is not enough
to assign a repeatable cost to the policy. The capture is
`pcie-sleep-180-01/`. L1.2 enable bits are not a residency counter. The
endpoint's LTR registers remained zero, unlike the nonzero latency settings
in the older native-reference experiment; no LTR override was attempted.

Apple's native driver contains a more complete path:

1. Stop TX and calibration/command activity and finish upper-layer suspend.
2. Issue `halPciePreSuspendCmd`, then wait for HIF acknowledgement.
3. Disable interrupts and perform the firmware-ownership handoff.
4. Reacquire driver ownership and restore transport on resume.

In the retained native binary, `halPciePreSuspendCmd` at `0x10003e448`
constructs legacy CID `0xf6`, payload length 36, starting `02 01 01 01`,
with the remaining 32 bytes zero. The low-power ownership helper writes
1 to chip register `0x7c060010`; the current Linux driver's corresponding
BAR offset is `0xe0010`, where its startup path writes 2 and polls bit 2
clear to acquire driver ownership. The native helper has multiple admission
guards and a prior HIF handshake: **the register write alone is not a sleep
implementation**. This binary's exact match to the running radio firmware
has not been established, and neither command was sent during this study.
`mt76/mt7925/pci.c` independently demonstrates the same broad handshake,
DMA/IRQ quiescence, ownership ordering for another MediaTek device; its
packet ABI is not automatically MT7932-compatible.

Next implementation target: an explicit, bounded HIF suspend/resume trial
inside the MT7932 driver, with admission checks, queue ownership retained,
acknowledgement validation, and rollback before changing D-state. Then test
firmware ownership, IRQ suppression, reconnect and battery energy together.
Merely masking the IRQ can strand live firmware/DMA and is not a fix.

## DCP: panel power is solved; controller power is separate

Linux currently displays through the inherited framebuffer. The qualified
`pmBL`/`gP16` hook turns off the backlight and LCD, but does not give Linux
ownership of DCP's firmware transport, scanout or DMA mappings. The 0.605
`PDCP` reading disappears at this point; it must not be cited as 0.605 W
of residual DCP processor consumption.

The local Apple DRM driver's `dcp_platform_suspend()` itself retains domains
via `device_set_wakeup_path()`, with an explicit comment that full shutdown
is unsupported. Enabling that driver alone does not provide full DCP sleep.
The native `AppleDCPExpert::_changeDCPPowerStateInternal` at
`0xfffffe0008d1f6e8` coordinates RTBuddy callbacks and expert power states;
the same binary has conditional-retention, force-wake, and display sys/FE
power-gate controls guarded by platform capability. This is a firmware and
display-pipeline transition, not another panel GPIO.

Next target: establish the selected J700 DCP sleep endpoint and acknowledgement,
stop scanout at the firmware level, retain/rebuild the exact DMA state, and
only then release the relevant domain assertions. Require a displayed frame
after wake and a battery delta. No independent DCP wattage is yet measured.

## GPU: distinguish core idle-off from full firmware/domain suspend

The `gfx` domain is retained and the platform driver lacks system-sleep
callbacks. However, `g17_live_boot.rs` already has firmware idle-power control:
render registration temporarily sends `IdlePowerOff(0)` to hold cores awake,
then sends the enable-idle argument 1 before the work doorbell. Its comments
explicitly distinguish GPU core idle-off from the two retained firmware CPUs.
This source establishes an available internal-idle path, not the physical
residency of the installed module during these measurements.

`agx_power_recovery.rs` records G17P/G17G/G17S firmware SLEEP=0x08,
NAP=0x10 and intermediate 0x21. It also explicitly leaves the executable
GFX1 transition gate closed: the G17S PIO prefix cannot be copied onto G17P.
`g17_lifecycle.rs` supports stopping/restarting or tearing down both ASC
roles, but teardown/recovery is not automatically context-preserving system
sleep. Do not terminate firmware behind live graphics mappings.

Next target: verify the current GPU's idle policy and residency, then add
submission exclusion, retirement/fence completion, coordinated firmware
sleep and context-preserving resume before removing the retained `gfx`
domain. Separate firmware/uncore savings from shader-core idle savings.
No independent GPU wattage is yet measured.

## PMP: no IRQ chatter; native v2 handler is not the old shutdown path

The current host policy and health work are paused during s2idle; firmware,
RTKit and DART mappings stay retained. The PMP message counter stayed at
1257 throughout the observed experiments, with no PMP IRQs during the noirq
interval. RUN=1 and retained memory do not prove active CPU execution or a
large power cost.

In the J700 25F84 kernelcache, `ApplePMPv2::powerStateHandler` at
`0xfffffe0009497e30` is exactly `bti c; mov w0, #0; ret`. The older
`ApplePMP::_powerStateHandlerGated` exists separately and is nontrivial.
The v2 stub does not prove that hardware lacks low-power support: its
RTBuddy/provider/PMGR layer or firmware may coordinate the transition.
It does mean that copying the older handler or stopping RUN because Linux
reports `running=1` is not justified. Inherited ADT `no-shutdown` further
requires understanding the provider lifecycle before clearing RUN.

Next target: trace the selected v2 provider's power transition and firmware
retention contract, including dependencies on SoC voltage/clock management.
Preserve live DART mappings until firmware quiescence is acknowledged.
`PPMC` is not a PMP-specific measurement; no independent PMP wattage is yet
measured.

## Native evidence and experiment identities

* Kernelcache: `/tmp/neo-kernelcache.decompressed`, SHA256
  `c4de7d65a016ff68f6ac2a335517da82c9be966175c80afd8a738dd00faa1c61`.
  Apple 25F84 J700 image; extracted `ApplePMP` and `AppleDCP` plus bounded
  disassembly are under `components/native/`.
* Extracted ApplePMP SHA256:
  `d0b0ee64e133ca705e65720dc0a0de484b40c74a1de412060b13057bb730f768`.
* Extracted AppleDCP SHA256:
  `ff6c9228d92d2d09752ad570ae031fac122ce2563960b76ee4fae3becab9ac5a`.
* AppleSunriseWLAN native reference: `/tmp/AppleSunriseWLAN.arm64e`, SHA256
  `7914176d1062f9c3c956c6f507b012a0b49f6a12710e50acad8acad8cda8da23`.
  The adjacent `.nm` file was stale: symbols were regenerated from this
  binary. Use `wifi-current-symbols.txt`, not the stale addresses.
  The analyzed binary was also copied into `components/native/`.
* SMC diagnostic module SHA256:
  `89045f24cb85016089b5f6196cde9fee4c88289bc2e34154993d65cbe6e4b27e`.
* IRQ diagnostic module SHA256:
  `42e075dcb13c88d4cdafeda58051e004cd5cebe40c07d39e4f8a473e4b8f24c7`.

Final state: all four RTC sleep cycles returned, display off/on counters
advanced together from 3/3 to 7/7, suspend failures remained zero, and the
original SDDM/Hyprland processes survived. `final-desktop.png` shows the
restored greeter. The installed known-good Asahi module hash remains
`379ba4c47d9757b46a2708eb41341ffb9001dbb94d7e3b780763060ac823bfc2`.
Both read-only diagnostic modules were unloaded after capturing results;
the qualified display/RTC/cpuidle setup remains available. No alarm or
PCIe policy experiment is pending.

The next useful reduction should be qualified one dependency group at a
time. Wi-Fi has a measured activity problem and an identifiable native
handshake. DCP has missing firmware ownership, GPU has a separate internal
idle-versus-system-sleep boundary, and PMP requires provider-level research.
After those are quiet, core/cluster, interconnect and memory-retention states
still determine the platform floor. There is no measured basis yet for a
specific sub-0.8 W target or for allocating that total among four devices.

For the Linux ordering contract, see the kernel's
[PCI power-management documentation](https://docs.kernel.org/power/pci.html)
and [device PM callback documentation](https://docs.kernel.org/driver-api/pm/devices.html).
