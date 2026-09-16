# G17P preamble upload and lifetime audit

Source checkpoint: isolated Mesa `549b7e7c8a2`, with exact-size metadata from
`dec426fd7a7e`; kernel `4863a3c90a3b`. Neither preamble commit is installed on
the Neo. This audit made no driver, kernel, firmware, or hardware changes.

## Address ownership

All Mesa paths below are relative to `mesa-j700-preamble-20260915`.

- `src/gallium/drivers/asahi/agx_state.c:1828` allocates the original executable
  BO using `info.binary_size` and copies the entire compiler binary. Its
  `info.preamble_offset` and `info.preamble_size` therefore describe bytes in
  this allocation.
- `src/gallium/drivers/asahi/agx_disk_cache.c:111` and `:123` restore executable
  BOs and CPU binary copies on cache hits. Both use the same allocation flags.
- `src/asahi/lib/agx_linker.c:386` copies only each part's `main_size` bytes
  starting at `main_offset`. The linked main BO is a separate address domain;
  adding the original `preamble_offset` to it is invalid.
- `src/gallium/drivers/asahi/agx_state.c:4088` already gives native compute
  append both `constant_bo` (the original compiled BO) and `linked` (the main
  executable). Caller `:7759` passes `cs->bo` as the former.

The intended external descriptor is thus CPU `cs->b.binary + preamble_offset`
and GPU `cs->bo->va->addr + preamble_offset`, with the exact recorded size.
Inline composition uses the same CPU extent. Before admitting either form,
validate presence, nonzero size, offset/size against both compiler binary and
BO bounds, required CPU/BO pointers, GPU-address overflow/alignment, and scratch
requirements. Do not let a missing external VA silently turn a long preamble
into the helper's absent-program form.

## Lifetime and publication

Vertex and fragment shader updates retain both original and linked allocations
at `agx_state.c:2808` and `:3088`. Native compute append retains `constant_bo`
and the linked BO at `:4240`. Geometry's original and copy shader allocations
are retained at `:7323`. `agx_state.h:1133` deduplicates BO handles and holds one
reference per batch until completion. Reusing the original allocation adds no
per-draw upload and does not require another allocation.

Both compile and cache-restore paths use `AGX_BO_EXEC | AGX_BO_LOW_VA`, without
`AGX_BO_WRITEBACK`. `src/asahi/lib/agx_device.c:392` only requests
`DRM_ASAHI_GEM_WRITEBACK` for that explicit flag. In
`neo-encoder-linux-j700-readback-20260915/drivers/gpu/drm/asahi/gem.rs:213`,
user GEM objects set `map_wc` when WRITEBACK is absent. The Linux implementation
of `agx_drm_bo_sync_for_device` at Mesa `agx_device.c:88` is a no-op; calling it
cannot supply an additional cache-publication guarantee.

This establishes which existing upload, mapping, and lifetime mechanism the
new range should use. It does not establish GPU instruction-cache behavior,
prove every shader's uniform/scratch ABI, or qualify hardware preambles.
There is no evidence here supporting extra blanket barriers, command streams,
or submissions. Runtime validation must use the ordinary submission path.

## Remaining native metadata question

The optional external prelude is gated by a serialized vector length. The
compute owner field `+0xef8` is initialized in its embedded ESL object's
constructor at offset `+0x520`, rather than by a direct outer-object store.
The updated `probes/verify_native_preamble.py` passes 46 selected instruction
witnesses and three callee-symbol checks against the pinned host macOS 27
G17P/HAL200 binary. Vector element meaning remains unresolved. This is not
evidence that Mesa should set the flag from scratch size or register count.

Next qualification remains: establish the prelude/input ABI for compiler-made
preambles, integrate exact ranges with the existing allocations, then run
uniform-dependent compute/vertex/fragment on/off checks and affected CTS cases
after the current full campaign ends. No hardware test may contend with that
campaign for the Neo.

## Checked range binding

Follow-up Mesa commit `ba1dc83f997` adds `agx_ioto_preshader_bind_range` to the
shared preshader core. It accepts the original binary's CPU pointer/length,
GPU base/allocation length, and the exact preamble offset/length. It rejects
empty or odd ranges, missing CPU or GPU mappings, either buffer being too
short, arithmetic overflow, and a range extending beyond the 48-bit branch
address domain. On failure the descriptor is unchanged. On success only its
code pointer, code length, and GPU address change; all force/prelude policy
flags remain the caller's explicit choice.

The helper checks declared ranges; it cannot prove that a caller supplied the
matching original BO, qualify the instruction bytes or entry ABI, or retain a
BO itself. At `ba1dc83f997`, Gallium admission guards remained intact. This
shared primitive alone did not enable runtime preambles; later Gallium wiring
is recorded below.

Graphics and compute regressions now bind the known compiler-produced preamble
at offset 128 within a larger original binary. Its GPU base deliberately
differs from the linked main's address. The tests verify exact inline bytes and
an independent external-branch fixture, with unchanged resource prefixes and
compute launch records. Automatic external selection remains tested as well.
Bounds tests include truncated CPU/GPU copies, odd ranges, missing mappings,
`SIZE_MAX` overflow inputs, and a valid range ending exactly at the 48-bit limit.

Validation command from the workspace root:

```
python3 neo-linux/artifacts/j700-gl30-cts-20260913/build-preamble-composition.py --prefix preamble-range
```

`preamble-range-host.log` records passing compiler/graphics tests, 724 preshader
checks, 913 program-core checks, 487 precompiled-kernel checks, and the focused
compute preamble success marker. The broader Honeykrisp executable still fails
the same baseline B10G10R10A2_UINT tile-layout assertion; its later tests remain
unexecuted. Baseline, candidate, and restored builds have the same pre-existing
`BITFIELD_MASK(partitions)` conversion warning and no new warning.

The script restored all 14 builder source files and all six test executables
byte-for-byte. Builder Mesa remained SHA-256
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
No candidate was copied to the Neo. `git diff --check` passed before commit.

## Gallium wiring and full-library validation

`29e2161ec9f` binds the original compiled binary and BO in both graphics and
compute launch construction. It rejects invalid ranges, overlapping main and
preamble code, absent register metadata, and preamble scratch. Checked graphics
allocation includes the suffix. Existing original/linked BO references retain
the code without additional allocation, upload, submission, or barriers.

The global G17P compiler `no_preamble` switch remains enabled; the Neo under CTS
has not changed. The candidate does not request the optional native LDIMM entry
prelude. Explicit incoming GPR reads are rejected by `733acb27f15`, but implicit
entry state and generated-uniform publication remain hardware-unqualified.

`build-preamble-composition.py --prefix preamble-gallium --gallium` built the
complete library, saved as `candidates/preamble-gallium-candidate.so`, SHA-256
`2953b1aa83cf7a9763c5f110eac9b5cee7bfe5e56e933c1bfa728cb8139e4d99`.
Compiler/graphics tests, 724 preshader checks, 913 program-core checks, 487
precompiled-kernel checks, and focused compute preamble checks passed. The
broader Honeykrisp suite retained its known packed-integer tile-layout failure.
All 16 builder sources, six CPU executables, and the Mesa library were restored
byte-for-byte. `preamble-gallium-host.log` contains the restoration receipt.

## Invalid-descriptor release behavior

The complete build exposed an existing `launch_sources` warning in the
precompiled compute path, reproduced in baseline/candidate/restored builds.
The rejection helper used assertions plus `UNREACHABLE`: with `NDEBUG`, that
macro promises impossibility to the compiler instead of guaranteeing runtime
termination. This was not traced to any current CTS failure.

`2a7391e997e` replaces that helper's assertions/`UNREACHABLE` with an unconditional
`abort()` and a `NORETURN` declaration, preserving its diagnostic. It adds no
successful-path GPU work. `check-descriptor-rejection.py` extracts the exact
production helper and exercises it in isolated Linux builder subprocesses:
both optimized debug and `NDEBUG` builds log the rejection and terminate with
SIGABRT. Device/log helpers are stubbed; this proves helper termination, not
hardware error recovery. See `descriptor-rejection-check.log`.

The full build/test command was repeated with `--prefix preamble-rejection
--gallium`. The candidate warning is gone; the unrelated Honeykrisp conversion
warning and known baseline failure remain. All focused checks above pass, and
all builder inputs/binaries were restored byte-for-byte again.
`candidates/preamble-rejection-candidate.so` has SHA-256
`ec9cafb14f55f9948adc7b80ba4199044098547a77feeef50a463b215156807e`.
Neither candidate has been installed on the Neo.

## Vertex launch metadata must be resolved before enablement

The register audit found a separate launch ABI gap. At `2a7391e997e`, Gallium
still builds G17P's `raw_vertex_shader_word0` with the AGX2
`VDM_STATE_VERTEX_SHADER_WORD_0` packer. That packer puts
`ceil(nr_preamble_gprs / 16)` into bits 12..15. The raw G17P state-update helper
copies this word without translating those bits. The current global
`no_preamble` switch keeps the preshader contribution zero, so this newly
identified risk has not been exercised by the running CTS.

The pinned host-macOS-27 G17P finalizer instead inserts an encoder field into
bits 8..19 at `0x216b8b838`. Its recovered producer chain is:

1. Serialized scalar 37 (vtable offset `0x4e`) is stored at ESL `+0x278`.
2. Vertex finalization reads owner `+0xc50`, consistent with ESL at `+0x9d8`.
3. It computes `round_up(ceil(value / 4), 2)` for non-overflowing inputs and
   stores the result at encoder `+0x12c` (`0x216b8b644`..`0x216b8b654`).
4. The vertex template reads that field and inserts its low 12 bits at bit 8.

`probes/verify_native_preamble.py` now checks 72 pinned instructions plus three
callee symbols, including this producer/consumer chain. The higher-level
meaning of scalar 37 is still unresolved; a mapping from Mesa GPR counts is not
established. Do not substitute `max(main_gprs, preamble_gprs)`, copy AGX2 count
bits, or invent a fixed native reservation from this evidence.

`probes/capture-native-preamble-uniforms.py` produced four offline native
vertex pipelines under `native-preamble-uniforms-host27`. For 0, 1, 32 and 64
uniform-chain inputs, vertex text sizes are 202, 202, 842 and 1482 bytes;
ShaderInfo scalar 37 remains zero and scalar 38 remains 20 in all four. These
captures do not establish that Apple's compiler formed a preamble, and they
do not reveal a nonzero scalar-37 producer. They provide compiler provenance,
input sources, metadata, native objects and hashes for further comparison.
No GPU was executed.

This launch-field mapping is now an explicit prerequisite to a preamble-on
vertex trial. The shared program wiring and software probe are retained, but
generation stays disabled. Fragment and compute launch metadata must be audited
independently; passing CPU composition checks is insufficient.

## Follow-up: distinct IPR metadata preserved

`PREAMBLE-NATIVE-AND-SIZE-AUDIT.md` now names scalar 37 as
`thread_invariant_largest_ipr_bytes` using four digest-pinned native compiler
schemas, with positive constant-program witnesses in all four existing captures.
Zero IPR is valid even when a constant program has temporary registers.

Commit `66af913ca3b` on `j700-preamble-20260915` preserves the compiler's
preamble staging footprint and its known/unknown flag separately from main in
both full and compact shader metadata. Previously only main retained these
fields. It also documents the proven 12-bit count meaning. No launch word,
shader instruction, barrier, draw grouping, or enablement setting is changed.

The full compiler regression proves known-zero preamble IPR survives compilation
and compaction for short/long uniform-load programs. The long G17P preamble has
16 half-registers of temporary GPRs but zero IPR staging. My first test assertion
incorrectly required nonzero IPR; its failure is retained in
`preamble-ipr-metadata-*`. Corrected, native-supported checks pass in
`preamble-ipr-metadata-v2-*`, together with graphics composition, 724 preshader,
913 program-core, and 487 precompiled-fill checks. Focused Honeykrisp preamble
composition passes; the complete Honeykrisp suite retains its separately
documented baseline packed-integer tile-layout failure. The only candidate build
warning is the baseline BITFIELD_MASK-to-uint8 conversion in that test.

The complete candidate library built successfully, SHA-256
`edbed23798365553f491a6cde5fc0e181979fd1ab8ab911133265cc872e0281f`, at
`candidates/preamble-ipr-metadata-v2-candidate.so`. It is not installed.
The builder's 16 overlaid sources, six test executables, and Mesa library were
restored byte-for-byte; baseline library SHA remains
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.

Next: qualify a nonzero preamble IPR workload, implement the correctly bounded
native launch-count conversion, finish independent fragment/compute resource
audits, and resolve the newly named `cprog_uniform_inputs` entry contract.
The existing Gallium BO wiring still requires those gates before enablement.

The final commit also updates only the VDM header's explanatory comment. That
comment was added after the successful build; it has no compiled effect. The
build helper now includes that header in its overlay/restoration set for future
trials (17 sources with `--gallium`). Original `mesa-j700` and
`neo-encoder-linux` dirty-worktree diff hashes were rechecked unchanged.

## Follow-up: G17P vertex resource words use native counts

Commit `6743d7ad6c1` adds the bounded G17P conversion for the now-proven IPR and
spill byte fields and uses it in Gallium. The vertex word no longer receives
AGX2 uniform/texture/sampler/GPR fields. The geometry path reads the GS copy
shader's metadata, the same owner passed to pipeline construction. The AGX2
packing remains in the AGX2 branch. Missing preamble IPR metadata is rejected;
existing preamble spilling rejection remains in place.

This changes vertex resource words even with preambles disabled, so the current
live campaign cannot qualify the candidate. Native shader-flag bit 31 retains
the existing Gallium value zero; its VsStateConfig semantics remain a separate
audit. No barrier, draw split, command-stream boundary, GPU allocation, or
preamble enablement was added by this change.

Validation: native ARM arithmetic agrees across 1,064,950 input pairs and seven
rejection controls under UBSan. The existing VDM suite plus new conversion and
packing regressions pass **79,622 checks** on the host and builder. Full compiler,
graphics composition, 724 preshader, 913 program-core, 487 precompiled-fill, and
focused Honeykrisp preamble checks pass. The documented unrelated whole-suite
Honeykrisp packed-integer failure and warning are unchanged.

The full candidate is `candidates/preamble-native-resource-counts-candidate.so`,
SHA-256 `a090d12ec1c0ce343bfcd2e4a79dc642b48840a5ab3e7887fd94b2342a6664f7`.
It is not installed. `preamble-native-resource-counts-*` contains the build and
test receipts. All 19 overlaid sources, seven test executables, and the builder's
baseline Mesa library were restored byte-for-byte.

After the full campaign finishes, qualify these resource-word changes with
preambles still disabled before testing preamble-on. The full official run,
fragment/compute launch handling, nonzero preamble IPR, native uniform-input
entry ABI (including input-count-dependent TPS storage), and efficiency
comparison remain open. Source and CPU verification do not close those gates.

The input-storage path is now grounded in `PREAMBLE-INPUT-ABI-AUDIT.md`:
33 default native input-copy cases and 86 compiler entry reloads across 17
captures round-trip through the typed TPS encoder. Storage grows with packed
input count; the 32-byte example is not a universal allocation. The candidate's
uniform-only compiler IR and rejected PRELOADs do not prove its input ABI or
justify suppressing Apple's optional entry prelude. No generation was enabled
and no candidate installed during this audit.

Follow-up `0a7050edd9e` adds the bounded native TPS input-copy primitive with
explicit byte offsets, masked half copies, and precise source-slot waits.
It matches 2,856 emulated native helper cases under UBSan. The full Mesa build
and relevant CPU suites pass with the known unrelated Honeykrisp baseline
failure unchanged. See `PREAMBLE-INPUT-ABI-AUDIT.md` for exact scope, counts,
and the new uninstalled `preamble-cprog-copy-v2-candidate.so` hash. All 24
overlaid files and nine CPU test binaries were restored. The primitive is not
yet connected to compiler input mapping or runtime preamble enablement.

`d4b97495b74` now records the final preamble uniform input set, preserves
known-empty metadata through compaction, and adds pure G17P packed-offset and
input-frame queries. Sparse/boundary/dead-read compiler fixtures pass, the
load core passes 3,326 checks, and all 17 native capture footprints match the
production helper. The full `preamble-input-layout` build passes the relevant
suites and restores all 27 files and ten test binaries. The known unrelated
Honeykrisp failure is unchanged. See `PREAMBLE-INPUT-ABI-AUDIT.md` for candidate
hash/build ID and the still-required IR lowering, loader emission, dependency,
storage, external-entry, and hardware work. No candidate is installed.
