# J700 GPU and DCP port

## Scope and provenance

- Target: `codex/j700-gpu-dcp-port`, initially `1e88269259939317666c7422e9534de2ad559c3e` (clean).
- Source: read-only `../linux`, `drm-agx3` at `9fc9aa2fe0373a95e6d2f0224655917ad77de8a7`, including its uncommitted GPU/DCP changes.
- Common ancestor: `c85eec3bf95b22b15f87bedf5cb29717b01586f8`.
- Read parent AGENTS.md, GPU_HANDOFF.md opening, DRM_ARCHITECTURE.md, native README-BUILD.md and build-asahi-ko.sh before editing.
- No deployment or reboot. Build and test outputs stay inside this worktree. No changes to the original source tree.

## Integration decisions

- Apply the cumulative source delta from the common ancestor only for `drivers/gpu/drm/asahi`, `drivers/gpu/drm/apple`, `include/uapi/drm/asahi_drm.h`, and the two supporting Rust kernel API files. Use three-way application to preserve target changes.
- Target sample descriptors and completion generation commits are patch-equivalent to source commits. Its render epoch tests/lifecycle match the corresponding source integration. Same-VM graph reuse is also integrated in the source, with later cache selection and retirement refinements. Retain the target's qualification tools and J700 platform code.
- Include new source configuration/channel modules and focused tests. Exclude the empty generated `module-common.c` from source inputs.
- Include phase-specific partial-render depth/stencil flags; exact compute retirement/rebind, render/recovery ownership, live-context native TLBI, and quiet defaults are one coherent driver snapshot.
- Use the pinned native Debian GCC 14.2/Rust 1.85/bindgen 0.71.1 workflow from README-BUILD.md, extended to full Image, modules and J700 DTB. The older container build document is not applicable.

## Validation

- Three-way conflicts were reviewed in `g17_manager.rs`, `g17_render.rs`, `g17_resources.rs` and five tests. The target changes were already incorporated and refined in source. Resolving the individual conflicts to those refinements produced byte-identical production GPU/DCP files, without modifying target platform or qualification files.
- Initial run: 121/130 Python source/model scripts passed. All nine failures reproduced against the untouched source with bytecode writes disabled (`.port-validation/source-baseline/tests.json`).
- Repaired stale host harnesses for private FList IDs, logical-control object IDs, census pool selection, quiet logging, exact-retirement close state, the two-GiB client arena, and factored mapping helpers. No production behavior changed for these repairs. All nine repaired scripts passed (`.port-validation/repaired/tests.json`).
- Replaced the TSQ test's missing `/tmp/g17p-re/fwb1/b1_raw.txt` dependency with an 11-instruction fixture decoded from the authoritative B1 image, verified SHA256 `cf352729699211decba77bea2499501b3df3b02665ac0a12316090e6cf5741fb`. The fixture preserves objdump's instruction words and condition aliases. The host publisher and firmware predicate tests pass.
- Added a focused host harness for the existing production partial-ZLS UAPI and register tests; both pass. The harness extracts pure manager geometry and selects the partial-ZLS UAPI test to avoid unrelated historical standalone-test dependencies.
- Optional `test_g17_render_samples.py --all`: 20 pass, 3 fail on both target and untouched source. Failures are `cold_opening_descriptors_apply_only_the_native_first_work_overrides` (expected 1, actual 0), `descriptors_match_m1n1_9165484_and_duplicate_pipeline_fields` (historical descriptor hash), and `register_programs_match_m1n1_9165484` (historical register hash). Evidence: `.port-validation/render-all.log` and `render-all-source-baseline.log`. Do not change these expected values without a current independent descriptor oracle. Standalone submission/UAPI test modules also contain obsolete producer struct fields and calls; the focused harness does not claim full standalone module coverage.
- `git diff HEAD --check` passes.
- Final focused run: **131/131 scripts pass**, zero failures (`.port-validation/final/tests.json`, individual logs in the same directory; invocation `AURORA_TEST_RUN=final python3 tools/aurora/test-j700-gpu-port.py`). This includes all three DCP scripts and both phase-specific partial-ZLS tests.
- Selected coverage: 720,432 native PI_300 TLBI operands, 20 exact async-retirement cases, 4 render timestamp-epoch cases, 13 render-owner/concurrency cases, and DCP allocation failure/fence/cancellation/ownership-transfer paths. The refined arena test also passes with the private FList size derived from production constants (`.port-validation/exact-arena/tests.json`).
- `.port-validation/source-manifest.json` records source and port SHA256 for all 143 imported inputs. Production inputs match exactly; deliberate differences are host-test repairs and complete stack-audit failure reporting. The four additional repairs to previously unchanged target tests, the B1 fixture and the focused ZLS harness are visible in the worktree diff.
- Compiled stack audit **fails**, with identical measurements on the new target object and the existing source-built object: probe **5664/5120** bytes, runtime start **2144/2048**, boot-initial-session **4192/4096**, module-internal direct-call estimate **18576/18432**. Other audited frames are within budget. The estimate excludes external/indirect/recursive paths and is not a hardware stack-overflow result. The audit now reports every violation before failing; thresholds are unchanged. Evidence: `.port-validation/stack-frames-all.log` and `stack-frames-source-object.log`. Source object SHA256 `44596c99ca1432988d932e826e7d41e28964773c6574a8022a73941a1c465004`; port object `c178149b7f34f4da8e59b565a4b6530ae70743b6ba4fcab0335bc27e2f034b0e`. Reduce construction/probe stack use or independently requalify the budgets before shipping.
- **Full native cross-build succeeded, exit 0**: `bash tools/aurora/build-j700-gpu-port.sh`, kernel `7.1.6-neo-unified`, J700 DTB and **1592 modules**. GCC Debian 14.2.0-19, Rust Debian 1.85.0, bindgen 0.71.1; host source/model tests used Rust 1.96.0. The build used `../linux-build/.config` as its seed and retained 16-KiB pages, Rust, modular Asahi/DCP, and built-in Apple NVMe/DART/RTKit/SART/ext4. Config SHA256 `8f19ca64c914125152b9f69dec008cca41d2a63250d95e4f49adc6c22190dee8`. Full log: `.port-validation/build.log`; machine-readable evidence: `.port-validation/build-receipt.json`.
- Module export checks: Asahi **312 imports / 0 unresolved**; DCP **276 imports / 0 unresolved**, against this full build's `Module.symvers`. Both vermagics: `7.1.6-neo-unified SMP preempt mod_unload aarch64`. Compiler warnings include bindgen diagnostics and inherited Rust unused-variable/import/mutability warnings plus an unknown lint under Rust 1.85; none was a build error.
- At the initial port checkpoint, hardware behavior was untested and the port remained uncommitted because of the stack-budget and historical descriptor-oracle failures. The subsequent request to integrate the latest local changes and publish `dj/j700-staging` supersedes that checkpoint decision; the outstanding qualification limits remain documented below.

## Initial built artifacts

All paths below are relative to `.port-build/`.

| Artifact | SHA256 |
| --- | --- |
| `arch/arm64/boot/Image` | `0ca2f96923505bd862a1a2ba3ad1588fc89d68877e5f655c4f39d363253d20c4` |
| `arch/arm64/boot/dts/apple/t8140-j700.dtb` | `0c7d5fbd42c981558f117ff27e0b85936477289036fb1126487b2f5e5f1487d9` |
| `drivers/gpu/drm/asahi/asahi.ko` | `4a4a29e8d1b47f677ed949155f427682306a58e6a7df48e321cff5497583d61b` |
| `drivers/gpu/drm/apple/appledrm.ko` | `851f472e8c796dd4afdc5280b0e98141af8a468ac3bd990a05e620e94243b714` |

## Latest local integration (2026-09-21)

- Branch: `dj/j700-staging`, created from the existing J700 port worktree. Source HEAD remains `9fc9aa2fe0373a95e6d2f0224655917ad77de8a7`; this integration includes its latest uncommitted DRM changes. The source tree is read-only throughout this work.
- Update twelve production Asahi files for two render pairs per logical queue, per-slot render FList ownership, publication/completion timing, compute installation fairness, register-derived compute resume boundaries, and SCS encoding. Production GPU/DCP inputs match the source snapshot. Preserve J700 platform code and qualification history, the prior DCP integration, and the supporting Rust/UAPI changes from the initial port. The public UAPI header has no new delta relative to the initial port; this update requires no new Mesa ABI change.
- Exclude unrelated source device-tree/power-manager and AVD changes, and the empty generated `module-common.c`.
- Add the five new source tests for compute installation fairness, SCS encoding, keyframes, render FList ownership and render multi-flight. Preserve portable B1 fixtures, production-derived logical-control and arena harnesses, the mapping-helper repairs, and aggregate stack-audit reporting.
- Initial latest-source run: 130/136 scripts pass. All six failures also reproduce against the untouched source. Repair host adapters for completion timing fields and the production queue-limit module, accept the mutable timing variable, and correct the multi-flight Python model to retire at two concurrent physical pairs rather than attempting four commands while forbidding pair reuse. This matches the production admission limit; production concurrency is unchanged.
- Final focused run: **136/136 scripts pass**, zero failures (`AURORA_TEST_RUN=latest-final python3 tools/aurora/test-j700-gpu-port.py`; `.port-validation/latest-final/tests.json`). `git diff HEAD --check` also passes.
- Full native incremental build (`bash tools/aurora/build-j700-gpu-port.sh`) succeeds, including Image, all modules and J700 DTB. Asahi has 312 imports and DCP 276, with zero unresolved exports against the resulting `Module.symvers`. Both vermagics remain `7.1.6-neo-unified SMP preempt mod_unload aarch64`.
- Latest Asahi module SHA256: `dddd46d5a955aaf053fe87be837b2c4e0f18c88eef7a2bdcd2cabdbc732d59b1`. Image, DTB and DCP module hashes remain those in the initial artifact table. Build log: `.port-validation/build-latest.log`.
- The broader render-oracle run remains **20 passed / 3 failed**, with the same three failures listed above (`.port-validation/render-all-latest.log`). Expectations are not rebaselined.
- Compiled stack audit remains failing: probe **5760/5120**, runtime start **2160/2048**, boot-initial-session **4192/4096**, and module-internal direct-call estimate **18688/18432** bytes (`.port-validation/stack-frames-latest.log`). The estimated chain is 112 bytes larger than the initial port. Budgets are unchanged; this estimate is not evidence of a hardware stack overflow.
- This is a staging integration, not a hardware qualification result. No deployment or reboot was performed. Resolve or independently requalify the stack budgets and historical descriptor oracles before treating this as release-qualified.
