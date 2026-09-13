# M5 CPU reuse audit and Linux SMP qualification

September 13, 2026. Workbench: `j813-linux`; branches: `j813`.

## Current hardware result

All ten cores (six E and four P) now run together with N1 Wi-Fi, native DCP,
XFCE, and Zink/Honeykrisp gears on the full diagnostic image. The user
confirmed that the physical screen shows rotating gears and the pointer moves.
The successful route initializes Linux on its boot core, then brings the
other nine online through the existing Linux hotplug state-control interface.
This is a qualified first-time startup route, not proof of CPU offlining,
suspend, low-power idle, dynamic frequency scaling, or a root fix for the
earlier concurrent-boot failures.

On the full desktop, ten concurrent CPU workers and 200 affinity visits passed;
a separate test passed 100,000 shared-memory handoffs among ten pinned
processes. Gears passed all eight tested animation/resize cases, from 192x192
through 889x719, with paired GPU queue retirement and changing pixels.
No kernel oops/lockup marker was found in the retained runtime log scan.
The live desktop is being preserved instead of rebooted for another
early-SMP comparison.

## Findings

The graphics-qualified kernel was deliberately booting one CPU. It has
`CONFIG_SMP=y` and `CONFIG_NR_CPUS=16`, but the Linux helper passed `maxcpus=1`
and never called `smp_start_secondaries()`. m1n1's `dt_set_cpus()` removes
unstarted CPU nodes, so deleting the command-line limit alone cannot enable SMP.

The earlier M5 fixes are already present in the current RAM carrier. The root
`m1n1` checkout and `hv/publish-j813` have identical `smp.c`, `cpufreq.c`,
`start.S`, `cpu_regs.h`, and `aic.c` at the initial audit point. The later
upstream CTRR reservation change described below is in `hv/publish-j813`.
Reuse includes:

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
was bypassed, so simpledrm alone is not established as the cause. Moving
secondary startup until after
all device preparation and uploads progressed farther, but the async
initramfs unpacker faulted on CPU 7 in `__pi_memcpy_generic`. The exception
was a level-3 write-permission fault at `ffff8000049a0000`, while the dumped
PTE `00e80100049a0707` has AP=0 (EL1 read/write). The Neo comparison below
provides a physical-protection explanation to investigate. The PTE mismatch
does not prove a TLB defect, and the old fault has not been causally tied to
this boot's protection bounds.
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

## Neo branch comparison and upstream reservation

The J700 native SMP workaround uses both `idle=nop` and `arm64.nowfxt`.
m1n1 commit `04d8f363` appends these for T8140; `9b363fed` enforces WFE
parking and `1ae740de` publishes secondary startup state to the point of
coherency. The M5 loader now exposes the same two Linux wait workarounds for
comparison. Neither the no-wait workaround nor a saved register file is proof
of low-power idle or suspend support.

Neo's later research (`neo-bringup` commit `cc9e9d6`) points to upstream
[m1n1 PR 657](https://github.com/AsahiLinux/m1n1/pull/657), merged August 29.
Commit `0d9a7d632cf0547edc31d244211492fb8653b422` reserves the M4+ CTRR region
that can be writable by the boot core but read-only on secondaries. It has
been cherry-picked to m1n1-aurora `j813` as `e14a1da7`. Use its CRm=1
register definitions; older Neo notes used CRm=0 aliases.

New RAM carrier: `hv/publish-j813/build-j813-smp-ro/m1n1.bin`, SHA256
`7b9d2528f95e588c4d8fd59f1430c590a412149f8c74b11c46693704650959c5`.
Its exported FDT was checked against live bounds: a `0xb000` raw reservation
at `0x100059a4000` covers the protected `0xc000` interval after Linux's 16 KiB
page rounding. The address changes across boots and must never be hardcoded.
The loader's explicit `--carrier-build` and `--carrier-sha256` options retain
the image hash, live code/symbol, and exported DCP/GPU mapping checks.

The full payload with this carrier, `idle=nop arm64.nowfxt`, and early
simpledrm disabled activated all ten CPUs, then faulted executing a data
address from the scheduler. The protected range was reserved in that boot.
This fix is therefore necessary upstream support, not a demonstrated cure
for the full M5 desktop failure. Evidence:
`logs/m5-linux-smp-upstream-dcp-resumed-20260913.log`.

The next CPU-only boot with the same new carrier and wait workarounds passed:

- 200 affinity visits over every ordered CPU pair, zero mismatches.
- Eight 64 MiB SHA256 jobs on each of ten pinned workers, all 80 digests
  correct (5 GiB processed).
- 100,000 shared-memory handoffs between ten pinned processes, verifying a
  256-byte payload after every acquire/release handoff.

Evidence: `logs/m5-smp-mini-neo-migration-corrected-20260913.log`,
`logs/m5-smp-mini-neo-concurrent-ready-20260913.log`, and
`logs/m5-smp-mini-coherence-20260913.log`. The first migration harness targeted
the parent shell's PID; its mismatches were harness errors. The corrected
test reads the actual subshell PID. The first repeated-hash attempt lacked
`/run` and did not execute; only the later ready log is qualification evidence.

`smp-coherence-smoke.c` is a small freestanding Linux/AArch64 test suitable
for the serial-only rescue initramfs. It also passed in the build VM before
hardware execution. It tests shared memory and core affinity; it does not
qualify every atomic instruction, CPU hotplug, or sustained thermal behavior.

A subsequent native m1n1 per-core CTRR probe timed out before returning a
complete row. No conclusion about per-core protection differences can be
drawn from that probe.

The separate `gpu-dcp-smp-diagnostic` image enables CPU hotplug, soft-lockup,
hung-task, workqueue, list, and spinlock diagnostics. Image SHA256:
`e3a2be14b1c46a9a5641667683f4de93117a3823235aa71ab5f035ff7160df2f`.
`--smp-boot-cpus=1` retains all ten DT nodes while deferring secondary Linux
startup until userspace. The full boot reached a shell and all nine remaining
cores were then onlined successfully. Spin-table lacks offlining operations,
so `cpuN/online` is absent even with HOTPLUG_CPU enabled. The existing
`cpuN/hotplug/target` interface supports first-time startup. Use
`online-m5-cpus.sh`, which derives the online target from the current kernel,
checks the board and topology, and refuses intermediate or downward changes.
Its already-online path also passed on the live M5.

Runtime evidence:

- `logs/m5-smp-diagnostic-deferred-20260913.log`: full kernel shell.
- `logs/m5-smp-diagnostic-cpu1-target-20260913.log` and
  `logs/m5-smp-diagnostic-online-all-20260913.log`: secondary startup.
- `logs/m5-smp-full-coherence-20260913.log`: 100,000 handoffs, PASS.
- `logs/m5-smp-full-desktop-restore-20260913.log`: DCP/GPU/N1/XFCE startup.
- `logs/m5-smp-full-gears-regression-20260913.log`: eight cases, PASS.
- `logs/m5-smp-full-desktop-cpu-smoke-ready-20260913.log`: ten workers and
  200 affinity visits while the desktop/gears were running, PASS.
- `artifacts/cpu/m5-ten-core-desktop-20260913.png`: verified X11 screenshot;
  the user's separate physical confirmation covers panel and pointer behavior.

The RAM desktop initially lacked `/dev/shm`, preventing Python's semaphore
creation before any workers started. Mounting tmpfs there fixed that
environment error; `start-m5-shadow-glx.sh` now performs that setup. The
unready Python run is not CPU-failure evidence.

Reproduction after a fresh scoped M5 reset and the new carrier chainload:

```sh
python boot-linux.py --gpu --gpu-initdata --gpu-display \
  --gpu-candidate=gpu-dcp-smp-diagnostic --smp --smp-boot-cpus=1 \
  --smp-idle=nop --smp-no-wfxt --smp-diagnostics --no-simpledrm \
  --carrier-build=/absolute/path/to/hv/publish-j813/build-j813-smp-ro \
  --carrier-sha256=7b9d2528f95e588c4d8fd59f1430c590a412149f8c74b11c46693704650959c5
# In the native RAM shell, run a transferred copy of online-m5-cpus.sh,
# then perform the existing DCP/GPU/N1/XFCE restoration and regression.
```

`gpu/build-m5-smp-candidate.sh` now reproduces this separate diagnostic build
configuration and output directory without replacing the original AGX3
recovery candidate. The large kernel/initramfs payloads remain workbench
artifacts. The successful deferred sequence differs from the earlier failed
image in both configuration and startup timing; no single-cause claim is made.

Neo's cpufreq proof is a guarded firmware P1/P2/P1 transition, not Linux
governor support. The latest compared Linux J700 tree has GPU OPPs but no CPU
OPP/performance-domain wiring. Reuse the CPM register identification and
rollback method, preserve inherited CLPC/throttle settings, and validate M5
transitions separately after SMP is stable. No CPU-frequency writes were
performed during this campaign.

The CPU-only payload uses the
existing 1 MB rescue initramfs instead of the 29 MB desktop initramfs; invoke it
with `--cpu --smp` (default WFI). Its Image is copied from the SMP candidate.
The reusable `smp-smoke.py` runs two seconds of pinned computation per available
CPU concurrently and visits every ordered CPU pair, retaining timer/IPI counters.
It does not qualify CPU hotplug, suspend, frequency scaling, or sustained load.
The helper passed ten workers and 200 affinity visits both in the build VM
and, subsequently, on the full M5 desktop, using a controller-generated
expected digest. The busybox and freestanding tests provide separate hardware
evidence with the smaller rescue environment.
