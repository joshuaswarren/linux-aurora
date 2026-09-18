# G17P native preamble contract and exact compiler extent

## Result and limits

The host's A18/G17P encoder independently confirms the short-inline /
long-external constant-program mechanism represented by our Ioto preshader
helper. The compiler now records the exact emitted preamble size in isolated
Mesa commit `dec426fd7a7e`, branch `j700-preamble-20260915`, based on the
cache-fixed `b79a17bbe78` candidate. Compiler and precompiled-kernel CPU tests
pass. This metadata change is not installed on the Neo and does not enable
runtime preambles. The live full CTS campaign retains its original binaries.

## Native provenance and witnesses

`ipsw dyld macho` extracted
`/System/Library/Extensions/AGXMetalG17P.bundle/Contents/MacOS/AGXMetalG17P`
from the host cache at
`/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/dyld_shared_cache_arm64e`.
The output is `probes/native-metal-host27/AGXMetalG17P`, 14,489,768 bytes,
SHA-256 `666816a37045b6dcd351b63ad56a1f5ce284f3acb2157262226e090ade9cfca2`.

The bundle reports version `360.34.5`, platform `27.0`, and DTPlatformBuild
`26A5412a`. This is a G17P/HAL200 reference from the host OS, not a binary
captured from the Neo's macOS 26 firmware. The earlier saved G17G/HAL300
reference must not be mislabeled as A18 evidence.

`probes/native_metal.py` checks the digest and Mach-O bounds before decoding.
`probes/verify_native_preamble.py` verifies 46 selected instruction witnesses
and three callee symbols; `native-preamble-witnesses.json` records the pass.
These are static witnesses, not a whole-function proof or hardware result.

`ESLStateLoadEncoderGen2<HAL200>::setConstantCalculationProgramInfo` starts at
`0x216b5c0dc`; `finish` starts at `0x216b5b2c8`.

- The setter reads start/end32 at descriptor `+4/+8` and alternate end at
  `+0x20`, choosing the positive unsigned 32-bit difference or alternate end
  minus start. The default inline threshold is 256 bytes (`0x216b5c274`).
- A larger size or flag `+0x28` selects an external address. Otherwise the
  setter copies code at CPU base `+0x18` plus start, trimming terminal NOPs.
- External GPU base `+0x10` plus start is saved at encoder `+0x108`;
  the inline trimmed length is saved at encoder `+0x110`.
- Prelude request `+0x29` is gated by encoder `+0x100` and saved at `+0x11c`.
- `finish` copies inline code or emits absent `0e 00 00 00`. The external
  path optionally emits LDIMM and then B, not BL: `0x216b5b674` calls
  `AGXIotoInstruction_B_0::emit` at `0x216b123fc`.

This agrees with the helper's suffix structure. It does not prove register
allocation, scratch, uniform lifetime, or resource visibility for every
Mesa-generated preamble. No barrier or separate dispatch was introduced.

## Compiler change and validation

Previously `agx_shader_info` recorded the preamble's start and presence but
not its emitted length. Using the whole binary end would depend on function
order and the absence of later segments. The new `preamble_size` is captured
immediately after packing that function and preserved by
`agx_compact_kernel_info`. Machine code and entry addresses are unchanged.

The regression compiles the uniform-pointer workload with preambles enabled
and disabled for AGX2 and G17P. It checks presence, range bounds, compact
metadata, and the G17P END instruction at the range boundary. The unsupported
AGX3A case remains rejected.

`build-preamble-size.py` results:

- Full `agx3_compile_fail_closed_test`: passed.
- `asahi-g17p-precompiled-fill`: 487 checks passed, exercising generated kernel
  metadata after the compact structure layout changed.
- Baseline and restored tests also passed with identical executable hashes.
- All three builder source files were restored byte-for-byte.
- Builder Mesa remained SHA-256
  `2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
- The new worktree is committed and clean. Original Mesa/kernel tracked diff
  hashes remain unchanged; `git diff --check` passed.

Logs: `preamble-size-host.log`, `preamble-size-{baseline,candidate,restored}-build.log`,
and `preamble-size-{baseline,candidate,restored}-{test-name}.log`.

## Shared graphics and compute composition

Follow-up commit `549b7e7c8a2` adds the preshader as an explicit terminal
component of the native G17P program composer. Both shared graphics and compute
builders accept it. Inline bytes or the external B follow the existing loader
and stage-specific protocol; no extra dispatch, submission, or barrier is
introduced. Graphics allocation bounds account for the actual suffix length.
The ordinary absent form retains the old output and allocation bound.

`build-preamble-composition.py` validates six graphics combinations (vertex,
fragment, coverage fragment, each with and without resources) and both compute
resource cases. Each checks the compiler-produced 36-byte inline preamble, an
independent external-branch byte fixture, unchanged loader prefixes, and invalid
address rejection without output mutation. Compute launch records are unchanged.

Candidate results:

- Compiler regression suite and graphics program test: passed.
- Preshader core: 682 checks passed; program core: 913 checks passed.
- Precompiled-kernel metadata: 487 checks passed.
- New compute preamble tests: passed, with an explicit success marker.
- The whole Honeykrisp program-core suite does **not** pass. Baseline, candidate,
  and restored source all fail the same packed-integer tile-layout assertion:
  `hk_agx3_g17p_rt0_tile_layout_supported(&tib)` for B10G10R10A2_UINT. Original
  line 2409 becomes line 2471 after the new test. Tests after that failure are
  not executed. The new compute test runs before it; this is not reported as a
  whole-suite pass.
- A `BITFIELD_MASK(partitions)` conversion warning also exists in baseline,
  candidate, and restored builds; this change introduces no new warning.

The first build script attempt stopped on the baseline failure before installing
any candidate source (`preamble-composition-host.log`). The second records this
specific failure explicitly while checking every other result
(`preamble-composition-host-2.log`). All 11 builder source files and all six test
executables were restored byte-for-byte, and the builder Mesa library hash
remained unchanged. Neither candidate commit has been installed on the Neo.

Additional native caller evidence: compute finalize at `0x216bab58c` and vertex
finalize at `0x216b8b6c8` pass the same descriptor shape. Owner offsets
`+0xee8/+0xeec` supply start/end, `+0xef4` supplies force-external, and
`(+0xef8 != 0)` supplies the optional prelude request. The underlying meaning
of `+0xef8` has not been established; it must not be guessed to mean scratch
size or register count. The compute constructor also derives a prelude request
from serialized metadata at `0x216ba9530` before its setter call at
`0x216ba9548`.

The initialization of `+0xef8` is now traced through the embedded ESL object:
the compute constructor passes owner `+0x9d8` to its ESL constructor at
`0x216ba77cc`. That constructor stores a vector length at ESL `+0x520`
(`0x216beb3d4`), which is owner `+0xef8`. The vector is reached through the
first table of the serialized metadata's vtable `+0x38` vector, then that
table's vtable `+0x0a` field. Finalize loads the count, tests it against zero,
and writes the result to descriptor `+0x29` before calling the setter.
The extended verifier checks these instructions against the digest-pinned
binary and passes. The vector's element semantics are still unresolved; the
trace does not justify mapping this count to Mesa scratch or GPR metadata.

Further static inspection shows the same vector participates in the native
load builder. At `0x216bea810` through `0x216bea828`, its count contributes
`min(count, 38) + 10` to the temporary-register starting point when encoder
`+0x100` is set; otherwise the start is 10. At `0x216beac3c` through
`0x216beac68`, entries are compared with a load's start/end in half-slot units
and converted to a per-load bit mask. The mask is passed in `x5` to
`EslBuilder::loadFromIndirectSCS` at `0x216bead34`. This narrows the metadata
to load/input handling, rather than a scalar scratch-size field. The complete
entry ABI and its mapping to Mesa-generated preambles remain unqualified.
These additional observations are not counted in the 46-instruction verifier.

## Upload and lifetime audit

`PREAMBLE-UPLOAD-AUDIT.md` records the source audit at Mesa `549b7e7c8a2`.
The original compiled shader BO already holds the entire binary, including
the exact preamble range, on both fresh and disk-cache paths. The linker copies
main ranges into a different allocation. Consequently, an external preamble
must use the original compiled BO's VA plus `preamble_offset`, not the linked
main BO plus that offset. Existing batch references keep the original BO alive.
Executable BOs use the existing non-writeback GEM allocation path; this audit
does not identify a need for another upload, allocation, or CPU cache clean.
It is source evidence, not a runtime preamble or GPU cache-coherency result.

Follow-up `ba1dc83f997` implements checked original-binary range binding in the
shared preshader core, preserving caller-selected force/prelude flags. The
graphics and compute composition regressions use it and pass; the preshader
suite now passes 724 checks. The existing broader Honeykrisp failure persists.
See the upload audit for validation and restoration details. Gallium guards
remain in place pending entry-ABI qualification.

## Compiler-generated external case

Commit `a3f2a8616a9` adds a 32-input uniform calculation to the compiler
regression, producing an 834-byte G17P preamble and a 558-byte AGX2 preamble.
Both enabled and disabled forms compile, and exact range/non-overlap, scratch,
END, and compact-metadata checks pass. The full compiler suite and 487
precompiled-kernel checks pass with builder restoration. See
`PREAMBLE-EXTERNAL-COMPILER-AUDIT.md` for measurements and limitations.

## Compiler entry-register contract

Commit `733acb27f15` closes a reproduced compiler validation gap: an explicitly
constructed preamble could read a fixed per-invocation GPR even though the
loader supplies no main-shader invocation-register ABI to that preamble.
The reproducer uses the software-vertex invocation intrinsic in a compute
preamble and keeps its result live through a preamble store and main-shader
global store. Before the fix, both AGX2 and G17P accepted it.

The compiler now rejects a live `PRELOAD` in a preamble after dead-code
elimination and before register allocation removes that operation. Failure
uses the existing whole-shader unwind, returning no binary or valid shader
metadata. Uniform-file sources are distinct from `PRELOAD` and remain legal.
The normal NIR preamble optimizer already restricts eligible intrinsics; this
adds an explicit backend contract for manually supplied preambles as well.

The final regression checks three cases on each of AGX2 and G17P:

| Input location | Expected and observed result |
|---|---|
| Preamble reads a uniform pointer/constant buffer | Compilation succeeds |
| Preamble reads an invocation GPR | Compilation fails; binary and metadata are cleared |
| Main reads an invocation GPR, preamble reads uniforms | Compilation succeeds |

The before-fix evidence is in `preamble-entry-before-candidate-agx3_compile_fail_closed_test.log`.
The first fixed run is `preamble-entry-after-*`; the complete positive/negative
matrix is `preamble-entry-final-*`. Run it with
`build-preamble-size.py --prefix preamble-entry-final`. The complete compiler
suite and 487 precompiled-kernel checks passed in baseline, candidate, and
restored phases. All three builder source files and both test binaries were
restored byte-for-byte, with builder Mesa still
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
No new build warnings occurred, and `git diff --check` passed before commit.

Source tracing also confirms that G17P uniform reads have explicit operand
kind/slot encodings (`agx3_copy_core.h` and device-load packing); generic ALU
uniform operands are materialized by `agx_lower_uniform_sources` before RA.
The new guard proves the explicit incoming-GPR boundary, not all implicit
hardware state, native LDIMM policy, or runtime preamble correctness. This
compiler gap has not been linked to an observed CTS failure, and no coverage
gain is claimed from these CPU tests. The Neo's installed compiler is unchanged.

## Remaining integration

Commit `29e2161ec9f` wires graphics and compute preambles into Gallium's program
builders. It validates exact, non-overlapping main/preamble ranges in the
original compiled BO, separately from the linked main entry, and rejects
missing register metadata or preamble scratch. Existing batch BO references
retain that allocation. No new upload, draw split, or barrier was added.

The global G17P `no_preamble` compiler switch is still enabled. The candidate
requests no optional native LDIMM prelude, based on its uniform-only input
contract; that choice and implicit entry/uniform-publication state still need
runtime qualification. Precompiled/background paths retain their rejection
guards. See `PREAMBLE-UPLOAD-AUDIT.md` for complete-library build receipts.

Before enabling vertex preambles, resolve the native launch-count mapping
identified in `PREAMBLE-UPLOAD-AUDIT.md`. The current vertex path still inserts
legacy preshader-register bits into a word with a different native layout.
The expanded native verifier passes 72 instruction witnesses plus three callee
checks, but the recovered scalar-37 producer is not yet mapped to Mesa metadata.

After that mapping is qualified and the full CTS ends, run hardware on/off workloads for uniform-dependent
compute, vertex, and fragment work, including inline and external preambles,
then affected CTS families. Compiler size/GPR improvements alone are not a
measured speedup or proof of M1/M2 efficiency parity.

`PREAMBLE-RUNTIME-PROBE.md` now records a prepared GL 3.3 VS/FS uniform probe:
12 software-validated workloads with 64 draws per readback, plus negative
controls that detect stale direct-uniform and UBO inputs. No Neo run or runtime
preamble qualification is claimed. Compiler metadata must establish actual
inline/external coverage before interpreting a later hardware pass.

## Native schema identification and positive constant-program captures

At 2026-09-16 01:50 UTC, the host compiler's embedded FlatBuffers reflection
schemas resolve the previously unnamed fields. This supersedes the uncertainty
about scalar 37 above. `probes/inspect-native-shader-schema.py` reads the local
compiler without invoking it and checks SHA-256
`eb941548121e8e962a7bc9acd2f80d613017601ce967ecf1ae27fb14079ec683`.
All four embedded shader schemas agree:

| ShaderInfo field | Native name |
| --- | --- |
| 0 | `temporary_register_count` |
| 14 | `spill_buffer_bytes` |
| 26 | `const_calc_phases` |
| 37 | `thread_invariant_largest_ipr_bytes` |
| 38 | `largest_ipr_bytes` |
| 41 | `entry_spill_buffer_bytes` |
| 42 | `entry_ipr_bytes` |

The phase table's field 2 is `temporary_register_count`; field 3 is
`cprog_uniform_inputs`, a vector of uint32. Thus the existing native vertex
count chain consumes an IPR byte footprint, not the preamble temporary GPR
count. Scalar 0 is a temporary-register count, not a stage identifier.
The 18-bit state-load count still needs its separate producer audit.

`native-shader-schema-host27.json` records the decoded schemas. The existing
72-instruction/3-callee verifier now also checks all four schemas against the
compiler digest. The reflection format comes from
[FlatBuffers reflection.fbs](https://github.com/google/flatbuffers/blob/master/reflection/reflection.fbs).
Read-only shared-cache inspection also confirmed Apple's separate named
constant-calculation temporary-register statistic; its format follows
[Apple's cache layout](https://github.com/apple-oss-distributions/dyld/blob/main/include/mach-o/dyld_cache_format.h)
and [pointer layout](https://github.com/apple-oss-distributions/dyld/blob/main/include/mach-o/fixup-chains.h).

Re-decoding the already captured native workloads gives positive constant-program
evidence, even though scalar 37 is zero. `probes/decode-native-preamble-captures.py`
checks capture/source hashes and the actual native symbols in addition to the
metadata. Its receipt is `native-preamble-uniforms-decoded.json`.

| Uniform inputs in source | Constant-phase temporary count | `cprog_uniform_inputs` | Thread-invariant IPR bytes | Main symbol offset |
| --- | --- | --- | --- | --- |
| 0 | 1 | empty | 0 | 0x40 |
| 1 | 2 | 0, 1, 2, 3 | 0 | 0x40 |
| 32 | 33 | 0, 1, 2, 3 | 0 | 0x2c0 |
| 64 | 65 | 0, 1, 2, 3 | 0 | 0x540 |

Every capture names and defines `agc.main.constant_program`, at text offset 0.
The earlier 202/202/842/1482-byte text sizes include that code and padding;
they are not main-only sizes. Main IPR fields 38/42 remain 20 bytes.
Do not infer temporary-register units from these names or count padding as
exact preamble instructions. No host or Neo GPU execution was performed.

The nonempty native `cprog_uniform_inputs` vectors also sharpen the outstanding
GPU0-prelude audit: a uniform-only Mesa preamble is not, by itself, evidence
that the native vector would be empty. Audit the actual compiled entry inputs
and state-load interaction before enabling it; do not inject a prelude blindly.

## Second count identified; native arithmetic checked directly

The 18-bit G17P count is now traced too: ShaderInfo field 31,
`thread_invariant_spill_buffer_bytes` (vtable offset 0x42), is stored at ESL
+0x260 and copied to +0x26c. Vertex owner +0xc44 feeds `ceil(bytes / 4)` into
encoder +0x128; native finalization inserts that count into pipeline bits 3:20
and stores the template at vertex owner +0xf38. The native verifier now passes
93 instruction witnesses, three callee checks, and four pinned schemas.

`probes/verify-native-preamble-counts.py` executes only six checked arithmetic
instructions from the pinned native binary in ordinary host CPU functions.
It compares the production helper against those instructions for each legal
IPR byte value and each legal spill byte value, mixing the other input:
1,064,950 comparisons pass under UBSan, plus seven rejection/zero-output cases.
This is independent of the C rounding implementation, but is not GPU execution.
`native-preamble-counts-oracle.json` records the source hashes and instruction
bytes. The limits are command-field bounds, not a claim that every resource
size is physically supported by the GPU.

Additional normal offline captures are retained in
`native-preamble-sampling-host27` and `native-preamble-pressure-host27`, with
derived `*-decoded.json` receipts. Uniform/varying 2D samples and uniform cube
samples still have zero thread-invariant IPR; their main IPR is 32 bytes. The
128/256/512-input chains also have zero thread-invariant IPR, and their constant
phase temporary count plateaus at 71. None provides the missing nonzero-IPR
shader witness; do not relabel their main footprint as a preamble footprint.

There is a positive spill-field witness: every capture with constant-buffer
inputs reports **32 thread-invariant spill bytes**, including the original
one-input workload; the zero-input control reports zero. The native conversion
therefore produces spill count 8 (pipeline resource bits 0x40). This fixed
small amount is not evidence of GPR-pressure spilling. Its relation to native
uniform-input/entry handling must be audited before applying it to Mesa's
separately compiled preambles. Do not add a blanket 32-byte reservation merely
because Apple emits one for these programs.

## Input ABI follow-up

`PREAMBLE-INPUT-ABI-AUDIT.md` now connects the native input-copy helper to
actual compiled entry reloads: 33 default-path copy cases and 86 reloads
across 17 captures round-trip through the typed TPS encoder under UBSan.
Seven multi-buffer controls report 0/32/32/48/48/80/144 spill bytes for
0/1/2/3/4/8/16 pointers. The payload is packed at TPS byte offset 16, with
the witnessed storage footprint rounded to 16 bytes. This replaces the
earlier unknown relationship between small spill fields and entry inputs;
it does not yet qualify Mesa's input ABI or justify a blanket reservation.
