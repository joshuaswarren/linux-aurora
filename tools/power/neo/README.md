# J700 suspend development

This branch does not yet provide working system sleep. Keep a boot entry with
the working kernel and initramfs when testing it. `freeze` and `mem` currently
select s2idle; their presence in sysfs is not evidence of a working platform
transition or a physical deep power state.

## Diagnostics and wake input

Apply `arch/arm64/configs/j700-suspend.config` on top of the working J700 kernel
configuration. It enables PM debugging and the existing `macsmc-input` module.
The module exposes SMC lid and power-button events. Validate physical events
while awake before relying on them for wakeup. During this test, use a logind
inhibitor for `handle-lid-switch:handle-power-key` to prevent desktop actions.
An input event while awake does not establish the suspended wake route.

Run one PM stage at a time, starting with the freezer:

```sh
python3 tools/power/neo/pm-test.py --stage freezer --output /root/pm/freezer-01
python3 tools/power/neo/pm-test.py --stage devices --output /root/pm/devices-01
```

The helper writes provenance and a `running` record before the transition,
serializes device callbacks, retains kernel logs, and restores PM controls on
return. It makes exactly one unbuffered suspend request. Buffered Python text
I/O may retry a failed flush during cleanup, repeating a rejected transition.
`returned` means only that the requested test stage returned. Inspect the logs,
boot ID, thermal state, storage, networking and desktop separately.

Device tests execute real driver callbacks. The PM test delay cannot recover a
hung callback. J700 currently has no qualified RTC wake alarm; do not assume
`rtcwake` is an escape route.

## PMP and device lifecycle

PMP owns a running firmware epoch, shared DMA buffers and retained DART streams.
Its shutdown/restart or retained-state suspend contract is not implemented.
Reject system sleep in `prepare`, before devices begin suspending. Keep the
runtime and hibernation vetoes as well. Do not remove these checks just to let
the test advance.

A retained-PMP s2idle implementation must establish power/clock and DART
retention, mailbox processing, thermal admission and worker quiescence, then
validate firmware health and fresh samples on return. An orderly shutdown
implementation additionally needs DMA drain and restart ownership. Neither
contract follows merely from holding a runtime PM reference.

The MT7932 radio, inherited MTP mappings, root NVMe and graphics also need
individual suspend/resume qualification before a complete sleep cycle.

## Experimental CPU idle

The optional `cpuidle_apple.neo=1` kernel parameter registers `apple_neo_idle`
on J700. Without it, the working architectural idle fallback remains in use
and the platform device stays unbound. Prefer activation after userspace starts
so a separate SSH session can retain the kernel log:

```sh
echo Y > /sys/module/cpuidle_apple/parameters/neo
echo cpuidle-apple > /sys/bus/platform/drivers/cpuidle-apple/bind
```

The `neo` parameter gates binding; clearing it does not unregister an already
bound driver. Use each CPU's state-disable attributes to stop WFI experiments.
The driver offers POLL plus the existing state-preserving WFI sequence. WFI
starts disabled on each CPU: after boot, write `0` to that CPU's
`cpuidle/state1/disable` attribute to qualify it individually. A separate
`cpuidle_apple.neo_cpu_pm=1` parameter includes CPU PM notifications around WFI;
leave it off when first testing the governor and state-save path. This separates
framework registration from the expanded CPU PM transition. The driver does not
use the older Apple driver's
locked CYC_OVRD register. Its latency/residency values are initial governor
estimates, not measured hardware guarantees or proof of power-down depth.

The `neo_cpu_pm` parameter can also be changed after boot. Disable the WFI
states first to keep experiments separate. Each entry snapshots that setting
so its CPU PM entry and exit notifications remain paired.

Before enabling it routinely, check all six CPUs, integer and vector canaries,
timer wakeups, migration/IPIs, idle accounting and the desktop. CPU PM also
invokes timer, PMU and potentially KVM callbacks; preserving GPR/FPSIMD alone
does not qualify this expanded path. The s2idle callback still needs a complete
device suspend and physical wake test.

## Initial observations (14 September 2026)

- The diagnostic kernel booted to SDDM with the existing GPU module and Wi-Fi
  firmware retained. SMC input registered lid and power-button capabilities.
- Ten SMC module unload/reload cycles completed. Physical lid/button event
  confirmation is still pending.
- Three freezer tests returned, each after approximately 5.13 seconds.
- The first device-stage experiment hit PMP at `300e00000.coprocessor` with
  `-EBUSY`. The original buffered helper inadvertently requested two attempts;
  the helper now uses unbuffered sysfs writes.
- The PMP thermal zone was disabled after that experiment. Sampling errors
  were already present before it; this does not establish suspend as the sole
  cause. The independent SMC thermal zone and minimum CPU caps remained active.
- An initial boot with automatic J700 cpuidle registration did not return over
  SSH during observation. Its boot progress and failure cause are unconfirmed.
  Registration is now opt-in, pending isolation and hardware validation.

No successful full system sleep/resume cycle has been demonstrated.
