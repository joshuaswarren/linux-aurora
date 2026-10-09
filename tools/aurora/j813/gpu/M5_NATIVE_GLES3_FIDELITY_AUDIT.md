# M5 native GLES fidelity and efficiency audit — 2026-09-13

**Result: not ready for an “all good” sign-off.** The retained GLES passes are
real test results, and native Gallium batches draws. The M5 kernel adapter
still serializes submissions, retains its first rendering VM, and accepts a
restricted render profile. These are unfinished bring-up mechanisms, not an
acceptable final substitute for the M1/M2 driver model.

The initial audit paused new hardware CTS runs and changed the evidence
validator. The follow-up [HAL300 command reservation correction](M5_NATIVE_VDM_RESERVATION.md)
fixes a reproduced 72-byte estimate for a draw path needing up to 164 bytes.
It is built and checked in the compiler-only path; hardware qualification is
pending. Shaders, CTS expectations and the installed driver are unchanged.

## Revisions and scope

- Mesa `j813`: `844aff1ff12a0f2fba0249a9d3467c4972833c60`.
- Linux `j813` at audit start: `a2d98240a63d411a0518eb50ccc90ef6b111fad0`.
- Mesa comparison: integrated AGX3 Gallium baseline
  `cc761b39c0266564a23b0882b4c3bf7de3df69e4`. The relevant source diff has
  145 files; this is a focused audit of the active submission, state, shader,
  memory-lifetime and evidence paths, not a claim of exhaustive line-by-line
  verification of every file or hardware mode.
- M1/M2 reference: the generic Asahi Gallium batching/dirty-state paths and
  Linux `drivers/gpu/drm/asahi/queue/mod.rs` in this checkout. This is a source
  architecture comparison; no matched M1/M2 performance benchmark was run.
- Unrelated pre-existing Mesa Vulkan edits and Linux filesystem case-collision
  changes were excluded and preserved.

## Verified evidence

The original v25 QPA independently parses as **2,323 cases: 2,275 Pass and
48 Fail**. Decompressing the committed QPA reproduces its original bytes.
The campaign inventory remains **54,799 cases**, with **52,476 NotRun**.
This is progress evidence, not GLES 3.0 conformance or full driver validation.

| Artifact | SHA256 |
| --- | --- |
| Original v25 `results.qpa` | `38fd0c4e83f280d303da39a23bed20b7eadb8d74dd380e6d2ec05712f4ce357d` |
| v25 `libgallium-m5-v25.so` | `78d7bfbf112baba498dd0f95c260442618056b3413a7588357de7dbaba1fbd6c` |

The native launcher unsets software rendering, no-op shim, preload and
GL/GLES/GLSL version overrides. It selects Asahi Gallium directly. The runner
records the mapped library and verifies its hash. This run used the original
CTS binary and image thresholds; offline shim results are not counted.

The retained submit log contains **5,207 submissions / 15,625 recorded draws**:
5,185 submissions have more than one draw, with a maximum of **394 draws in a
submission**. Thus the actual v25 path is not one draw per stream. This does
not establish optimal batching for all workloads.

The kernel snapshot contains only **4,373** unique contiguous completion
records, generations **835–5207**. Its ring overwrote the first 834 records.
Do not claim that the snapshot independently verifies all 5,207 retirements.
The next hardware run needs continuous kernel capture and an overrun check.

## Findings requiring correction or further qualification

### A1 — Synchronous submission prevents normal overlap (high)

`m5_drm.rs:147–156` waits on input fences in the submitting thread.
`m5_drm.rs:209–210` holds the shared runtime mutex through
`submit_application`. `m5_live_boot.rs:619–634` polls complete paired-queue
retirement with 1 ms sleeps before returning. Compute has the same pattern at
`m5_live_boot.rs:672–689`.

The M1/M2 path instead adds dependency fences to scheduler jobs
(`queue/mod.rs:759–762`), arms and pushes those jobs (`906–914`), and completes
their fences from the firmware completion path. M5 currently prevents multiple
submissions from overlapping, even if they are independent. The 1 ms polling
interval can also add latency; its precise performance cost is unmeasured.

Required correction: asynchronous job ownership and completion, real
dependency fences, bounded in-flight descriptor/ring storage and safe
retirement. Preserve necessary TA-to-fragment dependencies. Removing the
mutex or returning early without those owners would introduce lifetime bugs.

### A2 — One command, first VM and one compute dispatch are hard limits (high)

`m5_drm.rs:57` advertises one hardware command per submission, and `162`
rejects a second command. A hardware render command can contain many draws;
this is distinct from splitting every draw.

`m5_graphics_job.rs:337–341` refuses a rendering VM different from the retained
job's owner. `m5_resources.rs:566–579` permits only the first application
compute object, in the retained graphics VM, after graphics retirement.
`m5_live_boot.rs:662–663` rejects compute before the first render.
`m5_live_boot.rs:717–725` retains the session on destruction because halt and
teardown are not qualified. This explains the fresh-boot requirement between
GPU-submitting processes.

Required correction: support repeat compute and ordinary render/compute
command sequences, per-context resource ownership, VM handoff/destruction,
queue reuse and reset. Validate two simultaneous clients, sequential client
exit/relaunch, fence error propagation and memory retention under fault.
Do not delete admission or lifetime guards before the replacement is proven.

### A3 — Native Gallium still implements a narrow rendering envelope (high)

`agx_state.c:5463–5484` restricts M5 state to the qualified triangle path:
one color attachment, a limited varying layout, no depth/stencil attachment,
no MSAA, and other restrictions. `agx_state.c:3635–3637` declines texture
resources in the G17G program builder. The kernel has additional admission
restrictions at `m5_drm.rs:223–234`.

These restrictions explain why a clear/blend milestone is far from full GLES
3.0. The 48 sRGB destination-preservation failures remain actual pixel
failures. Unsupported state must be implemented from the hardware ABI, not
reported as passed, waived, silently discarded, or routed to software/Zink.

The compiler produces application shaders; the program builder points at the
linked shader's actual GPU address (`agx_state.c:3768–3791`). It does not
replace the application shader with the reference triangle. Nevertheless,
the restricted state envelope must be generalized.

### A4 — M5 state publication is less granular than M1/M2 (medium)

`agx_encode_state` skips emission when nothing is dirty, but dispatches any
dirty M5 state to `agx_encode_g17g_state` (`agx_state.c:5651–5659`). That helper
rebuilds native programs, uploads scissor/zero-bias, viewport and graphics
records, then republishes its full record set (`5528–5640`). The ordinary
Asahi path tracks individual state groups.

Required correction: track the actual HAL300 state dependencies and retain
unchanged records within their valid lifetime. Measure command bytes and
state uploads for unchanged draws versus one-state-at-a-time changes.
Do not replace this with per-draw stream splits or blanket barriers.

Follow-up: viewport and depth-bias/scissor records now follow their existing
Gallium dirty bits and batch lifetime. A real draw-path regression checks
nine state/batch transitions and the decoded raster values. Fragment-uniform
changes emit nine PPP records instead of eleven. This is a partial A4
correction; native program and graphics-record publication remain coarse.
See [raster-state evidence](M5_NATIVE_RASTER_STATE.md) for the source/build
checks and candidate hash. The physical v27 regression preserves all 2,275
passes and the same 48 failures, with all 5,207 GPU retirements captured.

### A5 — Evidence rejection missed explicit draw-admission failures (fixed here)

The previous validator detected compiler refusals and “dropping work” but
missed the M5 triangle-profile and native-program construction rejection
messages at `agx_state.c:5484` and `5534`. After either error the draw returns
without being submitted. A test expecting black could otherwise appear green.

The validator now rejects these messages, explicit native descriptor/index
admission failures, and nonzero `NEO_SUBMIT_RESULT` return values. Scoped
errors invalidate their case; unscoped errors invalidate the run. CTS results
and QPA bytes are unchanged. All seven validator tests pass. Reprocessing the
entire v25 stdout produces zero driver errors, preserving its current counts.

Remaining gap: some allocation/packing failure paths return NULL without a
diagnostic, and `agx_state.c:7667–7668` returns immediately. A string-based
validator cannot detect a silent failure. Proper driver error propagation
and complete telemetry remain requirements; absence of a log message alone
is not proof of successful execution.

## Synchronization and compiler checks that did not show these shortcuts

- Native Gallium's observed draw batching is described above. Honeykrisp's
  optional `HK_G17P_DRAW_SPLIT` and publication caps return disabled for G17G
  (`hk_cmd_draw.c:106–107`, `143–144`); they do not control native GLES.
- The VDM start barrier is emitted once during batch initialization
  (`agx_state.c:4925–4944`), matching the existing M1/M2 coherency purpose.
  The post-draw barrier at `7720–7725` is conditional on transform-feedback
  writes, not unconditional after every M5 draw.
- M5's fragment SKSM entry depends on its corresponding tiler stamp
  (`m5_graphics_queues.rs:134–148`). That is a real producer dependency.
  Host publication fences and range TLB invalidations are not equivalent to
  inserting full GPU execution barriers everywhere. Their minimal ordering
  still needs review when asynchronous execution is implemented.
- The recent compiler fixes lower source widths/modifiers and sRGB transfer
  semantics according to operation/format, not CTS case names or expected
  pixels. The reviewed compiler/tilebuffer diff contains no test-name or
  shader-signature substitution. The sRGB implementation uses Mesa's standard
  conversion path; alpha remains linear. The remaining 48 failures prevent a
  broad correctness sign-off.
- The physical diagnostic launcher disables the shader cache and Gallium
  threading. It also uses detailed diagnostic output, one CPU and a thermal
  frequency cap. This configuration is not an M1/M2 performance comparison.
  Shader-cache/threading-disabled results must not be presented as normal
  desktop throughput or proof of thread safety.

## Completion gates

1. Fix the actual 48 blend failures without changing CTS, expected pixels,
   image thresholds or shader semantics. Preserve the full existing regression.
2. Replace the synchronous, first-VM adapter restrictions with proper
   asynchronous ownership, dependencies, queue reuse, repeat compute and
   recovery. Test concurrency and error paths, not only one successful client.
3. Generalize texture/varying/depth/stencil/MSAA and remaining GLES state;
   propagate unsupported or failed work explicitly.
4. Prove efficient state reuse and batching with normal driver settings and
   targeted workload measurements. No artificial one-draw streams or blanket
   synchronization to make image tests pass.
5. Run the complete unmodified GLES 3.0 inventory on the final driver and
   required configurations with complete provenance, error and completion
   evidence. Review every non-pass and unrun case.

None of these broader gates is complete. The subsequent v27 hardware
regression qualifies command reservation and partial raster-state reuse,
with complete printk capture, but does not resolve the other findings.
The GPU/CTS objective remains unresolved; this document does not certify
completion or remove the safety guards.
