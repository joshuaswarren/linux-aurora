# J700 CTS implementation audit — 2026-09-13

Follow-up: `EOT-AUDIT-FOLLOWUP.md` records the repository reproduction tooling
and independently grounded mixed-format correction completed after this
checkpoint. Those source findings have advanced; the broader sign-off remains
open. The original audit and its evidence manifest are a historical snapshot.

**Verdict: not ready for an “all good” claim.** The reviewed changes do not
introduce one draw per command buffer, blanket per-draw barriers, test-name
dispatch, or fabricated CTS passes. They do contain unfinished bring-up paths,
and neither full conformance nor M1/M2-level efficiency has been demonstrated.

The user requested a pause for this audit. Hardware feature experiments were
not continued. The only new product fixes in this audit are a missing include
and a reproduced EOT descriptor-validation defect. The larger EOT and sparse
MRT changes were already pending when the audit began.

## Scope and baseline

Reviewed the working delta in `mesa-j700` against j700 HEAD
`1cf27f9be3d78735a857f06e2256b2c2e34fd1cc`: 36 tracked modified files plus
23 untracked source/probe files. This includes compiler/RA, linker/Ioto,
tilebuffer/BG/EOT, Gallium resource and draw paths, the shared Honeykrisp ABI
updates, tests, and the test harness/accounting relevant to the campaign.
Unrelated repositories, kernel bring-up and all historical AGX3 changes are
not represented as having received a complete audit here.

The M1/M2 source comparison uses the local `origin/main` snapshot
`6dfbc555b4128ee51139c5f78c5aba2594c9701b`. This is a pinned local baseline,
not a claim to have fetched the latest upstream. No M1/M2 hardware benchmark
was performed.

## Findings and disposition

### Fixed: explicit EOT masks could bypass validation

In `src/asahi/compiler/agx3_graphics_program.c`, malformed metadata could make
the new compact-mask path ineligible and then match the old RT0 descriptor.
For example, a one-target mask with first physical offset 4 was accepted;
an explicit mask with legacy attachment_count=0 could also be accepted.

The new regression first failed at line303 of
`test-agx3-graphics-program.c`, proving the bad acceptance. The builder now
rejects an explicit mask unless the complete compact attachment ABI matches.
The negative tests check that rejection leaves the output unchanged. Valid
legacy descriptors remain available when no explicit mask is supplied.

Evidence: `audit-eot-guard-red-detail.log`, `audit-eot-guard-build.log`,
`audit-eot-guard-green.log`. All 255 nonempty masks exercise the loader path;
the final four affected suites pass. The first targeted invocation used one
nonexistent test name and ran no tests; its replacement used the actual Meson
names. It is not counted as a test pass.

### Corrected in source, not hardware-qualified: EOT partition interpretation

The previous wide EOT input confused fragment synchronization classes with
physical imageblock partitions. Direct native UMD instructions establish:

```
partition = tile_offset / sample_stride
offset_argument = tile_offset % sample_stride
```

All currently admitted compact RGBA8 offsets are below their sample stride,
so every EOT attachment uses partition zero. The corrected native programs
for 1..8 active targets are 68, 120, 164, 208, 252, 296, 340 and 384 bytes.
Their recorded hashes match the source arrays. The eight-target program has
128 staging bytes; the CPU loader tests accept that metadata. The old
1,226-byte eight-target program was generated from the wrong input. Smaller
code here is not a measured performance claim.

The fragment logical-target and synchronization-mask work remains supported
by separate native fragment captures. It must not be reverted because of the
EOT correction. Full derivation and disassembly pointers are in
`EOT-PARTITION-CORRECTION.md`.

Remaining work: qualify the corrected RGBA8 path on hardware; review the older
mixed RGBA16F/R8 EOT's partition-one input; make the complete EOT generation
recipe reproducible from the repository. The current EOT templates are
internal native builtins selected by layout, not application shader hashes,
but their generator/serialized inputs still live in campaign artifacts.
This is a production-readiness limitation. A general compiler lowering is
preferable to proliferating separately maintained templates.

### Open: preamble optimization is disabled on G17P

`agx_state.c` sets `no_preamble` for every G17P API stage. The pending change
extends the existing restriction to geometry/tessellation shaders lowered
to compute. The M1/M2 baseline has no such blanket setting at this call site.
This avoids emitting an unsupported executable preshader, but repeats work
that the baseline can hoist. Its runtime cost is not measured.

Do not enable an unimplemented preshader just to remove the flag. Recover and
implement the execution/resource contract, then validate correctness and
measure representative shader workloads. Until then, efficiency parity is
unproven and this restriction remains an explicit limitation.

### Open: general MRT and MSAA behavior remains incomplete

Wide/sparse fragment admission currently requires straight-line opaque RGBA8
stores at compact offsets. Wide blending, arbitrary formats, layered wide
MRT and mixed clear/load operations still encounter unsupported paths.
`agx_build_bg_eot` explicitly rejects mixed background operations. The small
set of allowed paths must not be presented as full framebuffer support.

These rejection guards are not a conformance strategy. Do not lower reported
capabilities, add waivers, or split a draw into separate per-target draws to
make the suite finish. Implement the missing general operation and resource
contracts while retaining batching.

Existing MSAA color, depth/shadow, clip-distance and transform-feedback
failures remain in the saved official results. The known CPU robust-index
golden failure is also unresolved. No claim of their correctness is made.

## Efficiency and shortcut checks

| Concern | Source evidence | Audit conclusion |
| --- | --- | --- |
| One draw per command buffer | `agx_draw_vbo` appends to `batch->vdm.current`; the same three capacity-triggered batch flush sites occur in the M1/M2 baseline | No new per-draw submission found |
| Multidraw expansion | `util_draw_multi`, indirect unrolling and the two software indirect fallbacks occur in both baseline and current source | Inherited baseline behavior, not a new CTS workaround |
| Blanket barriers | `agx_legalize_xfb` and `agx_texture_barrier` are byte-identical functions to the baseline; the XFB self-barrier remains conditional | No new unconditional draw barrier found |
| New stencil flush | Added only for the separate stencil resource's actual writer, excluding the current batch | Dependency tracking, not a full-device flush |
| Tessellation barriers | Preserves existing scopes/semantics and translates shader_out to global after address-space lowering; pure execution barriers still disappear | Semantic preservation, not extra barriers everywhere |
| Native meta-dispatch barrier | Strong flags are selected only for AGX_BARRIER_ALL; that enum is a distinct bit, not all flag bits | Maps an existing requested barrier; hardware minimality not proven by this audit |
| Pixel synchronization | Shared MRT classes are acquired once and released after their final active target; six independent native sparse oracles support the sequence | No per-target reacquire/release workaround |
| MRT register allocation | ST_TILE payloads are limited to halves0..15; a full 16-half terminal store bank places other texture coordinates above it, still below128 | Specific allocator constraint with CPU coverage; occupancy/copy cost still needs measurement |
| Trace overhead | Added descriptor logging and retired R8/MSAA scans require debug flags; full CTS has tracing disabled | Diagnostic-only cost, excluded from normal test timing |
| Shader/result substitution | Reviewed product delta contains no case-name or application-shader-hash selection; normalized loads retain actual memory reads and format conversion | No test-recognition path found in reviewed delta |

The register placement is not an unconditional high-register policy. The
pre-RA ordering pass restricts its terminal region and keeps store payloads
live, so the full-bank condition has a concrete cause. That does not establish
optimal scheduling: native instruction/copy counts, allocated registers and
occupancy remain necessary performance evidence.

## Validation and accounting

The interrupted build was terminal with a missing `util_bitcount` declaration.
Adding `util/bitscan.h` restored the build. The complete Asahi/poly run then
reported **264 OK, 2 ExpectedFail, 1 Fail**. The failure is
`asahi-g17p-precompiled-fill`, already present in prior campaign results; it
was not waived or relabeled. After the descriptor fix, the build and four
affected suites passed. `git diff --check` passed.

VM source hashing checked 58 non-Markdown changed/untracked files: 52 matched.
The six absent files were offline `.metal`/`.mtlp-json` native probe inputs,
not Mesa build inputs. All compared product and compiled test sources matched.
These CPU/shim/software-control suites prove their specific contracts, not
rendering on a physical GPU.

Read-only SSH revalidated J700, the unchanged boot and Xorg PID1820, and the
installed Mesa hash
`c5826185f5ff6c80d176f0836b952addc19525445f1c5c409e737dca693cabfa`.
The new local `libgallium-audited-eot.so` was not installed. The resident, SSD,
other Mac, and hardware test state were not changed by this audit.

All **1,182 installed X11 CTS files** match their installed package manifest.
The local original surfaceless package differs only in `glcts`, `cts-runner`
and `build-configuration.txt`; test data matches. The X11 runner hash matches
the previous hardware receipt. This is package-integrity evidence, not a
fresh source-to-binary reproducibility proof.

The full runner selects official `--type=gl30`, with an empty inherited
environment, explicit native renderer validation, no trace flags, no case
filter and no API/extension version overrides. Focused diagnostics are stored
separately. Zink is used by the separate display server, not the CTS client.

Recounting `fast-iterations/sparse-identity-full/results/sparse-identity-full`
gives **43,901 selected executions** across the configured official sessions:

| Result | Executions |
| --- | ---: |
| Pass | 807 |
| Fail | 68 |
| InternalError | 3 |
| Incomplete | 1 |
| NotRun | 43,022 |

The additional 15 configuration passes are kept separate. Only 2/52 sessions
are complete, including configuration/no-context checks; no main session
completed. Exit134 is an assertion stop, not a conformance result. Repeated
diagnostics do not increase unique suite coverage.

## Conditions for eventual sign-off

1. Resolve the open correctness and validation failures without weakening
   tests, advertised requirements or accounting.
2. Complete the missing general EOT/MRT operations and executable preamble
   path with reproducible source/encoding evidence.
3. Measure batching, command/barrier counts, shader instruction/copy counts,
   register usage/occupancy and representative CPU/GPU time against the
   M1/M2 design baseline; explain necessary hardware differences.
4. Run every official desktop GL3.0 session on the final candidate, preserving
   complete receipts, logs, renderer identity and package/source identity.
5. Repeat this audit on that exact final source and binary. A CPU pass, a
   focused hardware pass or lack of a GPU fault cannot substitute for this.

The audit checkpoint is complete as a review artifact. The requested clean
implementation and conformance end state are not complete, so the goal must
not be marked achieved or artificially blocked merely to stop continuations.
