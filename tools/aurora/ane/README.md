# J700 ANE bring-up driver

The experimental firmware backend executes an ANEMLL tiny language model
and the project's convolution test on real Neo Linux. Applications can now
submit tensors to the trained 260032-parameter model through the private
`/dev/aurora-ane0` interface. A 256-job hardware qualification passed with
768 ordered events, repeated ring wrap, buffer-error recovery and two new
eight-token prompts. A second Linux boot passed automatic shutdown on the
last file close and an exact native output comparison. See [RUNTIME.md](RUNTIME.md)
for the application API, checks and limits. This remains a fixed-model
experiment requiring macOS compilation and a reset to release DMA/power.

The original power-only platform driver validates the cluster description
for the Neo's T8140 ANE and manages the existing Linux
`ANE_SYS` power domain. An explicit option adds manual child power and H17
CE initialization. **It cannot execute inference, matrix operations, or
DMA by itself.** A successful module load or power cycle is not ANE compute proof.

The driver is behind `CONFIG_APPLE_ANE_BRINGUP`, defaults off, and has no
enabled boot device-tree node. The test harness adds a temporary RAM overlay
only after identifying `apple,j700` and `apple,t8140`. Its deliberately
experimental compatible is `apple,t8140-ane-bringup`; it is not a finalized
upstream device-tree binding.

## Hardware-validated Linux firmware backend from native captures

### ANEMLL tiny language-model experiment

`prepare-anemll-tiny.py` exports ANEMLL's transformer modules with the trained
MIT-licensed Karpathy `stories260K` weights (260032 parameters, five layers,
512-token vocabulary). A stateless 32-position graph accepts FP16 token
embeddings and returns vocabulary logits. It executes the final MLP as well
as the output head; ANEMLL's cache-only prefill method omits that final MLP.
The native direct-ANE helper is based on ANEMLL's published benchmark API.
Public Core ML can select CPU for this tiny graph and is not ANE proof.

Native Neo ANE execution is proven for three successive prefixes. Tokens
432, 383, 286 extend "Once upon a time" to "Once upon a time, there was".
All active prefix positions match independent HF FP32 choices; maximum
absolute logit difference is 0.229. The same three steps now pass on real Neo
Linux 7.1.6-aurora-neo-gpu1: all 49152 FP16 logits match native ANE bytes,
all nine begin/profile/end events match their tokens, and program close and
firmware shutdown reply successfully. No new kernel diagnostics appeared.
See `neo-linux/artifacts/j700-anemll-20260913/RESULTS.md` and the machine-
checked `hardware-result.json`; `verify-anemll-linux-result.py` validates the
saved serial evidence, module/package hashes and tokenizer output.

`prepare-anemll-linux-case.py` validates two native program captures, a
complete 540672-byte mapped packed-weight extent, nine native events and the
independent numerical reference. It produces `j700-anemll-tiny.case` and a
build-pinned header. The `anemll-tiny` action uses CPU embedding lookup and
greedy selection around three ANE transformer/head runs. Each selected token
feeds the next prefix. Expected logits stay outside the ANE IOMMU domain;
output memory is poisoned before every job. Native ANE output bytes provide
the exact Linux comparison, after FP32 reference qualification.

This case adds one stage-3 event between begin/end; it is never a completion.
It opts into nine-event consumption with matching job tokens and validates
the three observed extra handles. The original convolution case retains its
six-event checks. `test-anemll-host.py` tests the actual kernel embedding and
integer-only binary16 argmax helpers against the three native outputs and
all 65536 binary16 bit patterns. `test-linux-work.py` tests both event forms.

This experiment still requires macOS compilation/capture of this particular
program. It is not an unmodified ANEMLL Python runtime on Linux, an arbitrary
model loader, or a Linux compiler. The installed resident is not modified.

### Original convolution qualification

`apple-ane-fw-test.c` is a separate, explicit RAM-test backend built by
`build-fw-test.py`. It implements a SHA-pinned 25G83 firmware loader, coherent
DMA buffers, a private Linux IOMMU domain spanning three DARTs, the native
boot record and channels, same-slot startup replies, and shared allocation
requests. An optional, separately pinned own-model case now implements
program loading, three numerical jobs, token-matched begin/end handling and
program-close/shutdown replies. All three Linux jobs are hardware proven:
49,152 exact outputs, matching completion for tokens 1–3, and successful
program-close/firmware-shutdown replies. The original
power-only driver's behavior is retained; its new exported hold lets the
ordered test overlays keep the engine powered before DART probe.

The actual C channel/DAPF checks pass ASAN/UBSAN on the controller and ARM64
Linux, including the saved native table and protected/failed-readback cases.
All three modules build against the matching kernel. Real Neo Linux now
completes firmware startup: all three DARTs, eight channels, four shared
allocations and a successful startup-command reply. No new diagnostics
appeared during the following sixteen minutes. Numerical execution is now
implemented as a bounded experiment. Captured-pattern, zero and one inputs
all pass under real Linux in `build-v9-handles` / `linux-ram-v9`.
See `neo-linux/artifacts/j700-ane-fw-20260912/RESULTS.md` for live evidence.
Once firmware starts the test retains mappings, power and module references
until a Neo reboot. This deliberate test lifetime is not a production unload
or suspend implementation.

Work-message word zero is an opaque reusable handle, observed in the range
0xfe00..0xfe03; stage at +0x1c determines begin/end. The third job reuses the
first job's handles. The consumer validates token, stage order, status,
length, mapping and ownership independently. The original bounded test
consumes six events. The optional tiny-model runtime separately qualifies
256 jobs and 768 events across repeated ring wraps; it does not make this
convolution path a general session API.

The numerical package is produced locally with `prepare-linux-case.py` from
the complete native setup and work captures. It checks captured FP16 CHW
input/output against the own-model scalar calculation, then creates pattern,
zero and one inputs with independent CPU references. Build with
`build-fw-test.py --case-dir <prepared-directory>` to opt in. The driver
requires the generated SHA-pinned case and the exact `conv16` sysfs action;
it does not accept arbitrary user commands or DMA pointers. Every output is
poisoned before each job. Reference bytes are never mapped for ANE access.
`test-linux-work.py` checks the actual C consumer's bounds, ownership and
event ordering under ASAN/UBSAN on both host and ARM64 Linux.

The native program-create command requests a 2 MiB EXPP pool (tag
`0x45585050`). The backend accepts that exact large request and retains a
1 MiB limit for other allocation forms, a total budget and IOVA overlap
checks. The first numerical candidate reached program creation but stopped
at its earlier allocation guard; its log is a failure, not computation proof.

The 2026-09-12 follow-up switches ANE bring-up to macOS hypervisor tracing.
Standalone power/scratch/firmware experiments below are historical recipes;
they are paused pending evidence from Apple's driver. The restored Neo uses
26.6.2 / 25G83, while `m1n1-firmware.py` intentionally still accepts only the
previously tested 25G76 firmware hash. Do not change that pin just to make a
new image load. The 25G83 firmware has the same validated segment layout but
different bytes; that is not proof of protocol or hardware compatibility.

The latest 25G83 RAM-only macOS guest now performs real ANE computation.
The HV's target-scoped native first-init DAPF implementation corrected 24
missing register fields; all readbacks passed without changing protection
locks. The previous SID-0 fill-region fault at `0x30070c008` stopped.
Nine workload intervals produced exact CPU-reference values and nine Apple
driver submissions, starts and completions, with no firmware timeouts or
pending requests. Firmware power-down succeeded. A program-unload error in
the first heavily traced interval and a timer diagnostic remain recorded;
this is not broad workload or complete teardown qualification.

The corresponding workspace evidence is
`neo-update/kernel/25G83/ram-ane-dapf-test/RESULT.md`. This is a working
macOS reference for Linux bring-up, not Linux ANE computation. The power
driver itself does not load firmware; the separate backend above now does.
The installed resident remains unchanged. SPTM-derived HV work is not an
author-isolated cleanroom implementation.

`decode-ipc.py` decodes captured channel tables and ring slots without opening
hardware. It validates every ring before returning a result: bounds, alignment,
overlapping allocations/table, duplicate names/sources and 64-bit overflow.
The three slot qwords are reported without inventing semantics for the third
word or flag bit 1. A valid decode is not an ANE computation result.

```sh
python3 tools/aurora/ane/decode-ipc.py captured-channel-table.bin \
  --channels 8 --iova 0x10000380000 --size 0x10000
python3 tools/aurora/ane/test-ipc.py
python3 tools/aurora/ane/test-firmware.py
```

For a paired captured boot record and channel table, the decoder can derive
IPC bounds and reject conflicting explicit addresses:

```sh
python3 tools/aurora/ane/decode-ipc.py channel-table.bin \
  --channels 8 --boot-args bootargs.bin
```

The 25G83 handshake capture has IPC base `0x100003d0000`, requested size
`0xc441`, command buffer `0x10000384000` / `0x40000`, heap
`0x100003e4000` / `0xf00000`, and 64-byte slots. Its eight channels retain
the earlier relative layout. The first boot-record qword is zero; its
meaning remains unresolved, and it must not be assumed to be the firmware
IOVA. Validation of these captured fields does not authorize constructing
or executing a new bootstrap from the older standalone recipe.

The example address/count are from the saved 25G76 handshake and must be
replaced with the corresponding captured values for a new run. The decoder
tests reconstruct only the eight channel metadata records printed by that
hardware run; they contain no proprietary firmware bytes. These are host
protocol/parser tests, including when run in the Linux builder. The new
firmware backend's separate real-hardware result is recorded above.

## Hardware basis

The subsequent IPC capture identifies same-slot synchronous IO replies,
firmware shared-allocation requests, and token-matched asynchronous work
events. `drivers/soc/apple/apple-ane-ipc.h` implements those transport helpers;
`test-ipc-transport.py` compiles their actual C code with ASAN/UBSAN on ARM64
Linux. Startup and asynchronous work helpers are wired into
`apple-ane-fw-test.c` and exercised by the three-job Linux test. The continuation record is
`neo-update/kernel/25G83/ANE_LINUX_IPC_HANDOFF.md`.

The J700 live ADT has `ane,t8132exclave`, `exclave-assigned`, and service
`com.apple.service.ANEExclave`. Translated `ane` reg[0] is
`0x400000000 + 0x2000000`. Its DART apertures lie within this cluster. Other
ANE register entries include shared PMGR and CPU ACC/CPM ranges, so mapping
all ADT entries as private ANE registers is unsafe.

PMGR's device 291 (`ANE-SYS-V`) is a virtual child of device 60 (`ANE_SYS`,
power-state register offset `0x290`). Device 338 (`ANE-SYS-V-PMP`) is a
separate PMP notification dependency. The Linux driver uses the existing
provider for device 60. It does not implement device 338 or the exclave
protocol. The optional direct child power sequence was qualified in m1n1.

The driver uses Linux genpd/runtime PM. An optional read-only PMGR diagnostic
uses the existing syscon mapping; it makes no additional register writes and
does not access the ANE cluster, DART, or CPU ACC/CPM windows.
The running boot tree has no phandle for the
unused ANE_SYS provider. The temporary `apple,power-domain-path` property
identifies that existing provider by its verified path; `of_genpd_add_device`
attaches without powering it on. The driver validates resources first. A
final binding should use a normal `power-domains` phandle once the engine
and its complete power dependencies are understood.

Loading with `enable_power_snapshot=1` enables the `power_state` attribute.
It reads ANE_SYS, ANE CPU and ANE subblock state registers at offsets `0x290`,
`0xc000`, and `0xc008` in the verified T8140 PMGR syscon. Reads do not wake the
ANE. The three observations are sequential. Without this module option,
the attribute returns `EOPNOTSUPP`.

`enable_engine_power=1` opts into CPU and subblock power sequencing at each
runtime resume, using the existing PMGR syscon and a separately validated
PWGATE range (`0x300726680 + 0x40`). The CE handshake uses only
`0x4018a4000 + 0x20`. Automatic child power management stays disabled.
Runtime suspend gates the subblocks, blocks CPU power requests, acknowledges
the gate and then gates the CPU before genpd disables ANE_SYS. The
`ce_initializations` attribute counts completed handshakes without reading
powered engine registers. Firmware and DART are not started by this option.

## Build

Use a complete build of the exact running kernel, including `Module.symvers`.
Run on the dedicated Neo ARM64 builder. Do not use the Air's VM or endpoint.
Confirm the ANE_SYS path in the running Neo's device tree; the test loader
revalidates its T8140 compatible, label and register offset.

```sh
python3 tools/aurora/ane/build.py \
  --source-root /path/to/linux-j700 \
  --kernel-build /path/to/exact-kernel-build \
  --power-domain-path /soc/power-management@300700000/power-controller@290 \
  --output /path/to/new-output-directory
```

The output contains two modules, five compiled test overlays, checksums and
the hardware test. No proprietary firmware or captured device identifiers
are included. The primary driver is self-authored against Linux interfaces;
reading existing public ANE implementations means this is not a claim of
author-isolated clean-room development.

## Hardware test

Transfer the build output to the verified Neo, then run as root:

```sh
python3 test-hardware.py --modules /tmp/ane-test \
  --output /tmp/ane-test-result.json --cycles 20
```

The test refuses a pre-existing ANE node, either preloaded test module, an
already active ANE_SYS domain, a different machine, mismatched kernel release/configuration,
or mismatched module checksums. It checks explicit opt-in, rejection of the
shared CPU register range and a missing domain, actual genpd on/off state,
runtime callbacks, unbind/rebind, full unload/reload, desktop process survival
and new kernel warnings/errors. Baseline kernel taint is retained in the
result. Process survival is not a visual desktop-frame test.

Add `--power-snapshot` to collect raw PMGR states while powered on and off.
The harness checks that ANE_SYS register state agrees with genpd and that
diagnostic reads introduce no extra runtime PM transitions. Child register
values are checked against their expected gated state. Add `--engine-power`
along with `--power-snapshot` to test actual child transitions and count one
completed CE handshake per resume. This separately opts into register writes.

`--keep-loaded` leaves a successful driver and test overlay present, with
ANE_SYS idle. Otherwise both modules are removed. Reboot also removes them;
the test changes no persistent boot image. To remove a retained test:

```sh
rmmod ane_test_overlay
rmmod apple_ane_bringup
```

## Next gate

Complete firmware command exchange and Linux IOMMU ownership, then execute
one small known operation, compare against a CPU reference,
and record completion evidence proving ANE execution without fallback.

`inspect-firmware.py` validates the segment layout and entry point of a
locally extracted, decompressed ARM64 `MH_PRELOAD` firmware image. It checks
file and memory bounds, overlap, executable entry, and supported command
structure. Its output is an inspection report, not a loadable Linux firmware
ABI or proof of authenticity, board compatibility, firmware startup, or ANE
execution. It does not inspect symbol tables or section contents. No firmware
bytes are included in this repository. Run its synthetic malformed-image
tests with `python3 tools/aurora/ane/test-firmware.py`.

[M1 Linux experiments](https://github.com/joshuaswarren/ane-linux-experiments)
provide useful runtime and numerical-test references. Their M1 initialization
ladder and task-register layout are not validated T8140 initialization code.

## Hardware qualification: 2026-09-12

The driver and harness built with `W=1` against the Neo's running
`7.1.6-aurora-neo-gpu1` kernel and exact configuration hash
`5a2435c69452b1b88a33816de58aa843e64ce09c6992b0e0ce08129fb40cad32`.
Kernel checkpatch reported zero errors and warnings for both C modules.

Two real J700 runs passed, totaling 40 normal on/off cycles. The final run
passed all 11 checks, including 20 matching resume/suspend callbacks, three
unbind/rebind cycles starting with the domain held on, and full module
unload while powered followed by reload. Both expected invalid descriptions
were rejected. The domain returned to `off-0` and runtime status `suspended`.
The test helper does not modify existing device-tree properties.

KWin and glxgears kept their process identities and advanced their CPU time.
The boot ID stayed unchanged. No new unexpected kernel warnings or errors
were found; kernel taint was already 4612 and remained 4612. This establishes
power-domain and lifecycle behavior, not ANE computation, firmware readiness,
desktop frame correctness, or a previously untainted kernel.

The final hardware runner SHA-256 was
`a8d02825eb1f3480bfff2e65faac2279c040ee683723e8bf339589058a89b08a`.
Private serial logs, build manifests and JSON results are retained separately
in the Neo workspace's `artifacts/j700-ane-driver-20260912` directory.

## Power-state diagnostics: 2026-09-12

The diagnostic revision passed two more real J700 runs: 12 checks with the
diagnostic disabled (three normal cycles), and 12 checks with it enabled
(20 normal cycles, 42 PMGR observations including reload). Both runs also
covered active unbind/rebind and unload/reload. Kernel taint stayed 4612,
there were no new unexpected kernel warnings, the boot ID stayed unchanged,
and KWin/glxgears retained their PIDs and advanced CPU time.

| State | ANE_SYS | ANE CPU | ANE subblocks |
| --- | --- | --- | --- |
| Off | `0x00002300` | `0x0f000300` | `0x00000300` |
| On | `0x000023ff` | `0x0f000300` | `0x00000300` |

Bits 3:0 are requested state; bits 7:4 are actual state. Thus the current
driver powers ANE_SYS while both engine child states remain power-gated.
This is measured evidence of a missing initialization step. It does not
establish that writing those two child registers alone is sufficient.

Target-specific static analysis maps ADT `ane-type=0x200` to H17 generation
`0xe0` and `AppleT8140ANEHAL`. The direct power path includes additional
PWGATE controls and a shutdown handshake; Linux still needs these and the
firmware/DART startup sequence. The J700 restore manifest selects
`h17_ane_fw_theia_d9x.im4p`. Its extracted image passes the new validator;
12 synthetic parser tests also pass. These source investigations do not
claim author isolation from Apple's implementation.

Build logs, firmware layout metadata, private analysis, and the hardware
console capture are in `artifacts/j700-ane-engine-20260912` of the Neo
workspace. The diagnostic modules remain loaded and suspended after testing.

## Direct H17 firmware bootstrap in m1n1

`m1n1-power.py` is a bounded, target-guarded experiment for J700/T8140 only.
It refuses a different chip, model, ANE ADT layout or serial endpoint. It
preserves resident Stage 1 and uses temporary RAM for firmware and tables.
The firmware is supplied locally and must match the J700-selected 25G76 H17
payload hash; no proprietary bytes are included here.

```sh
python3 tools/aurora/ane/m1n1-power.py --m1n1-root /path/to/m1n1-j700 \
  --cycle --init-ce --firmware /private/path/h17_ane_fw_theia_d9x.bin \
  --handshake
```

Observed on the real Neo: manual CPU/subblock power, CE initialization,
firmware CPU first wake, boot-argument acceptance, publication of eight IPC
channels, and full host handshake after initialization of the host-owned
rings. No DART fault was reported. Firmware startup is not neural-network
execution. `--command` additionally attempts the native opcode-4 startup
request; its response and final teardown still require qualification.

The CE handshake is essential: scratch-register writes before it caused
SError even without DART changes. RAM is uploaded before enabling the engine.
Stream 0 must be unused and unlocked; the experiment installs private 16 KiB
translation tables without resetting other DART streams. After a full
handshake, clearing CPU RUN did not make CPU_STATUS idle. The updated
teardown keeps mappings until the child power domains are verified gated.
If its watchdog aborts, allocations and hardware state are unknown; reset
the verified Neo before retrying. A normal controller reboot can enter
another boot choice, so confirm m1n1 again rather than assuming the proxy.

Logs and private source-analysis artifacts for this stage are retained in
the Neo workspace under `artifacts/j700-ane-power-20260912`.
