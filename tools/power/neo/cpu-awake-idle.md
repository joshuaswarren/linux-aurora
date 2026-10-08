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

A further busy sample with `neo_cpu_pm=N` also kept P0 ACTIVE in all
200,000 observations; the parameter was restored to Y. This excludes the
optional CPU PM notifications as a simple explanation for this boot.

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

## Next native comparison

`cpu-awake-config.py` uses the existing read-only client commands on either
OS to collect the same six core words, AIC configuration, SMC/NVMe IRQ_CFG,
and the two native CPM gating-enable words. It does not read IACK or issue
register writes. Native MMIO access requires the already installed DevMem
ABI 7 client. Example on macOS:

```
python3 cpu-awake-config.py --client /usr/local/bin/devmem-v12 --output NEW_DIRECTORY
```

Linux backend qualification (`config-linux/`) completed 64 rounds:
P0 ACTIVE in all 64; P1 OFF in all 64. AIC version/config were
0xa/0xe0000001, SMC RX IRQ584 and NVMe IRQ1039 configuration words both zero.
PMGR SOCPwrGate regmap 0x65 maps to 0x300724000 on this binary/ADT.
The native enableCPMPowerDomainGating method sets bit31 at offsets 0x60 and
0x68; Linux already reads 0x80000000 at both. No toggle was attempted.

Native AppleInterruptControllerV3 startup also preserves its configuration
and sets ENABLE. Its optional `aic-ext-intr-cfg` property supplies per-IRQ
low-byte overrides, but the retained J700 IORegistry has no such property.
The last-core PMP doorbell in generic ApplePMGR::_cpuIdle is gated on PMP
**v1**, so it is not evidence for a missing T8140 PMP v2 notification.
Do not copy this command based solely on finding it in generic code.

The saved macOS powermetrics capture reports P-cluster idle residency of
99.51% and 99.71%, E-cluster 97.35% and 96.51%, and CPU power 3 mW in each
5-second interval. Those are the native tool's reported estimates, not
whole-machine battery watts or a controlled comparison with this Linux boot.

## Matched native capture, macOS boot 819CE0F3

macOS 26.5.2 build 25F84, boot UUID
`819CE0F3-62A5-4DC7-A5B8-4B337D40C12A`. System sleep remained disabled.
The same `cpu-awake-config.py` completed 256 rounds in 11.610 seconds;
retain `neo-cpu-awake-819CE0-01/{metadata.json,reads.jsonl,result.json}`
under the artifact directory above.

| Physical core | OFF observations | ACTIVE observations |
| --- | ---: | ---: |
| E0 | 207 | 49 |
| E1 | 224 | 32 |
| E2 | 250 | 6 |
| E3 | 253 | 3 |
| P0 (boot core) | 113 | 143 |
| P1 | 147 | 109 |

OFF reads were 0x100 and ACTIVE reads 0x1f0 on both operating systems.
P0 therefore can reach OFF during native awake operation. Subprocess
creation perturbs scheduling; these counts are not residency percentages
and the sequential core reads do not establish simultaneous cluster OFF.
The AIC version/configuration, both IRQ_CFG words and both CPM gating words
were identical to Linux before and after the capture. The two CPM gating
enable bits are already set on Linux; writing them again is not a fix.

A separate, otherwise idle `powermetrics` capture reported E-cluster active
residency 6.28%/5.94%, P-cluster 0.81%/0.32% and CPU power 6/5 mW in two
five-second intervals. These are Apple's reported CPU estimates, not
whole-machine battery power. Full IODeviceTree, CPU/PMGR registry and
powermetrics outputs are retained in `native/*819CE0*`.

Sven Peter independently describes the same *register-state-loss*
asymmetry on newer Apple CPUs: the boot CPU retains state across WFI,
secondaries can lose state, and no explanatory Apple sysreg difference
was found. This is consistent with our physical-state observations but
does not establish a common cause or assign power to it:
https://lkml.iu.edu/2606.2/06457.html

Additional native checks exclude two tempting shortcuts:

- `AppleARMLightEmUp::start` (0xfffffe00089918b0) gates CPU halt setup on
  the `light-em-up` boot argument. It does not unconditionally clear a
  boot-core debug power request. The fresh CPU-debug device has no
  `light-em-up` property.
- The 25F84 iBoot reference to `arm-io/cpu-debug-interface` at
  0x1007fbf1010 reads `panic-trace-mode` and selects panic tracing; this
  reference is not an idle power-control operation. Retained iBoot SHA256:
  `7293fc9e1842dd83fe8a464890377dc309720540eeab74cb0fb05bd588fc57af`.

No macOS register writes or Linux power-saving change resulted from this
capture. Investigate the boot-core startup/retention contract next; avoid
guessing CYC_OVRD, AIC routing or CPU-domain writes.

## Whole-machine target and energy-counter comparison

The user clarified the awake target as **0.2 W whole-machine battery power**,
with the actual panel/backlight off and Wi-Fi/SSH reachable. Neither 0.7 W
nor 1 W is the completion threshold. CPU model estimates are diagnostic
evidence; only a controlled battery measurement establishes target power.

`cpu-energy.py` takes two read-only snapshots on either OS. The Linux
backend is `cpu-energy-read.c`; the native backend is existing DevMem ABI 7.
Neither backend enables, resets or calibrates accumulators. In particular,
stalled or reset Linux counters must not be reported as zero watts.

Native 25F84 implementation and live PMGR mappings establish:

- Core/SRAM: cpu-impl + 0x9000/0x9008, native functions
  0xfffffe000982f120 / 0xfffffe000982f2ec. Maps 14..17 are E0..3;
  maps 26..27 are P0..1. The table is at 0xfffffe0007630ab8.
- Cluster CPM/SRAM: map 10 (0x210e40000), map 22 (0x211e40000),
  offsets 0x8000/0x8008. These are distinct named model counters;
  do not assume their sum is independent rail power.
- `energyAccumulatorToNanojoules` at 0xfffffe000982ef20 multiplies
  by 2.5e-11 and then 1e9, using doubles at 0xfffffe00076309d0/9d8:
  **25 pJ per count**. Retained disassembly: `native/core-energy.asm`.

`neo-cpu-energy-819CE0-01` completed a native 120-second interval. P0's
core counter averaged 1.057 mW, its SRAM counter 0.105 mW; P1's core
counter averaged 1.457 mW. These are modeled per-component estimates,
not whole-machine watts, and do not prove the amount of static/leakage
power covered. All 16 counters increased. The matching fresh PMGR mapping
capture is `neo-pmgr-819CE0-energy` under the awake CPU artifact directory.

Linux #48 boot `57a0b2d4-7dc1-487d-900e-d7bb9c01a9f1` completed the same
120-second read-only measurement (`energy-linux-57a0b2d4-01`). All 16
counters increased, no accumulator configuration was written, and SSH
remained available. Both clusters were capped at their minimum frequencies
(744/816 MHz), multi-user target, qualified cpuidle with CPU PM enabled.

| Model counter | macOS mW | Linux mW |
| --- | ---: | ---: |
| P0 core | 1.057 | 70.221 |
| P0 SRAM | 0.105 | 4.256 |
| P1 core | 1.457 | 0.090 |
| P-cluster CPM | 0.588 | 43.671 |
| P-cluster CPM SRAM | 0.172 | 6.708 |

E-core counters were each below 0.07 mW on this Linux interval. The P0 and
P-cluster model readings strengthen the boot-core lead, but do not establish
all calibration coefficients match between operating systems or assign
whole-machine battery savings. These captures also have different observer
overhead and OS background work. Do not add the rows and claim measured
independent rail power, or claim solving P0 alone will achieve 200 mW.
No new whole-machine power reduction is established by these diagnostics.

## Linux per-core system-register capture

The opt-in `overlay/neo-cpu-regs.ko` takes read-only snapshots using pinned
process-context workers, never IPI callbacks. Its temporary die notifier
recognizes only the diagnostic's exact MRS sites and is removed before
module initialization returns. Unsupported reads can still emit an Oops
header and taint the kernel: require `kernel.panic_on_oops=0`. No MSR/MMIO
writes or automatic interpretation of read-as-zero registers are provided.

On boot 57a0b2d4, all nine selected registers read successfully on all six
cores; the UNDEF recovery path was **not exercised**. Module unload passed
and taint stayed 4096. Captures/binaries are in `registers-57a0b2d4`.

P0 and P1 matched exactly: CYC_OVRD=0, ACC_CFG=0xd,
ACC_OVRD=0x1800109000040212, MDSCR=0x1000, PMCR0=PMCR1=0,
DBGPRCR=0, OSLSR=8, AMX_CTL=0. E cores differ only in ACC_OVRD
(0x1800101000040212). Thus these registers do not identify a P0-only
debug power request, enabled legacy PMU, AMX context or WFI override.
This does not establish every PMU register's behavior on T8140.

The next control must match display state: previous Linux CPU sampling was
after panel restoration, while native display idle can already have blanked
its pipeline. Measure the CPU states and energy during qualified native
SET_POWER(false), together with whole-machine battery draw, before treating
the asymmetric CPU behavior as independent of display/firmware activity.


### Display-off control, boot 57a0b2d4

`display-control-57a0b2d4/measurement` completed a 120-second concurrent
battery/CPU-energy interval with native SET_POWER(false), LCD power GPIO
low both before and after, FAST Wi-Fi PS and both frequency caps unchanged.
The panel and HPD returned, gateway pings passed, and boot identity stayed
unchanged. The controller now optionally retains CPU energy and short busy
hardware-state captures outside the battery measurement interval.

P0 remained ACTIVE in both 200,000-sample snapshots. Its core model averaged
70.557 mW and P-cluster CPM 43.710 mW; these agree with the earlier panel-on
control. Thus native panel shutdown alone does not remove this asymmetry.
The battery's sampled power averaged **1.084681 W**, still far above 0.2 W.
Remaining-charge estimation is unreliable in this interval: charge_now
jumped upward during measurement, producing a misleading endpoint estimate
of 1.936 W. Retain both raw readings and report the sampled power with this
limitation; the raw charge_counter unit remains unqualified.

This is a diagnostic control, not a newly demonstrated power reduction.
The controller's post-test addition only checkpoints the pre-measurement
state earlier; the executed helper hash is retained in result.json.
