# J700 system-sleep device audit

Observed on kernel build #19, boot `eee6042d-cc10-4c2d-8cf4-66e01a884832`,
with the local `neo-display-sleep` qualification module. Evidence is the
saved `display-sleep-10-01/result.json` (before/after kernel logs) under
`artifacts/neo-suspend/display/`. This is an audit of the current retained
s2idle path, not a list of measured component wattages.

See `component-power.md` for the subsequent SMC telemetry, noirq interrupt
measurements, native DCP/PMP/Wi-Fi analysis, GPU idle-policy distinction and
negative PCIe L1.2 power experiment. Wi-Fi is now a measured recurring
interrupt source; none of the four components yet has isolated wattage.

| Component | Current behavior | Remaining work |
| --- | --- | --- |
| Backlight and LCD enable | SMC `pmBL=0`, then `gP16=0`; reverse sequence on resume | Repeat long sleep measurements; integrate the qualified controls with the eventual panel driver |
| DCP/display engine | Inherited framebuffer/firmware state remains; the new hook only controls the physical panel and backlight | Qualify DCP firmware sleep/wake and its DMA/power dependencies before stopping the display engine |
| NVMe/ANS | Existing driver shuts down NVMe/RTKit, stops the coprocessor, and resets/restarts on resume; native firmware reports saved context | Verify physical power-domain residency and repeated storage integrity; do not replace a working shutdown path with retention |
| CPUs | All six use state-preserving WFI with CPU PM callbacks; s2idle freezes timer timekeeping | Establish actual core/cluster residency and deeper platform transitions; the state name is not proof of power-down |
| Wi-Fi MT7932 | Disconnects and drains host activity, but deliberately retains PCI D0, firmware, DMA and MSI; IRQ uses `IRQF_NO_SUSPEND` | Implement firmware sleep/wake or a complete restart contract before allowing D3 or removing power |
| PMP | Host policy paused, firmware/DMA/mailbox and DART retained and checked | Native quiescence/resume protocol before stopping its firmware or changing shared power |
| GPU | `gfx` domain explicitly retained; current platform driver has no system-sleep shutdown/resume hooks | Stop submissions and retire work, coordinate both firmware roles, preserve/rebuild mappings and contexts, then qualify domain transitions |
| USB and PCIe domains | `atc0_usb` and `apcie_phy_sw` explicitly retained | Establish attached-device/wake dependencies and ownership before removing retention |
| GPIO and I2C | `gpio` and `i2c2` explicitly retained | Verify wake and resume dependencies before individual gating tests |
| DRAM, SMC and RTC | Required state/wake infrastructure remains available | Memory must retain contents; retain a working wake island while qualifying deeper SoC sleep |

The noirq log explicitly reports five retained domains: `apcie_phy_sw`,
`gfx`, `i2c2`, `gpio`, and `atc0_usb`. Other active domains observed while
awake must not automatically be described as active throughout sleep.

Relevant source:

* `drivers/nvme/host/apple.c`: `apple_nvme_suspend()` / `apple_nvme_resume()`.
* `drivers/net/wireless/mediatek/mt7932/mt7932.c`: `mt_pm_prepare()`,
  `mt_pm_suspend()` / `mt_pm_resume()`.
* `drivers/soc/apple/pmp-v2.c`: retained prepare/suspend/complete sequence.
* `drivers/pmdomain/apple/pmgr-pwrstate.c`: `retain_s2idle` power-off veto.
* `drivers/cpuidle/cpuidle-apple.c`: `apple_neo_idle_driver` and
  `apple_neo_enter_wfi()`.

Continue with one device or dependency group at a time. Require returned
callbacks, preserved data/context, functional work after wake, and measured
battery reduction before removing its retention safeguard. Disabling every
domain together would lose the evidence needed to locate a failed wake.

Future `pm-test.py` snapshots also retain `/proc/interrupts`, `/proc/softirqs`,
`/proc/stat`, and display callback counters at the endpoints. These help
identify IRQ activity and CPU accounting without a sampling timer during
sleep. The initial 10-second capture predates those additional fields; its
timekeeping log shows repeated short returns to s2idle but does not identify
their interrupt source. Do not infer the source from the interval alone.
