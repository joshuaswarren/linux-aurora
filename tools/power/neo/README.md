# J700 suspend development

This branch has an experimental retained-power s2idle path with a successful
60-second PMU-alarm wake on J700. It requires the opt-ins and qualification
setup below; it is not yet a normal-boot or deep power-down implementation.
Keep a boot entry with the working kernel and initramfs when testing it.
`freeze` and `mem` select s2idle; their presence in sysfs alone is not evidence
of a working transition or a physical deep power state.

See `rtc-alarm.md` for the qualified timed wake setup, `screen-sleep.md` for
the retained display test and `sleep-power.md` for battery measurements.

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

Device tests execute real driver callbacks. Neither the PM test delay nor the
qualified RTC alarm can recover a hung callback. The RTC setup is experimental
and loaded explicitly in the development boot; see `rtc-alarm.md`.

## PMP and device lifecycle

PMP owns a running firmware epoch, shared DMA buffers and retained DART streams.
Its shutdown/restart or retained-state suspend contract is not implemented.
By default, reject system sleep in `prepare`, before devices begin suspending.
The opt-in retained s2idle path described below keeps its power domains, DMA
and mailbox live. Runtime and hibernation remain unsupported.

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
# In a separate session, retain this output on the controller:
stdbuf -o0 dmesg --follow

# Then activate from the control session:
echo 5 > /sys/module/cpuidle_apple/parameters/neo_cpu
echo Y > /sys/module/cpuidle_apple/parameters/neo
echo cpuidle-apple > /sys/bus/platform/drivers/cpuidle-apple/bind
```

This example registers only CPU 5, leaving the other CPUs on the architectural
fallback. `neo_cpu=-1` selects all CPUs. The selection applies at bind time;
unbind the driver before selecting another CPU and binding again.

The `neo` parameter gates binding; clearing it does not unregister an already
bound driver. Use each CPU's state-disable attributes to stop experiments.
State 0 uses the existing architectural state-preserving WFI sequence. The
experimental state 1 starts disabled on each CPU: after boot, write `0` to its
`cpuidle/state1/disable` attribute to qualify it individually. A separate
`cpuidle_apple.neo_cpu_pm=1` parameter includes CPU PM notifications around WFI;
leave it off when first testing the governor and state-save path. This separates
framework registration from the expanded CPU PM transition. The driver does not
use the older Apple driver's
locked CYC_OVRD register. Its latency/residency values are initial governor
estimates, not measured hardware guarantees or proof of power-down depth.

The `neo_cpu_pm` parameter can also be changed after boot. Disable the experimental
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
  SSH and showed a black screen. A second boot with the experimental WFI state
  and CPU PM callbacks disabled also failed. Both used generic POLL as state 0.
- The same kernel with cpuidle left unbound booted to SDDM. The early PMP veto
  was observed at `prepare`, and CPU 5 passed 500 integer/FPSIMD timer-sleep
  checks plus 20 stopped-task intervals each with the existing arm64 FPSIMD,
  streaming SVE, ZA and ZT register tests.
- Runtime binding limited to CPU 5 returned successfully, then connectivity
  was lost while only generic POLL was available. This reproduces the issue
  without enabling the WFI experiment or CPU PM callbacks. State 0 now uses
  the architectural WFI fallback; the qualification below passed. Use
  unbuffered log capture so short final messages do not remain in userspace
  buffers when a test stalls.

No successful full system sleep/resume cycle has been demonstrated.

## External recovery host

For this workstation setup, Neo is connected to **root@fedora-m1, Left-back**
(the user confirmed the port). Left-front has another Apple device: do not
send Neo reset commands there. Select the connector label, not an assumed I2C
bus number:

```sh
ssh root@fedora-m1 \
  /root/tuxvdmtool/target/release/tuxvdmtool --connector Left-back reboot serial
```

On 14 September this reset recovered Neo from the runtime polling experiment
to the normal Dirtyroom GPU kernel; SSH returned with a new boot ID. The VDM
command returning success alone is not proof that the target rebooted.

The recovery host has a physical UART at `/dev/ttySAC0` (115200 baud). Serial
capture is a separate qualification: the working Neo boot had no registered
serial console despite its `console=ttySAC0` argument. Its device tree exposes
`apple,t8140-uart` at `0x385200000`, for which the kernel has an early console
implementation. Keep console-routing experiments separate from proof of reset.

## Retained PMP qualification (experimental, 14 September 2026)

The all-CPU selector initially rejected `-1` through a signed/unsigned comparison;
that is fixed. Kernel build #15 passed all six CPUs with architectural WFI and
again with state 1 plus CPU PM notifications: 6,000 integer/FPSIMD timer-sleep
canaries, 48 vector test runs (960 stopped-task intervals), and two six-worker
timer/migration runs. Each vector test recorded positive usage of its selected
idle state (1,149 entries across the baseline tests, 1,091 across CPU PM tests).
These are CPU idle checks, not system suspend or physical residency measurements.

The PMP policy now has a reversible pause. Ten one-second awake retention tests
returned with the epoch running, DMA pinned, no protocol fault, policy unpaused,
and minimum frequency guards in place. The existing intermittent `-ENODATA`
thermal samples remain unresolved; successful firmware retention does not imply
that temperature admission has recovered.

With PM_DEBUG and PMP thermal enabled, the debugfs `retention_test_ms` write
accepts 1..1000 milliseconds for an awake pause/check/resume interval. It does not
suspend other devices. The opt-in system path requires BOTH controls:

```sh
echo Y > /sys/module/apple_dart/parameters/pmp_s2idle
echo Y > /sys/module/pmp_v2/parameters/retained_s2idle
```

The DART path requires the pinned live SID0 profile and the two T8140 PMGR
providers marked `apple,always-on`; it verifies DART/APF state before and after
without resetting the running firmware's mappings. PMP pauses policy and host
health work, while RTKit mailbox IRQ processing stays active. Its `complete`
callback restores policy after either resume or abort, requiring fresh thermal
admission. All controls default off. Runtime suspend, hibernation and non-PMP
inherited DART ownership are still rejected.

The first device-stage test returned `-EBUSY` in 0.897 seconds, but Wi-Fi did
not reconnect and SSH was lost. After external recovery, the saved result and
journal showed PMP prepare/suspend/complete success, its DART verification
passing, and NVMe shutdown/reinitialization followed by a durable result write.
The rejecting device was `390000000.iommu`, the radio DART. No full sleep took
place. The apparent hang over SSH was not a hung suspend transition.

The next candidate adds `apple_dart.inherited_s2idle=1` for the existing J700
radio/MTP profiles with no managed clocks/domains. It snapshots and checks all
stream roots, enables and protection registers rather than rewriting inherited
mappings. The radio additionally requires the BOOT parameter
`mt7932_fullmac.retained_s2idle=1`: its disconnected D0 firmware/DMA remain live,
with NO_SUSPEND interrupt service and transport geometry checks. This mode does
not save the radio's full power, implement firmware restart, or provide WoWLAN.

Test `devices` before `platform`. The latter includes late/noirq callbacks but
returns before the s2idle loop. Both can perform real hardware transitions.

The radio/DART candidate (kernel build #18) passed the complete `devices` stage
in 5.711 seconds. All three retained DART checks and the radio transport check
passed, NVMe resumed, and Wi-Fi automatically reconnected. The following
`platform` test, with all-CPU state 1/CPU PM enabled, left its durable result at
`running` and required external recovery; there is no saved callback failure.

A separate default-off `pmgr_pwrstate.retain_s2idle` switch rejects T8140 domain
power-off during system s2idle, leaving genpd's state ON. Driver callbacks still
execute and runtime PM outside the suspend window is unchanged. It isolates
power gating from device suspend while qualifying a conservative retained
platform. It is not evidence of a deeper physical power state.

## Earlier checkpoint: retained platform ready for a physical wake test

Build #19 (`9a1b478a5`, with the previously retained GPU module) completed three
`platform` tests with power domains retained. Two runs kept FPSIMD, streaming
SVE, ZA and ZT selftests active on each of the six CPUs across the transition:
all 48 process runs passed, and the PM resume-failure counters remained zero.
The Omarchy SDDM/Hyprland greeter was running during those two runs and rendered
correctly in a post-test screenshot. Wi-Fi reconnected automatically.

Observed domains retained in the headless test were `apcie_phy_sw`, `gfx`,
`i2c2`, `gpio` and `atc0_usb`. Keeping these powered is a conservative starting
point; it does not identify which individual domain caused the earlier stall.
System sleep power consumption, display power-down and sustained desktop
stability have not been measured or qualified.

`suspend-vectors.py --stage platform --tests DIR --output NEWDIR` repeats the
vector preservation test across a debug stage. `sleep-test.py --check-only
--output NEWDIR` checks the actual-sleep prerequisites without requesting sleep;
that check passed on build #19. Without `--check-only`, the latter tool requests
ONE real sleep, using the wakeup-count handshake and retaining per-CPU s2idle
entry deltas. A returned request without those entries is not proof of sleep.

At this checkpoint a physical operator was required for wake testing. Timed
wakeup was subsequently qualified as described in `rtc-alarm.md`. Fedora
Left-back remains the reset recovery path. The normal Dirtyroom GPU entry is
still the boot default.
During qualification, the transient `neo-suspend-qualification-inhibit` unit
blocks automatic logind lid/power/sleep actions; direct test requests still work.
Stop that unit when controlled testing is finished.

The first actual sleep/wake results are recorded in `rtc-alarm.md`.

`pm-test.py` and `sleep-test.py` compare `/sys/power/suspend_stats` failure
counters (`fail`, `failed_freeze`, `failed_prepare`, `failed_suspend*`,
`failed_resume*`) between the before and after snapshots. A write to
`/sys/power/state` returns success even when a device resume callback fails, so
a run whose counters increased is marked `status=error` with
`suspend_stats_error` naming the counter, delta and `last_failed_dev`,
`last_failed_errno`, `last_failed_step`; the deltas are saved as
`suspend_stats_check`. `validate-result.py RESULT_JSON...` reapplies that check
and the firmware health check to saved results offline (descending through
`sleep_result`), printing PASS/FAIL per file. `test-validate-result.py` is its
regression test against the `nvme-phy/platform` artifact, whose returned
platform run had `failed_resume` 0 -> 1 on `0000:01:00.0`.
