# GL 3.3 completion audit — 2026-09-15

Subsequent work: `RESTART-BALLOT-FIX.md` records an independent restart fix at
`8326b87458c`. The late-readback issue remains reproducible with that fix
installed (92/2,988 first-read mismatches). The restoration state below is
the end of this earlier completion audit, not the latest Mesa branch head.

The full OpenGL 3.3 goal remains active. This investigation establishes a
stronger readback oracle; it does not fix the defect or claim more CTS passes.
No production delays, additional barriers, draw splitting, or kernel changes
were introduced. Mesa `j700` remains clean at `4aad4e2ea25`.

## Direct hardware evidence

The existing `probes/compressed-readback-race.c` executable was run on the
identified Neo, then under llvmpipe as a software control. Each 3,000-iteration
run has 2,988 reference comparisons after initializing twelve references.

| Experiment | Observation | Artifact |
|---|---|---|
| Original driver, initial 1,500-iteration check | 16/1,488 first-read mismatches; all 16 second reads correct | Tool output in this turn |
| Original driver after verified restoration, 3,000 iterations | 45/2,988 first-read mismatches; all 45 second reads correct | `fence-audit-baseline-restored.log` |
| llvmpipe, same executable, 3,000 iterations | 0/2,988 mismatches | `fence-audit-llvmpipe.log` |
| Observe the same mapped staging buffer before/after 200 us, without another GPU command | 52/6,000 mappings changed; logged examples include valid pixel bytes | `fence-observe-same-map.log` |
| Pin to CPU 0 and continuously poll the same mapping for up to 200 us, without sleeping or submitting another GPU command | 18/6,000 mappings changed; all 18 changes include valid pixels | `fence-observe-busy-same-map.log` |

The second experiment addresses a limitation in the earlier oracle: a second
`glGetTexImage` submits another GPU operation, so it cannot itself prove that
the first mapping changes after its synchronization path returned.

The observer runs at the linear R8_UNORM return in `agx_transfer_map`, after
`agx_prepare_for_map`. The changed pinned samples report `usage=1` (read),
`bo_flags=8` (writeback), and `valid=1`. It snapshots bytes in the same mapping;
the polling variant counts pixel changes separately from row padding. This
demonstrates late visibility after the driver's map/synchronization path.
It does not yet identify the exact missing firmware/cache completion condition.

## Diagnostic isolation and restoration

`probes/agx_pipe-fence-observe.c` is a diagnostic copy outside the Mesa worktree.
It is not production source and must not be used for conformance qualification.
Its interval intentionally lets late data become observable; zero probe
mismatches with observation enabled are **not a fix**.

Verified binaries:

- Original/restored: `b1ad237f0ae2c358285ecdf1be060bf109af7bce04ec7b4b2bbfe264870b87ba`
- Sleep observer: `52cb30f68b1df407b686712851f6559d7f398e2e7a0c888dec66f0a692ae414f`
- Pinned/busy observer: `01f10a88b55ea94c49b5f2dab2a8f29a43fdb55ad78452169145024dc10d21e9`

The VM source was restored byte-for-byte and rebuilt. Both its final library
and the installed Neo library match the original digest above. Final hardware
receipt: `fast-iterations/fence-observe-final-restore-20260915/remote-report.json`;
complete, official LOD smoke passed, no GPU fault. Resident, kernel, SSD layout,
and the other Mac were not changed. The loaded kernel module build ID remains
`a947f573e40321482d8e76da54c1663561075deb`.

One intermediate rollback was rejected because the installer used only the
candidate filename as the backup key. The guarded installer now includes the
previous content digest and verifies any existing backup, preserving distinct
predecessors across A/B/A experiments. The successful final rollback exercises
this change. `fence-observe-busy-disabled.log` was captured while the observer
binary was still installed with observation disabled; it is **not** a result
for the original binary. No failed rollback was treated as successful.

## Next investigation boundary

Source provenance correction (verified on 2026-09-15): the running module was
built from `/Users/mcruz/Developer/aurora-silicon/neo-encoder-linux`, branch
`j700`, commit `c85eec3bf95b22b15f87bedf5cb29717b01586f8`, with
`kernel-msaa-samples.patch`. The older
`neo-linux/sources/linux-native-display-20260910` checkout used for the initial
source audit is not the source of this module. All 77 Asahi Rust files in the
correct checkout match the builder source. The builder module and the running
module both carry build ID `a947f573e40321482d8e76da54c1663561075deb`;
`kernel-msaa-samples-boot/manifest.json` records the build inputs. Hardware
measurements above are unchanged; the following references use the correct tree.

The M1/M2 path (`workqueue.rs:852`, `event.rs:120`) treats an event as a wakeup,
reads the current stamp, and completes commands against that value. G17P's
`service_async_render_completions` collects QID bits and then validates queue
retirement, an empty job list, nonzero pass timestamps, and EventControl
unlinking. A mere difference between these protocols is not proof of a bug.

The next step is to correlate a late read with the exact G17P job's physical
completion/stamp and cache-writeback lifecycle, using a native witness where
the protocol is not established. Inspect `g17_live_boot.rs:4781`,
`g17_manager.rs:6811`, `g17_resources.rs:10363`, and the native completion
records. Do not add sleeps, broad flushes, or speculative register writes.

The timestamp storage is newly allocated for each restaged render
(`g17_resources.rs:8353`); do not assume it simply reuses a previous job's
timestamp pair. The async output-fence publication fix is already in this
kernel source (commit `45d52d561bc0e`), so do not rediscover it as a new patch.

## Conformance scope

The handoff's 9,886-case resumable sweep is a useful complete case inventory
for its selected configuration. Its runner directly invokes `glcts` at config
1, 64x64, seed 1. It is not by itself the complete multi-configuration official
`cts-runner --type=gl33` qualification. Finishing the goal requires the official
applicable GL 3.3 schedule, correct accounting of unsupported/error outcomes,
and evidence that batching/synchronization retain the requested efficiency.
The 22 handoff non-passes remain unresolved; no full sweep was rerun here.
