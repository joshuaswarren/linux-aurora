# MT7932 host-interface sleep qualification

J700 now completes acknowledged HIF sleep, firmware ownership, driver
ownership recovery and HIF wake on `eryk_neo_deepsleep`. Both
`mt7932_fullmac.hif_s2idle` and `fw_own_s2idle` default off; enable both
for the qualified firmware-owned experiment. HIF-only mode also works.
PCI remains D0 and DMA memory stays pinned. This disconnects Wi-Fi during
sleep and reconnects afterward; it does not implement WoWLAN, PCI D3 or
whole-SoC deep sleep. The development record and retained captures follow.

## Native reference

Retained `artifacts/neo-suspend/components/native/AppleSunriseWLAN.arm64e`
SHA256 `7914176d1062f9c3c956c6f507b012a0b49f6a12710e50acad8acad8cda8da23`.
This host driver is a protocol reference; equivalence to Neo's currently
loaded radio firmware is not assumed. Symbol-bounded disassembly is in
`wifi-hif-exact.dis` and `wifi-resume-and-hif.dis` beside it.

* `_halPciePreSuspendCmd` at 0x10003e448 sends legacy CID 0xf6, set=true,
  generic response=false, body length 36. First four bytes: 02 01 01 01;
  remaining bytes zero.
* `_halPcieResumeCmd` at 0x100038580 sends the same command with first four
  bytes 02 01 00 00. Constants at 0x10046bcb8 and 0x10046bcb0 respectively.
* Dispatch entry at file offset 0x488ce0 maps EID 0xf6 to
  `_nicEventHifCtrl` at 0x1003038b8. This is not unified event 0x03.
* Its event body is 36 bytes after a 12-byte event header. In our RX framing
  the header starts at byte 24, body at 36, minimum packet length 72.
  PCIe interface is body[0]=2. Suspend body[3] is nonzero, with success
  requiring body[1]=body[2]=2. Resume has body[3]=0.

The separate HIF completion prevents unrelated generic events from proving
success. Direction and a nonzero sequence must match the pending command;
sequence zero permits firmware's unsolicited event convention. Malformed or
mismatched events cannot complete the wait. A three-second timeout aborts
suspend. Even a failed suspend request invokes an explicit resume rollback,
because a lost acknowledgement does not prove the firmware stayed awake.
Rollback failure leaves the netdev detached and marks its policy failed.

## Qualification order

1. Build and boot with the parameter off; retain the known-good GPU module.
2. Enable HIF only, then `pm-test.py --stage devices` (real device callbacks,
   five-second PM test delay). Capture full HIF event payloads and rollback.
3. Only after successful round trip, run RTC/display-off tests, retaining
   noirq interrupt counters, firmware health, Wi-Fi reconnection and battery
   accumulated-counter data. Recalibrate the counter for the new kernel.
4. Firmware ownership handoff is a separate step: native suspend disables
   IRQs and calls `_nicpmSetFWOwn` after HIF acknowledgement; native resume
   acquires driver ownership before restoring IRQs and sending HIF resume.
   The corresponding always-on ownership register is BAR offset 0xe0010
   (chip address 0x7c060010), write 1 for firmware, 2 for driver. Do not gate
   DMA memory or PCI power merely because these writes have been issued.

Hardware results will be appended after execution; a compiled Image alone
is not evidence of a working suspend protocol or reduced power.

## First hardware results

Kernel #21, Image SHA256
`18bf4c97e2e42a2ef73b6f41e15a6a8b4fcf711aebcc5cd6226f6331b88cd94a`, source `b6eaeba53`, retained GPU module unchanged.
The `devices-01` test passed: EID 0xf6 carried exactly 72 bytes with
02 02 02 01 for suspend (sequence 200), and 02 02 02 00 for resume
(sequence 201). Transport geometry checks passed and WPA2 reconnected.

`sleep-10` passed real timed s2idle with the LCD/backlight hook. Noirq interval
10.71185 seconds recorded **zero MT7932 interrupts** (IRQ 58 stayed at 2207),
versus roughly 2.83/s in the prior retained-only experiment. RTC IRQ rose by
one, all CPUs entered s2idle, and the display restored. This establishes
host-interface quiescence, not firmware ownership or a wattage saving.
Captures are in workspace `artifacts/neo-suspend/wifi-hif/`; the manifest
retains binary identities and the installation script saved the previous slot.

## Separate firmware-ownership experiment

`fw_own_s2idle=1` additionally requires `hif_s2idle=1`. After the successful
HIF acknowledgement it requires all TX descriptors/tokens and calibration
requests to be retired. IRQ service is synchronized and masked before the
always-on register ownership request. It waits up to 500 ms for firmware
ownership acknowledgement. PCI remains D0 and every DMA allocation remains
pinned; neither device reset nor ring reconstruction is attempted.

Resume, including rollback from a failed ownership request, first requests
driver ownership through the always-on register and checks bit 2 cleared.
Only then does it restore IRQ service and send the HIF resume command. Failed
ownership recovery leaves the IRQ disabled, blocks command submission, skips
transport MMIO diagnostics and fails the netdev rather than accessing gated
registers. Deferred calibration is held across the HIF interval and scheduled
again after an acknowledged resume. This switch is off by default and still
requires hardware qualification.

Native reference `wifi-own-exact.dis` retains the symbol-bounded ownership
and IRQ functions. `_nicpmSetFWOwn` and `_nicpmSetDriverOwn` are tail branches
to `_halSetFWOwn` and `_halSetDriverOwn`, not empty implementations.

The same-kernel three-minute comparison completed both cycles and Wi-Fi
reconnected after each. Calibrated accumulated-counter estimates were
0.82113 W retained-only and 0.84469 W HIF-only; **no power saving established**.
Noirq Wi-Fi counts changed from 364/180.060 s to 0/180.092 s. Eliminating these
interrupts alone did not lower total battery power. The new awake calibration
was 3.588375 raw units/uAh; the capacity gauge misleadingly implied 2.64 W
and 6.19 W respectively. Full calculation: `wifi-hif/power-comparison.json`.

Review of `_halSetDriverOwn` found a conditional SER/DMA reconstruction path
(`wifi-resume-upper.dis` and `wifi-own-exact.dis`). Our retained-only mode now
checks DMA global configuration and every ring base/count immediately after
recovering ownership, **before** restoring interrupts or submitting HIF resume.
A firmware reset that loses geometry is rejected with the IRQ still disabled;
this path cannot yet recover such a reset. Kernel #22 booted with both HIF
switches off, but ownership testing was deferred to the build with this guard.

Kernel #23's first ownership device test **returned** in 5.821 s; it did not
hang the system. FW ownership was acknowledged as 0x00000004, driver ownership
as 0x00000000. The pre-HIF-resume DMA guard failed and deliberately disabled
Wi-Fi; the durable result was recovered after Fedora's external reset.
The original guard compared with fully running DMA configuration too early:
HIF suspend may itself change enable bits. The next revision captures the
acknowledged HIF DMA configuration immediately before ownership and compares
against that on ownership return. The normal active-state check still runs
after HIF resume. It also logs specific configuration/ring mismatches. No DMA
reconstruction is attempted until the actual differing state is established.

The #25 capture rules out a HIF enable-bit mismatch: DMA config stayed
0x5030b875 across ownership. Only **TX16**, the boot firmware-download ring,
changed: base became zero and count 512 instead of the host allocation's 256.
Every active command/data/RX ring retained its geometry. The device test
returned in 5.918 s; its network wrapper saved the failure and rebooted.

TX16 is never submitted after `mt_runtime_rebuild` retires the loader epoch.
The next candidate accepts only the exact observed inactive TX16 image, with
both hardware cursors zero and all software cursors/queued/used zero, while
keeping every live ring strict. `mt_scatter` now explicitly rejects runtime
submissions. It does not rebuild live DMA or rewrite firmware's retired ring.

## Firmware ownership round trip qualified

Kernel #26 (`224778573`) passed the device stage after accepting the inactive
loader ring. FW-own status was 0x4, driver-own status 0x0, both HIF events
acknowledged, active DMA geometry retained, and Wi-Fi reconnected over SSH.
The test wrapper initially treated `iw link`'s unsupported station-statistics
request as failure despite its valid Connected output; it was stopped before
its recovery reboot, and the complete partial output and live-SSH evidence
were saved. The wrapper now accepts the association output independently.

The following `sleep-10` passed actual RTC-woken s2idle, all six CPUs entering,
and restored the display. Noirq interval 9.83416 s had zero Wi-Fi interrupts
and one RTC interrupt. Firmware ownership was recovered and HIF resume
acknowledged before Wi-Fi reconnected. Captures and installation identities
are in workspace `artifacts/neo-suspend/wifi-own-tx16/`. Power measurement is
still separate from these functional transition results.

`wifi-sleep-test.py --stage devices --output NEW` wraps the functional PM
stage and waits up to 60 seconds after return for association plus a global
IPv4 address. `--seconds 10` instead runs real RTC/display-off sleep. Optional
`--recovery-boot-entry dirtyroom-gpu-suspend` saves evidence and reboots if
network recovery fails. The helper cannot recover a hung callback and does
not claim that association/address recovery proves throughput or application
stability. Use `sleep-power.py` separately for energy measurements.

The first same-kernel (#26) power pair measured **0.85598 W HIF-only** versus
**0.78120 W HIF plus firmware ownership**, a 0.07478 W (8.74%) reduction.
Battery intervals were 181.733 and 181.780 seconds; calibration was 3.603503
raw units/uAh. Both real sleeps and network returns passed, with zero radio
interrupts during the firmware-owned noirq interval. This is a total-machine
comparison, not an isolated absolute Wi-Fi wattage. The remaining-capacity
gauge again produced implausible 2.25 W and 7.95 W estimates; the accumulated
counter result is retained in `wifi-own-tx16/power-comparison-180.json`.

With firmware ownership enabled, the coordinated PCIe `powersupersave`
policy completed a three-minute RTC sleep and restored the original `default`
policy. It measured 0.78666 W versus 0.78120 W without the policy change;
no saving was established. Enabled L1.2 bits still do not prove physical
L1.2 residency. `wifi-own-tx16/power-comparison-pcie.json` retains the nested
sleep inputs and counter calculation; the outer PCIe capture retains policy
and link restoration evidence.

## Ten-minute confirmation

The screen-off, firmware-owned run measured **0.762495 W** over a 601.454 s
battery interval. Real s2idle returned in 601.123 s, RTC wake was observed, all
six CPUs entered (counts 10/1/2/1/2/5), and the display off/on counters matched.
During the 600.232 s noirq interval, Wi-Fi IRQ 58 stayed at 7135; RTC IRQ rose
once, architectural timer rose 21, SMC rose 9, and GPU/PMP/NVMe/USB IRQs did
not advance. No reboot occurred. This is a calibrated battery-counter estimate,
not external-meter metrology, and includes transition overhead. Its lower
average than the three-minute 0.781 W run is not a separate feature saving.

After wake, live SSH, association, ten successful gateway pings, a retained
SDDM/Hyprland greeter screenshot and the unchanged known-good Asahi module
hash were verified. The RTC alarm was disarmed; PCIe policy was `default`.
`power-confirmation-600.json`, `own-600/result.json`, `final-check.json` and
`final-desktop.png` under `wifi-own-tx16/` retain the calculation and evidence.

The outcome is working disconnected Wi-Fi firmware sleep/wake and a modest
energy reduction. It does not establish a platform power floor or implement
deep suspend for DCP, GPU, PMP, interconnect or DRAM. Both Wi-Fi opt-ins remain
off by default in the driver and are enabled only in the running qualification
session; the normal GRUB boot default is unchanged.

The reusable runner subsequently passed its own ten-second RTC test and
association/address recovery check (`runner-check/result.json`). On the final
#26 boot, the PM counters recorded seven successful transitions and zero
failures; these include one device-debug stage and six real RTC sleeps. Display
off/on counters were 7/7 with state restored. The temporary SMC/IRQ measurement
modules were unloaded and the test inhibitor stopped after confirming the lid
was open. RTC/display/cpuidle support and both Wi-Fi sleep opt-ins remain active
on this running test kernel. `cleanup.json` retains the final state.
