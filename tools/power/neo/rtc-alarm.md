# Daily-full integration (2026-09-28)

The driver is now a platform child of the existing Baku PMIC NVMEM provider.
It shares that provider's regmap and keeps the reboot/RTC offset cells available.
The normal DT contains the child. Build CONFIG_RTC_DRV_APPLE_PMU=m and load
`modprobe rtc-apple-pmu experimental=1` only for retained-s2idle qualification.
Without experimental=1 probe returns ENODEV before touching hardware.
Do not load the historical neo-rtc-overlay: it creates a second device on
SPMI slave 14, already owned by apple-spmi-nvmem in the daily tree.
The port has only host object checks; repeat the awake alarm, cancellation,
rearm, and timed-s2idle tests below on the new kernel.

---

# J700 PMU alarm qualification

The SMC RTC implementation has no alarm operations. J700 instead describes a
`pmu,baku` on `/arm-io/nub-spmi0`, with an RTC alarm. The bus port translates
to physical `0x308714000`; ADT slave ID is 14, controller generation 3,
AIC interrupt 487, peripheral interrupt 2. The existing Apple SPMI FIFO
layout reads this port successfully. Do not conflate it with the HPM bus
at `0x308908000`.

Static reference: Darwin 25.5.0 / XNU 12377.121.10~1, RELEASE_ARM64_T8140,
kernelcache SHA256
`c4de7d65a016ff68f6ac2a335517da82c9be966175c80afd8a738dd00faa1c61`.
`AppleDialogSPMIPMURTC::programRTCAlarm(unsigned int)` at
`fffffe000968da44` clears pending alarm bit 0 at `0xf80c`, writes a 32-bit
little-endian absolute up-counter seconds value at `0xf808`, unmasks bit 0
at `0xf80e`, then writes monitor bit 0 plus enable bit 6 to `0xf800`.
Disabling masks bit 0 and leaves the monitor enabled. Alarm event is W1C.
The interrupt action at `fffffe0009682de4` calls `programRTCAlarm(0)`.

The 6-byte counter at `0xf802` is little-endian, shifted right once to get
32768-Hz ticks. Legacy scratchpad offset uses seconds at `0xf704` and a
15-bit fraction at `0xf715`, combined and sign-extended from 47 bits.
`_readCurrentOffsetTicks` at `fffffe000968ce3c` describes this layout.
Keep these offset registers read-only during qualification.

Awake reads on kernel #19 (`9a1b478a5` Image, boot ID
`c96d75a7-fc5d-41d5-8a2e-fd0fd584e962`) succeeded, with empty FIFO status
`01000100` and valid response command/slave/parity fields. At one sample,
counter seconds `19410037.393280` plus offset seconds `0x697fdfa1` and
fraction `0x28ec / 32768` matched the current Unix time. Alarm control was
`01`, event `00`, mask `00`: alarm disabled, event unmasked.

`pmu-read.c` reproduces the bounded reads via `/dev/mem` and refuses a
controller already claimed in `/proc/iomem`. `--status` only reads status;
`--rtc` sends SPMI READ commands, without writing PMU registers. Build with
`aarch64-linux-gnu-gcc -O2 -Wall -Wextra -Werror -static pmu-read.c -o pmu-read`.
Do not run it concurrently with binding or using a kernel SPMI driver.

Full local evidence, ADT values, disassembly and decompilation are retained
under workspace `artifacts/neo-suspend/rtc-alarm/`. The alarm interrupt and
suspended wake route remain unqualified at this checkpoint.

## Experimental Linux driver

`CONFIG_RTC_DRV_APPLE_PMU=m` builds `rtc-apple-pmu.ko`. The module requires
`experimental=1` and a J700-specific device node. It preserves the hardware
time and its scratchpad offset, supports alarms within one hour, reads back
the comparator, and handles only retained-power s2idle. Its NO_SUSPEND
threaded interrupt explicitly raises a system wake event after identifying
the PMU alarm. This does not establish wake with SPMI/AIC powered off.

The first awake 5-second `rtcwake -m on` test passed: peripheral IRQ 2 was
delivered and the PMU event was acknowledged/disarmed. The controller uses
AIC 487 (Linux IRQ 72), RTC Linux IRQ 73. Device time agrees with system UTC.
Registering `rtc0` invokes the RTC core's HCTOSYS path, setting system time
from the hardware clock; this may cause a subsecond journal time adjustment.

For runtime qualification, build `overlay/` as an external kernel module.
Replace `AIC_PHANDLE` in `overlay/rtc.dts.in` with the **current live** AIC
phandle and compile with `dtc -I dts -O dtb` (without `-@`: the live J700
tree has no `__symbols__`). Place the DTBO in `/lib/firmware`, load the RTC
driver, then explicitly load `neo-rtc-overlay.ko firmware=NAME.dtbo`.
No bootloader image or normal-boot configuration is changed. The overlay
borrows the inherited powered bus; use only the retained development boot.
Do not run `pmu-read` after the controller has bound. Disarm before unloading
the overlay or RTC module. A reboot discards these runtime additions.

## First timed sleep result

After delivery, cancellation and rearming tests passed while awake, the full
platform debug stage returned in 5.87 seconds with the RTC driver loaded.
The first actual sleep used:

```
python3 sleep-test.py --wake-after 60 --output NEW_DIRECTORY
```

It returned in 60.694 seconds of CLOCK_BOOTTIME and 0.734 seconds of
CLOCK_MONOTONIC: about 59.960 seconds with timekeeping suspended. The boot ID
was unchanged. All six CPUs recorded s2idle entries, the PMU alarm fired,
its wakeup count advanced, and all suspend/resume failure counters stayed zero.
Wi-Fi reconnected and the PMP firmware epoch remained running and pinned.
No physical button press or external reset was used.

This is retained-power suspend-to-idle, not complete platform power-down.
PMP/radio firmware and unqualified power domains remain on. CPU 0/4 woke
intermittently for retained interrupts during the interval. Display power-down
is not implemented. No battery-life improvement is established by these tests.
The existing PMP thermal producer still reports intermittent/stale ENODATA;
minimum frequency caps remain enforced. SMC sensors read about 37–41 C after
the first cycle; the PMP's old 73 C sample is not a fresh temperature reading.

A second 60-second cycle returned in 60.678 seconds (0.802 seconds monotonic),
again through the PMU alarm with the same boot ID and no PM failure counters.
All 24 vector processes (FPSIMD, streaming SVE, ZA and ZT on all six CPUs)
reported success after spanning this transition. Use `suspend-vectors.py
--stage s2idle` with the existing `--tests` and `--output` arguments to repeat
that experiment. The login compositor rendered a fresh screenshot afterward;
Wi-Fi reconnected and root NVMe resumed. These are two successful retained
sleep cycles, not qualification of arbitrary applications or long-duration sleep.

The RTC driver and overlay are loaded in the current development boot only.
The existing qualification inhibitor continues blocking automatic logind
lid/power/sleep actions while these opt-ins are armed; the direct test helper
still works. No alarm is left armed after a completed test. The normal GRUB
default, its kernel/initramfs and stage-2 image remain unchanged.
