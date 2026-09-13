# M5 CPU reuse audit and Linux SMP qualification

September 13, 2026. Workbench: `j813-linux`; branches: `j813`.

## Findings

The graphics-qualified kernel was deliberately booting one CPU. It has
`CONFIG_SMP=y` and `CONFIG_NR_CPUS=16`, but the Linux helper passed `maxcpus=1`
and never called `smp_start_secondaries()`. m1n1's `dt_set_cpus()` removes
unstarted CPU nodes, so deleting the command-line limit alone cannot enable SMP.

The earlier M5 fixes are already present in the current RAM carrier. The root
`m1n1` checkout and `hv/publish-j813` have identical `smp.c`, `cpufreq.c`,
`start.S`, `cpu_regs.h`, and `aic.c` at the audit point. Reuse includes:

- Per-core reset stacks and MPIDR-based CPU identification.
- Publishing the RAM image and locked-reset-vector relay to the point of
  coherency before waking cores in another cluster.
- M5 PMGR/core-release sequencing and WFE parking.
- Six E-core and four P-core topology, with physical CPU 6 as the boot core.

The new `boot-linux.py --smp` path starts the secondary cores before preparing
the Linux device tree and omits `maxcpus=1`. It reports failed cores explicitly
and rejects an already-started secondary or a boot with no working secondaries.
Use it only after a fresh M5 reset and the guarded RAM carrier chainload. The
default single-core boot remains available for recovery.

## What the Windows work contributes

`AuroraSilicon/drivers/AicHal` documents the distinction between logical CPU
numbers and MPIDR hardware identities. Its Windows local-unit fix must not be
copied literally into Linux: Linux's AIC fast-IPI path already uses
`cpu_logical_map()` and MPIDR cluster/core fields. The Linux M5 AIC3 entry also
already carries the read-only virtual-timer-FIQ workaround.

`AuroraSilicon/drivers/AppleCpuFreq` provides a portable register contract, whose
normal and sanitizer host tests pass. Its KMDF hardware integration is a stub;
the set-frequency entry point returns `STATUS_NOT_IMPLEMENTED`. This is useful
reference code, not evidence of working Windows or Linux dynamic frequency
control.

The current m1n1 CPU-frequency code contains measured M5 cluster bases and ADT
operating-point information. Command registers at `0x210e20020` and
`0x211e20020` read `0x400102` and `0x40010a` again in this test. These identify
firmware command states; they are not a measurement of effective frequency.
The source maps those states to E 1152 MHz and P 3720 MHz. Linux CPU_FREQ is
still disabled and no cpufreq policies exist in the baseline.

Linux frequency work needs M5 cluster/OPP device-tree wiring, a supported
register encoding and status readback, and validation of transitions and
thermal behavior. The old M1/M4 throttling feature tables cannot be assumed to
describe M5 CLPC/PMP hardware.

### Retained macOS/HV reference

The existing PMGR journal
`hv/logs/pmgr-traces/m5-1788956883587715000.jsonl` contains 43 guest writes
to the P-cluster command register: 22 of `0x2400109` and 21 of `0x2400102`.
Every command with a subsequent register read (42 of 43; the final command
has no following read in this capture) reads back the requested state with
bit 25 cleared. This agrees with m1n1's M5 encoding: state in bits 4:0 and
the request strobe in bit 25. The capture also contains adjacent PMP/PTD
accesses; it does not establish that arbitrary isolated frequency changes
are safe or that idle, thermal control, and both clusters are qualified.
An earlier journal, `m5-1788954433968613000.jsonl`, has 13 matching writes.
No E-cluster command writes were found in the eight retained PMGR journals.

These traces provide a concrete reference before another macOS boot. A new
capture should target secondary-core initialization and idle/wakeup state,
with narrow tracing to limit timing changes. Native Linux remains necessary
for validation because the hypervisor traps WFI and changes timer delivery.

## Qualification evidence

The first fresh RAM boot started all nine secondary cores in m1n1:

```
prepared 0x3bf started 0x3bf failed 0x0
BOOT_CPU=6 SECONDARIES=[0, 1, 2, 3, 4, 5, 7, 8, 9] MISSING=[]
```

Controller evidence: `logs/m5-smp-carrier-20260913.log` and
`logs/m5-linux-smp-first-20260913.log`. The same AGX3-qualified kernel and
initramfs are used; this experiment changes the SMP handoff only.

Linux activated all ten CPUs in the first boot, then faulted on secondary idle
with PC/LR and all general registers cleared. With `idle=yield`, the next boot
got past idle but faulted after WFIT in `__delay()` on a secondary, again with
cleared registers. These match the earlier Windows/m1n1 wait-state failure.

The candidate now preserves x18 and the callee-saved general registers around
WFI and WFIT on the measured M5 CPU models. The compiled wait wrappers and
complete kernel build pass. Candidate: `gpu-dcp-smp-candidate`, Image SHA256
`770641203a66b62b119cf447b7ff0e396ddc83e7e30747f7602d9a60f6c73089`.
Use `--smp --smp-idle=wfi` to qualify normal interrupt waiting on that candidate.
The temporary yield option is diagnostic and does not provide low-power idle.

The full display boot with protected WFI/WFIT stopped progressing at simpledrm
initialization. The small CPU-only payload with the same patched kernel and
`idle=yield` reached a responsive shell with CPUs `0-9` online. All ten cores
completed concurrent, pinned 64 MiB SHA256 jobs with the expected digest and
their actual CPU IDs verified. Architectural timer and function-call/reschedule
IPI counters are present on all ten CPUs. Evidence:
`logs/m5-cpu-mini-yield-20260913.log`, `logs/m5-smp-mini-live-20260913.log`, and
`logs/m5-smp-mini-work-20260913.log`.

The matched CPU-only test with `idle=wfi` also reached a responsive shell and
passed all ten concurrent pinned 64 MiB SHA256 jobs. Timer counters and
function-call IPIs advance on all CPUs. Evidence:
`logs/m5-cpu-mini-wfi-20260913.log` and
`logs/m5-smp-mini-wfi-work-20260913.log`. No timed wakeup workaround was needed
for that test; the Windows hypervisor's virtual-interrupt wakeup workaround
must not be assumed to apply to native Linux.

The combined boot also stalled at cacheinfo initialization when simpledrm
was bypassed, so simpledrm alone is not established as the cause. That
unsuccessful bypass option was removed. Moving secondary startup until after
all device preparation and uploads progressed farther, but the async
initramfs unpacker faulted on CPU 7 in `__pi_memcpy_generic`. The exception
was a level-3 write-permission fault at `ffff8000049a0000`, while the dumped
PTE `00e80100049a0707` has AP=0 (EL1 read/write). This mismatch merits checking
translation/state synchronization; it does not yet prove a TLB defect.
Evidence: `logs/m5-linux-smp-dcp-20260913.log` and
`logs/m5-linux-smp-late-20260913.log`.

The corresponding full-image `idle=yield` comparison again stopped at
simpledrm initialization (`logs/m5-linux-smp-full-yield-20260913.log`). This
rules out disabling idle WFI alone as a sufficient fix; the delay path still
uses protected WFIT. No full desktop SMP qualification is claimed. The
previously qualified single-core AGX3 candidate remains the recovery image.

The current macOS `hv/bootstrap-m5.py` explicitly requires `cpus=1`. Its
retained PMGR captures therefore cannot establish secondary-core startup
behavior. A multicore reference capture needs that hypervisor path qualified
first; removing the guard alone is not qualification. A native Linux trace
of the stuck CPUs is the direct next step for the combined-boot failure.

Graphics regression remains pending. The CPU-only payload uses the
existing 1 MB rescue initramfs instead of the 29 MB desktop initramfs; invoke it
with `--cpu --smp` (default WFI). Its Image is copied from the SMP candidate.
The reusable `smp-smoke.py` runs two seconds of pinned computation per available
CPU concurrently and visits every ordered CPU pair, retaining timer/IPI counters.
It does not qualify CPU hotplug, suspend, frequency scaling, or sustained load.
The helper passed ten workers and 200 affinity visits in the build VM, using
a controller-generated expected digest. This validates the helper only; the
M5 hardware result above comes from the separate busybox pinned-work test.
