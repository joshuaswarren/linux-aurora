# J700 GL 3.3 iteration loop

Measured on the Neo on 2026-09-15, boot
`5a46d969-07c9-4cb1-b307-8adae3456a74`, normal Mesa
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`
and stamp-gated Asahi module
`6e8621eb0c4147778d243870907cffccdee5b9b74711516c451fdc9c88937cbe`.
The readback defect is still present. These changes accelerate diagnosis;
they do not increase conformance coverage or change the driver.

| Stage | Measured end-to-end time | Result |
| --- | ---: | --- |
| Fail-fast readback probe, budget 12,000 iterations | 3.77 s | Rejected after 93 iterations: one mismatch and one unstable read; second read correct |
| Official failure replay, six original sessions | 75.13 s | 55 executions: 27 Pass, 28 Fail, zero missing results |
| Earlier compressed wildcard group | 72.75 s | 570 Pass; this narrower configuration did not reproduce the race |

The replay covers the 54 failures and one interrupted case from the preserved
partial main campaign. This is 24 unique cases (23 previously failed and one
interrupted) across six sessions. Its 28 failures cover 14 unique packed-pixel
cases; the fragment-invocation query passed this time. An intermittent failure
is not cleared by a subsequent pass.

The earlier partial campaign took 5,479 seconds to reach its stopping point.
That is a different workload, not a controlled speedup benchmark. The complete
pinned official GL 3.3 campaign still requires 50 main executions of 9,886
cases each, or 494,300 main case executions, plus preliminary work.

## Use the shortest test that can reject the change

Run from this artifact directory. Labels must be new for each invocation;
the harness refuses to overwrite an earlier run. Add `--candidate` to the
first invocation when installing a new Mesa library.

```sh
python3 fast-iteration.py my-change-probe --profile gl33 --readback-probe
```

The probe defaults to 12,000 iterations. `LOOP_FAIL_FAST=1` stops only after an
observed mismatch or unstable read. Success requires all requested iterations,
the expected number of comparisons, no mismatches, no unstable reads, and no
GL error. The GL command sequence has no added synchronization. This is a
custom diagnostic, not an official CTS result. The original full-budget probe
behavior remains the default when its executable is run without that flag.

If the probe fails, investigate the defect before spending time on the next
stages. If it passes, replay the original official failures:

```sh
python3 fast-iteration.py my-change-replay --profile gl33 \
  --replay-failures partial-depth-storage-tess-full2-20260915
```

`--repeat 5` repeats every session five times without changing its settings or
seed. Every round is reported separately. The replay preserves framebuffer
dimensions, window/FBO type, EGL configuration, context type and base seed.
It omits preceding passing cases; order-dependent defects can therefore still
require the full original sequence. The unmodified official `glcts` binary
executes a generated list of exact case names. No CTS source, shader, expected
result, extension advertisement or driver behavior is changed.

After repeated probe/replay success, run affected feature groups and then the
complete official campaign. A focused pass must never be counted as completion
of the full campaign. Keep shader caching and tracing settings consistent when
comparing results; this harness continues to disable the shader cache.

## What the harness now saves

- It uploads and verifies the candidate once, retains the healthy Xorg, and
  supervises the entire replay under one hardware lock and console command.
- Identity, native renderer and binary checks run once before the replay;
  kernel fault monitoring continues across all its sessions. A GPU fault stops
  the replay before another session starts. GPU tests remain serialized.
- By default it collects QPAs, commands, receipts, kernel logs and source
  provenance, while leaving verbose `runner.log` on the Neo. Use
  `--collect-full-log` when that stdout is needed. `collection.json` records
  the remote evidence location. Nothing is deleted remotely.
- The replay's collected archive was 349,319 bytes. The earlier campaign's
  multi-gigabyte console log need not cross SSH on every iteration.
- A nonzero CTS exit now makes the host command fail even when transport and
  result collection completed. Replay analysis additionally rejects missing,
  duplicate, unfinished or unexpected cases, changed settings, missing rounds,
  and a formerly supported failing case becoming NotSupported.
- Probe receipts include the source digest, installed module digest, loaded
  module build note, Mesa digest, iteration budget and actual comparison counts.

Seven evidence-accounting tests passed, including interrupted/malformed QPAs,
changed seeds, duplicate/missing cases, unsupported results and nonzero exits.
The real hardware replay returned failure with 28 recorded failing cases; the
real hardware probe returned failure at its first observed corruption. Both
reused Xorg PID 762 and reported no GPU fault or kernel-log overrun.
The existing single-case path also passed `KHR-GL33.info.vendor` after the
supervisor changes. A 24-iteration probe smoke check executed all 24 iterations
and reported 12 successful comparisons, verifying the successful-budget path
only; it does not clear the race. Final checks confirmed no test process was
left running and both installed Mesa and kernel module digests were unchanged.

Evidence: `fast-iterations/speed-replay-baseline-20260915/` and
`fast-iterations/speed-readback-gate-20260915/`. A diagnostic's
`complete: true` means its execution and evidence collection finished;
`diagnostic_passed` and the actual results determine whether it passed.
