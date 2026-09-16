# Awake core power-state investigation

This investigates a running, Wi-Fi-accessible J700, not system suspend.
`cpu-idle-sample.c` only reads the six ADT-described CPU PMGR power-state
words and the AIC version/configuration registers already read by Linux.
It never writes either controller. Compile with:

```
aarch64-linux-gnu-gcc -O2 -Wall -Wextra -Werror cpu-idle-sample.c -o cpu-idle-sample
```

The default observer is Linux CPU1 (E0 on the tested boot). `--cpu 0..5`
selects another observer; retain MIDRs/topology to check the mapping on a
different boot. The default takes 2,000 sequential observations with
deterministic 7–13 ms jitter. `--busy` takes 200,000 without sleeping, as a
check against observer timer aliasing. The observer necessarily keeps its
own core active. These are sampled states, not hardware residency counters;
the six reads are not atomic and can miss short transitions.

## Kernel #48 evidence

Source parent `676d430ed`, boot `31dfb1fa-85ae-434e-acb0-b4a4bd7257d7`.
Captures and binary identities are retained in the parent workspace under
`artifacts/neo-suspend/cpu-awake-idle/`. Experimental cpuidle is bound on all
six CPUs with CPU PM notifications enabled. Both clusters remain at their
minimum requested frequency (P: 744 MHz, E: 816 MHz).

- Normal polling, E0 observer: P0 actual state ACTIVE in all 2,000 samples
  over 20.16 seconds. E1/E2/E3/P1 were OFF in >99% of observations.
- Busy E0 observer: P0 ACTIVE in all 200,000 samples over 0.65 seconds.
  `/proc/stat` counted all 66 CPU0 ticks in that interval as idle.
- Thermal polling temporarily changed from 25 ms to the existing guarded
  1-second policy: P0 still ACTIVE in all 2,000 samples. CPU0 state1 idle time
  increased by 20.138299 seconds over a 20.164518-second capture, with 1,129
  entries. E1/E2/E3/P1 were each OFF in 1,999 samples. Normal polling was
  restored, and both cluster maximum-frequency limits remained at minima.
- Busy observers on E1 and P1 also saw P0 ACTIVE in all 200,000 samples each.
  A CPU0 observer control saw the other cores mostly OFF, as expected.

This is evidence of an asymmetry between the boot performance core and
secondaries, not evidence that CPU0 is busy-spinning. It could prevent a
deeper P-cluster state, but no wattage has yet been assigned to it.

Prior macOS `macos-pmgr/idle-01` reads P0 OFF (0x100) and P1 ACTIVE;
`idle-02` reads P0 ACTIVE and P1 OFF. Thus native P0 can reach the reported
OFF state. These snapshots are not a matched native residency measurement.

Most device IRQs target Linux CPU0. The current AIC2/3 irqchip has no
affinity callback. Its version reads 0xa, configuration 0xe0000001: the
legacy PREFER_PCPU bit 28 is already clear. Do not use AICv1 target-register
semantics on this controller or assume flipping that bit fixes routing.
CPU hotplug is unavailable through the spin-table cpu_ops (no cpu_die).

## Native code checks

The retained T8140 kernelcache (macOS 26.5.2, 25F84) has a retention WFI
sequence at 0xfffffe000b38bc60: WFI, check whether LR became zero, return or
reconstruct idle exit. It does not program CYC_OVRD there. The adjacent
system-sleep WFI loop is a separate path and must not be copied into awake
idle. The older Apple deep-WFI CYC_OVRD writes remain inappropriate here.

Resolved authenticated T8140 PMGR vtable slots confirm:

- 0xf70/0xf78: cpuComplexIdleEnter/Exit.
- 0x1130: readACCReg. Their 0xe20020 operation polls bit 31 clear; it is
  a read, not a guessed power-state command.
- 0x10a0/0xdb8: getComplexToVoltageRail/enableSingingCapWidget.

The T8140 cpuCoreIdleEnter/Exit overrides are empty. The generic PMGR idle
path maintains core/cluster bookkeeping and updates performance state.
Next investigate boot-core initialization, native cluster policy, and
interrupt routing. A reduction in watts requires a controlled before/after
measurement with the real panel off and Wi-Fi associated.
