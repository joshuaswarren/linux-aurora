# G17P constant-program input ABI — 2026-09-16

## Result and scope

The pinned native `copyGprToCprog` helper stages default-path constant-program
inputs in TPS. Independently compiled native preambles reload those same TPS
locations. Inputs are packed, rather than placed according to the largest
source uniform number. This is stronger evidence than identifying COPY/RLD
opcodes by name or inferring an ABI from the presence of compiler uniforms.

This is **host macOS 27 G17P/HAL200 encoder and offline compiler evidence**.
No native driver was loaded, no GPU program executed, and no Neo state changed.
It does not qualify Mesa preambles or establish whether an alternative ABI
using direct uniform reads is valid during constant-program execution.

## Exact encoder evidence

`probes/native-ioto-copy-oracle.py` emulates only the digest-pinned native COPY
constructor, emitter, and their local callees in Unicorn. Native image pages
are read-only, execution addresses are whitelisted, and pointer-authentication
bookkeeping is modeled. All data writes go to emulated stack/object storage.

`probes/native-cprog-input-oracle.py` additionally emulates native
`copyGprToCprog` at `0x216b59054`. The only modeled external operation is byte
vector insertion at `0x216b63608`; it captures the real emitter's bytes.
It checks 66 combinations of masks, destination offsets, and encoder flag
`+0x100`. The default path has 33 checked cases; the alternate path's TPS
fallback and GPR constructor types are also checked.

Default path:

- The raw destination argument is a **byte offset**. It maps to
  `TPS half = 8 + destination_bytes / 2`.
- Mask bits select 16-bit source halves. A two-half copy can use a 32-bit
  instruction when the destination is aligned; an odd half address produces
  two 16-bit copies.
- The helper returns `2 * popcount(mask)` bytes. The native caller advances
  its destination by the same byte quantity at `0x216b58300`.
- The full helper marks source registers killed after copying. A raw COPY
  constructor called with only its low value word does not establish this
  policy: the high word matters.
- Encoder flag `+0x100` selects GPR destinations for offsets below 152 bytes;
  higher offsets return to TPS with a 76-half adjustment. The meaning and
  enablement policy of that flag remain unqualified.

For source GPR half 10 with source availability value 1, destination byte
offset 0, and mask 3, the complete native helper emits
`4b0b0f00224000140000`. The production typed decoder identifies
a 32-bit store to TPS half 8, source half 10, source-kill set, slot 6, wait mask
2. Re-encoding reproduces every byte. Destination byte offset 4 moves the
store to TPS half 10.

The supplied source availability value is an oracle input, not a captured
`finishRound` scheduling decision. The test does not establish that every
native call uses wait mask 2.

Native Register types 8/9 select TPS 16/32-bit copies respectively. Types 2/3
select GPR 16/32-bit copies. The earlier artifacts named
`native-cprog-copy32-oracle.json` and `native-cprog-copy64-oracle.json` were
named before this decoding: their actual instruction widths are **16 and
32 bits**, not 32 and 64. Their raw bytes remain useful; do not infer widths
from those filenames.

The existing field named `cprog_dest_half` in the load core is therefore
misnamed: its arithmetic divides by two to obtain a half-register address,
and the uniform-program builder advances it by four bytes per dword. No
production arithmetic or ABI was changed during this audit. Rename only with
all callers accounted for; do not introduce an extra scale conversion.

## Independent compiler captures

`probes/capture-native-preamble-buffers.py` compiles normal Metal shaders with
0, 1, 2, 3, 4, 8, or 16 distinct constant-buffer pointer arguments. Each pointer
contributes one uniform integer to a hash exported by the vertex shader.
The ordinary offline compiler produces real `agc.main.constant_program`
symbols and schema-decoded input vectors.

| Buffer pointers | Input half count | Thread-invariant spill bytes |
| ---: | ---: | ---: |
| 0 | 0 | 0 |
| 1 | 4 | 32 |
| 2 | 8 | 32 |
| 3 | 12 | 48 |
| 4 | 16 | 48 |
| 8 | 32 | 80 |
| 16 | 64 | 144 |

For these captures, the nonempty-input footprint is
`align(16 + 2 * input_half_count, 16)` bytes. This is a witnessed relationship,
not a universal formula for arbitrary pressure spills or other shader ABIs.
The empty-input control has zero footprint. All these preambles have zero
thread-invariant IPR; they do not supply the missing nonzero-IPR witness.

The one-pointer entry starts with:

| Native bytes | Typed decoding |
| --- | --- |
| `0c88092760000080` | 32-bit TPS half 8 to GPR half 0; slot 2, wait mask 32 |
| `1c8a0827` | 32-bit TPS half 10 to GPR half 2; slot 2, no wait |

The native store signals slot 6 and the first reload waits on its bit. That is
an encoding-level link, not a justification for adding global waits elsewhere.
Native texture captures have source input indices `[8,9,10,11]` yet use the
same TPS halves `[8,9,10,11]` as pointer inputs `[0,1,2,3]`: the entry payload
is packed independently of those source indices.

## Reproduction and verification

From this artifact directory:

```sh
.native-oracle-venv/bin/python probes/verify-native-cprog-inputs.py \
  --mesa /Users/mcruz/Developer/aurora-silicon/mesa-j700-preamble-20260915 \
  --output native-cprog-input-verification.json
.native-oracle-venv/bin/python probes/native-cprog-input-oracle.py \
  --output native-cprog-input-oracle.json
```

The first command builds the production static TPS decoder/encoder under
UBSan, checks 33 default input-copy cases, and verifies **86 entry reloads
across 17 native captures**. It reads sections from digest-checked native
objects, validates source hashes and metadata schemas, and checks exact byte
round-trips and complete packed input ranges. The receipt includes source and
header hashes, compiler/driver digests, and the build command.

## Consequences for Mesa enablement

The optional GPU0 external-entry prelude is **two bytes**, `0c10`. A source
cross-check against `agx3_pack_ldimm_u32(AGX3B, 0, 16)` identifies a 32-bit
load-immediate to GPR half 0 with value 16. The pinned native LoadImmediate
parser at `0x216b2f378` independently extracts immediate 16 and destination 0
from this compact form. This is not a decoded explanation of why the entry
needs it: its connection to the first RLD's structural flag/implicit state
remains unresolved. Do not change prelude policy based on the literal alone.

The candidate remains disabled by `.no_preamble` on G17P. Its uniform inputs
and rejected PRELOADs do **not** prove that the optional native external-entry
prelude is unnecessary. The descriptor-binding code does not qualify that ABI.

Before enabling preambles:

1. Establish how Mesa's preamble inputs are collected and mapped at entry.
   If using the native TPS ABI, lower reads and copy only the required inputs
   from already-loaded GPRs, preserving their lifetimes and packed offsets.
2. Account for the actual input storage separately from pressure spills and
   IPR. Do not reserve 32 bytes for every program or use a GPR count as IPR.
3. Resolve external-entry LDIMM/prelude policy, fragment/compute resource
   state, and publication of generated uniforms. Preserve targeted producer
   dependencies; do not add blanket barriers or split draw streams.
4. After the current full CTS campaign is terminal, qualify candidate
   `6743d7ad6c1`'s changed vertex words with preambles **off** first. Then run
   the prepared on/off workload matrix and full affected conformance tests.

The current live CTS campaign tests the installed cache/completion fixes. It
cannot qualify this uninstalled candidate or these CPU-only ABI findings.

## Production encoder primitive: `0a7050edd9e`

The isolated candidate now includes `agx3_ioto_pack_cprog_copy` in
`src/asahi/compiler/agx3_ioto_load_packer.{c,h}`. It implements the native
G17P default TPS path for one aligned source dword and mask 1/2/3:

- Destination is explicitly a packed **byte offset**.
- Partial masks pack only the selected half; odd TPS-half destinations use
  two half-width copies for mask 3, matching the native helper.
- Copies are last uses of their selected source halves, signal slot 6, and
  wait only on the supplied source slot (or none if it is already ready).
- Invalid chip, alignment, register, mask, or wait-slot inputs leave output
  unchanged. The API reports both emitted code bytes and copied data bytes.
- This primitive does not allocate TPS, emit entry reloads, change barriers,
  split streams, or enable preambles. The compiler/loader connection and
  external-entry policy remain the next implementation steps.

`probes/verify-cprog-copy-packer.py` compares the actual production function
against the emulated native helper for **2,856 cases** under UBSan. It covers
all seven supported source readiness choices, all three masks, register
boundaries including halves 252/254, alignment transitions, and byte offsets
through 65534. Every emitted byte and returned data-byte count matches.
`cprog-copy-packer-native-oracle.json` retains the cases, build commands, source
hashes, and pinned native digest. Native fixtures and rejection checks also
raise the repository's standalone packer suite from 230 to **271 checks**.

Full build/validation is in `preamble-cprog-copy-v2-*`: loader 271, uniform
program 219, preshader 724, program core 913, precompiled fill 487, and VDM
79,622 checks pass, as do the full compiler and graphics suites and focused
Honeykrisp preamble composition checks. The existing Honeykrisp packed-integer
whole-suite failure remains unchanged. The first build caught a missing
standalone Honeykrisp link dependency; it was fixed before the successful v2
build. Both build attempts restored the VM baseline.

Candidate: `candidates/preamble-cprog-copy-v2-candidate.so`, SHA-256
`63449d05b801c3e231c7df33b81bf68a820ccd17d6dac69e62b7c8228fc3cf2f`.
All 24 changed sources/build files, nine test executables, and the baseline
Mesa library were restored byte-for-byte. The candidate is **not installed**.

## Compiler input metadata and shared layout: `d4b97495b74`

The compiler now records `agx3_preamble_inputs`: a 512-bit source-uniform-half
set plus a `known` flag. It scans final lowered IR after register allocation
and last-use handling, so dead references and main-only inputs are excluded.
Known-empty is preserved rather than being confused with absent metadata.
`agx_compact_kernel_info()` retains the complete structure, and the precompiled
shader generator serializes that structure using its current size.

`agx_ioto_cprog_input_offset()` provides the shared compiler/loader contract:
selected source halves are packed in increasing index order and the returned
offset is relative to the input payload at TPS byte 16. The metadata does not
itself change the generated shader reads. `agx_ioto_cprog_input_frame()` counts
the selected halves and returns the input-only frame size, separately from
pressure spills and IPR. Both helpers reject unknown metadata and unsupported
chips, and preserve output arguments on rejection. The 512-half domain is a
bounded source-register layout, not a physical TPS-capacity qualification.

Verification:

- The load-core suite passes **3,326 checks** under UBSan and in the Linux
  build. Cases include all bitmap-word boundaries, sparse and full sets,
  known-empty versus unknown, unsupported targets, and unchanged outputs.
- Real compiler fixtures cover sparse 16-/32-/64-bit inputs at halves 8,
  64, 127, and 508; half 511 is retained correctly. Dead reads at half 196
  disappear. Constant-only preambles produce known-empty input metadata.
  Full and compact metadata agree. Existing AGX2 controls still pass.
- `native-cprog-layout-verification.json` checks the production helpers
  against all 17 source/native-object-hash-verified offline captures; input
  counts and frame footprints match their actual native metadata.
- The long G17P arithmetic fixture remains main 36 bytes / preamble 834 bytes
  with generation enabled, and main 840 bytes when disabled. This metadata
  change does not add shader instructions to that fixture.

`preamble-input-layout-*` contains the successful full Mesa build and CPU test
logs. All 27 overlaid source/build files, ten test executables, and the baseline
Mesa library were restored. The known unrelated Honeykrisp packed-integer
whole-suite failure remains unchanged; its preamble checks pass.

Candidate `candidates/preamble-input-layout-candidate.so` has SHA-256
`24fec79335d18a302c5e7a59cc404676a76b8f637d7fda15b2abf01b80bdbe3c`
and GNU build ID `70c60f3e0cb6032875d0a3315516be4d215aae58`. The prior candidate
has build ID `bd7062cdac5ddfee1fd2bb912180f8494b109452`. Source inspection of
`agx_disk_cache_init()` confirms that the ELF build ID selects the cache
namespace, isolating this changed metadata layout from older cache entries.
This is a source/build check, not a warm-cache hardware test.

Remaining connection: materialize preamble input reads using this map, emit
the matching copies from the loader's already-loaded registers with correct
last-use handling, import the producer dependency into the preamble, and
account for input storage alongside pressure spills. The optional external
entry prelude and first reload's structural/implicit state still need an
explanation. Generation remains disabled, and the candidate is not installed.

## Loader integration and source lifetime: `2fed294aa41`

The optional `agx3_ioto_uniform_program_desc.cprog_inputs` now connects the
shared packed input layout to the actual resource-load body. It emits input
copies from the registers already loaded for the ordinary uniform upload.
The ordinary upload is preserved even with a known-empty input mask. A source
needed by a following TPS copy is kept live through its ordinary uniform copy;
the TPS copy retains the native last-use policy and signals slot 6. It adds no
new memory loads or wait instructions. Input positions depend on source index,
not range order or allocator round order.

Native lifetime evidence was re-read from the digest-pinned host G17P driver
`666816a37045b6dcd351b63ad56a1f5ce284f3acb2157262226e090ade9cfca2`:

- `finishRound` at `0x216b56df8`: `0x216b57f70` tests the two input-mask bits;
  `0x216b57fcc` sets w24 to zero on an empty mask, then branches to the
  ordinary-copy path at `0x216b57ff8`. Empty inputs do not skip that copy.
- `0x216b581d8` ORs w24 into the lifetime condition, `0x216b581dc` inverts it,
  and `0x216b581f4` stores the result in COPY object field +0x18. The ordinary
  COPY emitter is called at `0x216b58238` (target `0x216b61fd8`).
- `0x216b58258`/`0x216b5825c` preserve the high source metadata and replace
  its availability value with 8 (already ready). Only then does
  `0x216b58264` skip the optional input copy when w24 is zero.
  `0x216b582dc` calls `copyGprToCprog` at `0x216b59054` when required.
- Fresh CPU oracle calls with ready source value 8 give `(offset, mask, GPR
  half)` `(0,1,4)` = `43040f00220000140000`; `(0,3,4)` =
  `4b050f00220000140000`; `(2,3,6)` = `43060f00260000140000` followed by
  `53060f002a0000140000`. These bytes are pinned in the new loader fixtures.

Both optional emission callbacks must be present. Bounds include only selected
input halves, and emitted records participate in the existing transaction.
Incomplete metadata, unsupported allocator modes/targets, missing uploaded
inputs and duplicate uploads of selected inputs fail closed. This opt-in path
does not silently substitute a larger buffer or extra synchronization.

Verification:

- Host UBSan: load packer **441 checks** and uniform program **484 checks**.
  Exact-byte fixtures cover empty, sparse, aligned-pair and odd packed-pair
  copies, source lifetime, and an earlier load-only finish. They verify that
  address setup, LDDEV and existing round NOPs remain unchanged.
- Transaction tests include capacity rejection and failures after an ordinary
  copy was emitted. Logical pending state and committed bytes remain intact.
- Program tests cover out-of-order ranges, source half 511, missing/duplicate
  input coverage, output preservation and a real allocator rollover using
  initial GPR count 92. Limiting workspace capacity alone does not request a
  rollover; the first draft of the fixture incorrectly assumed it did.
- Full Mesa and all relevant CPU checks passed in the Linux builder. Existing
  load-core 3,326, preshader 724, program-core 913, precompiled-fill 487 and VDM
  79,622 checks still pass. Compiler and graphics suites pass. Honeykrisp's
  focused preamble checks pass before its unchanged packed-integer baseline
  failure; the entire Honeykrisp suite is not claimed to pass.

Successful receipt logs are `preamble-input-loader-v2-*`. Candidate
`candidates/preamble-input-loader-v2-candidate.so` has SHA-256
`faea8b5f4494a4a80bd17ba4093dcaa8a270c3e91169994c62272f16876227e3`.
All 30 overlaid files, ten test executables, and the original baseline Mesa
library were verified restored by the v2 run. The earlier interrupted build
temporarily left differing builder artifacts; its saved baseline Mesa was
restored and the v2 baseline rebuild reproduced every prior baseline hash.
This recovery concerned the isolated build VM only.

The candidate remains uninstalled and Gallium still disables G17P preambles.
No graphics/compute runtime caller supplies this optional input descriptor yet.
Compiler input-read lowering, input/pressure storage accounting, dependency
import, implicit external entry state, stage qualification and efficiency
measurements remain required. The live CTS driver and original dirty source
trees were not changed.

## Incoming producer ledger: `8ec7c5b8bee`

`agx3_slot_assign_with_live()` imports disjoint pending-result token ranges into
the existing scoreboard algorithm. This is the missing representation for
loader-published TPS inputs: the producer exists before the shader instruction
list begins. RAW and WAW checks now apply to an imported producer through the
same ledger as in-shader producers. Waiting its slot once satisfies the other
inputs from that generation. Reusing the slot later creates a fresh dependency
without leaving old input aliases attached to the new result. The old API calls
the new function with no imported ranges and preserves its existing behavior.

Host UBSan and Linux tests pass **1,137 scoreboard checks**, including every
half of the 512-half input domain, native two-RLD pointer entry, disjoint TPS
ranges sharing a producer, GPR/TPS separation, incoming WAW, transient slot reuse,
generation reuse and rejection of invalid/overlapping metadata before outputs
change. Full Mesa and all relevant CPU suites pass; the known Honeykrisp
packed-integer failure is unchanged. All 33 overlaid files, eleven CPU test
binaries and baseline Mesa were restored byte-for-byte.

Candidate `candidates/preamble-input-slots-candidate.so` SHA-256:
`e8dd8480acd1dc5ad05d6244243a2f973e76f96a4541ebd6204b8aa7d98fef23`.
It is not installed. This adds an opt-in dependency model, not compiler input
lowering or runtime enablement.

## Expanded entry witnesses and rejected fixed policies

Five additional offline captures with 5, 6, 7, 9 and 12 buffer pointers are in
`native-preamble-entry-window-host27`. All original and additional source/native
hashes and native schema identities were checked by
`native-cprog-entry-window-verification.json`: **22 captures, 164 entry reloads**
plus 33 native copy-helper cases. The added **input halves / frame bytes** pairs
are 20/64, 24/64, 28/80, 36/96 and 48/112. They agree with the packed-frame formula.

The RLD structural flag is not a generic first-instruction flag. In 1–6-buffer
captures it is on reload 0. In the larger captures it is later (zero-based):

| Buffer pointers | Flagged reload | Byte offset | GPR destination half | TPS half |
| --- | --- | --- | --- | --- |
| 7 | 9 | 40 | 10 | 26 |
| 8 | 11 | 48 | 10 | 30 |
| 9 | 13 | 56 | 10 | 34 |
| 12 | 19 | 80 | 10 | 46 |
| 16 | 27 | 112 | 10 | 62 |

Those later flagged reloads have no loader wait, whereas the first reload still
waits slot 6. The flag's meaning is unresolved; this rejects coupling it to the
first incoming dependency. Do not implement an index formula from this table.
The result-slot policy also varies: the 16-buffer entry uses slots 2, 6, 3 and 4.
Assigning every input reload to slot 2 does not reproduce that native schedule.

`probes/verify-native-cprog-slots.py` compiles a bridge to the production ledger
and reconstructs each native reload's GPR/TPS read/write tokens. Its receipt,
`native-cprog-slot-verification.json`, matches **all 164 computed wait masks**
given the native result-slot choices. Those slot choices are replay inputs,
not independently derived policy. Structural flags and slot-mirror policy are
outside this comparison. The initial hardcoded slot-2 replay was rejected by
the 16-buffer witness and was corrected to state the narrower comparison.

No entry-state heuristic has been enabled. Compiler lowering, storage ownership,
native scheduling/structural policy, and real hardware qualification remain open.
