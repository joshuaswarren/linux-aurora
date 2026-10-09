# Native CTS iteration without a reboot

Keep the qualified dedicated Xorg alive while the GPU remains healthy. Each
invocation starts a fresh CTS process, so an idle private Mesa library can be
replaced atomically without restarting Xorg, probing DCP or rebuilding KDE.
The Xorg process uses its separate qualified runtime. GPU faults still require
a fresh qualified RAM boot; an observation timeout never permits a retry.

From the workspace root:

```sh
python3 neo-linux/artifacts/j700-gl30-cts-20260913/fast-iteration.py UNIQUE-LABEL \
  --case KHR-GL30.draw_buffers.draw_buffers_1 \
  --candidate /absolute/path/libgallium-CANDIDATE.so

python3 neo-linux/artifacts/j700-gl30-cts-20260913/fast-iteration.py UNIQUE-FULL-LABEL --full
```

Labels and candidate filenames must use lowercase letters, digits and hyphens.
Use `--take-desktop` only on the initial transition from the qualified test
desktop. Its existing helper preserves unexpected user application windows.
Omit `--candidate` to reuse the installed library. Add `--trace` to an exact
diagnostic case only when shader/command dumps are needed. Full mode always
calls the unmodified official `cts-runner --type=gl30` without filtering.

To reproduce a failing configuration, save its entry from `session-ledger.json`
and pass `--session /absolute/path/session.json` alongside `--case`. The
diagnostic preserves the recorded dimensions, surface type, configuration and
seed, replacing only the full case list with that case. It rejects added case
or log overrides and duplicate options. Full mode cannot accept `--session`.
`max-height-session.json` reproduces the official 64 x maximum-height FBO
configuration, seed3, config1. Diagnostic repetitions remain separate from full
suite coverage.

The host checks the live SSH target is J700 before uploading, verifies serial
ownership and uses only KIS00100000ch0. Root operations use the already
authenticated serial console; no new SSH/sudo privileges are installed.
Each helper rechecks boot identity, hashes and GPU health. Results are
compressed for transfer and saved under `fast-iterations/LABEL`, with per-step
timings, native renderer and kernel evidence, official QPA logs, and full-run
ledgers. The root worker's `complete` field means the iteration was collected;
only the CTS analyzer assesses suite completion.

## Hardware measurements, 2026-09-13

All three ran on boot `bac77c46-149d-42d4-aba1-dcae3a6422cb`, Xorg PID1820,
without a reboot or GPU fault:

| Operation | End to end | CTS process |
| --- | ---: | ---: |
| Initial candidate upload, desktop transition, focused case | 24.30 s | 0.97 s |
| Focused case reusing Xorg and installed library | 4.63 s | 0.98 s |
| Full unfiltered attempt, result transfer and analysis | 14.61 s | 10.74 s |

The initial transfer carried a 97.8MB library and took10.85s. SCP compression
is now enabled for future uploads. The 4.63s iteration did not transfer a
new library; it must not be presented as a measured fresh-build deployment.
The full attempt still stops at the existing draw-buffer layout assertion:
879 selected executions reached,807Pass,68Fail,3InternalError,1Incomplete,
43,022NotRun, plus15configuration passes. A completed 14.61s attempt is not
a completed suite. Diagnostic repetitions are not added to CTS coverage.

Leave the healthy CTS server available across ordinary checkpoints. Restore
KDE once at the end of a hardware test campaign or when requested, using its
existing fresh-boot guard. Do not bypass the gears/DCP once-perboot guards.
