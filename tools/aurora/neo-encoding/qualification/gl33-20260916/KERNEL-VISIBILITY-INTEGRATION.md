# J700 kernel visibility candidate, 2026-09-15

## Source and build

Isolated checkout: `neo-encoder-linux-j700-visibility-20260915`.
Branch: `j700-gl33-visibility-20260915`.
Source commit: `c4478d9d130bec15dd62bc00301e78ccc5a7ab25`.

The candidate preserves the local sample-descriptor and fragment-stamp checks
and integrates eleven GPU commits from `672147175154` through `7296ee3c3169`.
The broad branch merge was aborted because it also pulled unrelated platform
history; the selected GPU commits were then applied. The timestamp conflict
keeps J700's grouped GID reservation while adopting host-side timestamp results.

Eight targeted host scripts pass, with the scheduler result recorded separately
after updating its mock mutex/pin-init and packet-construction APIs. Those three
modeled scheduler tests do not establish hardware parallel execution.

Build: `/home/mcruz.guest/neo/build/linux-j700-visibility-20260915` in `neo-builder`.
The prior source and build directories were copied and left unchanged.
All 77 Rust source-file hashes match the isolated checkout, with no extra files.
Build log: `kernel-visibility-build.log`, exit status zero.

Candidate: `candidates/asahi-kernel-visibility.ko`.
SHA256: `f8d887c8c95251b98d11a54831fbdad5d6a0b6c73a9e8e0793f45fe9c898d0d0`.
ELF build ID: `c4f4f52075e024517a367781e33ed26a00349c4e`.
Vermagic: `7.1.6-aurora-neo-gpu1 SMP preempt mod_unload aarch64`.

## Before/after experiment

Baseline run: `fast-iterations/kernel-visibility-before-20260915`.
The unchanged Mesa and stamp-gated kernel failed after 21/12,000 iterations:
nine comparisons, one mismatch, one unstable read, second read correct, no
GPU fault. Probe time 1.066 seconds, total 4.756 seconds; Xorg 762 reused.

The Neo's Wi-Fi stopped passing traffic while Linux remained responsive over
its exact USB console. Reconnecting its existing connection restored SSH.
No GPU or kernel change was needed for that recovery.

The owned Xorg took time to exit after termination; the staging guard correctly
refused while it still existed. After it exited, the candidate was staged with
`stage-asahi-kernel-visibility.py`. The previous module is retained at:
`/home/mcruz/gl30-cts-20260913/asahi-backup-6e8621eb0c4147778d243870907cffccdee5b9b74711516c451fdc9c88937cbe.ko`.

Boot evidence: `boot-kernel-visibility-ready-20260915/`.
The first boot-helper attempt caught USB re-enumeration and exited before any
upload. The second passed resident and bundle checks, confirmed J700/T8140,
and began the established RAM boot. The bundle's init script loads NVMe/Btrfs
and switches to the installed root; Asahi is loaded from the staged root module.
The resident boot image is unchanged.

Hardware result: **the readback defect remains**. Boot
`0117abac-6cb4-4194-9d9b-9d69f80e26e5` loaded the candidate; both its module
hash and loaded ELF build note were verified. DCP bound successfully and the
dedicated Xorg started as PID 750. The first display helper invocation rejected
an invalid evidence label before attempting the DCP probe; the corrected label
passed all guards. See `kernel-visibility-prepare.log`,
`kernel-visibility-display2.log`, and `kernel-visibility-x11.log`.

With baseline Mesa unchanged, the identical probe failed after 261/12,000
iterations: 249 comparisons, one mismatch, one unstable read, second read
correct, no GPU fault. Probe time 0.850 seconds, total 3.627 seconds. Evidence:
`fast-iterations/kernel-visibility-after-20260915/`. This is diagnostic failure
evidence, not CTS execution. The differing failure iteration does not establish
an improvement in this timing-sensitive race. No full CTS campaign was started.

A subsequent baseline run with Mesa integration `f8b2817802af` hit a render
timeout and left the GPU session crashed. Evidence:
`fast-iterations/kernel-visibility-final-mesa-baseline-20260915/`. The failed
VM bind was a consequence of the sticky `-110` timeout, not evidence that its
reported address was wrong. No CPU-read cache-command diagnostic was installed
or executed before this failure. Do not attribute the timeout to that experiment.
The prior kernel was restored on disk with the guarded helper after stopping
the owned Xorg (`kernel-visibility-restore-stage.log`); a fresh RAM boot is
required before further GPU work. This candidate is not qualified for promotion.

The fault log records RTKit firmware failure at PC `0xfffffc00000095f0`,
FAR `0xfffffc20018c0000`, ESR `0x96000007`, immediately after a retired mapping
reclamation message. Read-only disassembly using `probes/native_b1.py` and its
digest-pinned B1 image identifies the faulting instruction as `ldrb w9, [x0]`.
This is evidence of an unmapped firmware read. It does not yet identify the
allocation owner or prove that reclamation caused the fault. The raw devcoredump
was not collected before reboot; only the kernel crash summary is retained.
The test supervisor now collects bounded Asahi devcoredumps on future faults;
four host tests cover filtering, limits, read errors, and partial kernel reads.

Rollback boot completed as `fcc7cfaf-1f57-4d12-84a4-debecd791a6f`; loaded module
identity was verified before graphics startup. Normal Mesa was also restored
after the separate paired diagnostic, and the vendor smoke exited successfully
without a GPU fault. Current Xorg PID is 749. The resident remains unchanged.

## Preservation

Original `neo-encoder-linux` tracked diff remains byte-identical to
`kernel-before-visibility.patch`, SHA256
`c3ed2522ea3923f80cd73ff01089c018a89e109e74fad947ccc191d2fb430d93`.
Original `mesa-j700` tracked diff remains byte-identical to
`mesa-before-native-visibility.patch`, SHA256
`3e282558eab687b541fc23773c20f7c048962581e133ff6a673141ae2d93b60a`.
