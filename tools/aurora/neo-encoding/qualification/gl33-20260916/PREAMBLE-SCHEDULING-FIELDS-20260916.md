# Native RLD scheduling-field provenance

The RLD field currently called `structural_flag` in the isolated Mesa candidate
comes from native scheduling metadata: bits 36:37 of named operand `0x4b`.
It is field 41 for compiler encoding family 0, and field 44 for families 1–3.
It is separate from dependency waits, which occupy metadata bits 24–31 and
map to fields 30–37. This establishes the field's provenance, not the hardware
meaning of its values or the policy for placing it.

No Mesa source, installed library, kernel, or Neo state was changed by this
investigation. Runtime preambles remain disabled. The full production CTS
campaign continues independently on boot
`94c3f079-27bc-456d-93a2-5db1e7ed2171`.

## Reproducible verification

Run from the Aurora workspace:

```sh
neo-linux/artifacts/j700-gl30-cts-20260913/.native-oracle-venv/bin/python \
  neo-linux/artifacts/j700-gl30-cts-20260913/probes/verify-native-rld-scheduling.py \
  --output neo-linux/artifacts/j700-gl30-cts-20260913/native-rld-scheduling-field-verification-20260916.json
```

The check emulates only qualified ARM64 leaf helpers, with read-only native
pages and bounded control flow. It executes no GPU or operating-system code.
The pinned compiler is the host Metal toolchain's `libapplegpu-nt.dylib`:

- Full binary SHA-256: `eb941548121e8e962a7bc9acd2f80d613017601ce967ecf1ae27fb14079ec683`.
- ARM64 slice SHA-256: `b29231c242a8c5d738aaad50d20e906979731c87f829c93dfe0c56d5dc079175`.
- Independent named mapper: host macOS 27 `AGXMetalG17P`, SHA-256
  `666816a37045b6dcd351b63ad56a1f5ce284f3acb2157262226e090ade9cfca2`.

Result: **2,464 comparisons passed** (616 metadata values across four encoding
families). The complete 163-word field-map output agrees between the compiler
and the independently named driver RLD mapper in every comparison. Tests cover
all four values of all four scheduling fields, every single metadata bit, and
combined wait masks. Wait fields, flag bit 6, static TPS source selection, and
the complete scheduling-field array are checked explicitly.

Result digest: `d042d8ff541ed633f605288f5071fc98d7d2df1091983c8a62b2b5ed371f924b`.

## Exact native chain

Compiler addresses are ARM64 slice virtual addresses, starting at zero.
Driver mapper addresses belong to the separate pinned driver image.

1. The generated converters at `0x383c3c`, `0x383f58`, and `0x3840dc`
   write typed instruction kind `0x91`. Each passes RLD object offsets `0x70`,
   `0x44`, and `0x50` to common helper `0x381198` for flag, waits, and hints.
2. The dispatch helper at `0x3e1c18` returns
   `table[typed_kind]`, using table address `0x48f7b90`. Entry `0x91` is the
   pinned chained-rebase word `0x1000000040caac`, identifying the mapper at
   `0x40caac`. The verification checks the exact word; it is not a general
   Mach-O fixup decoder.
3. Common helper `0x381198..0x381254` extracts the metadata. Family 0 stores
   bits 32:33, 36:37, 40:41 at RLD offsets `0x50`, `0x54`, `0x58`.
   Nonzero families store bits 32:33, 36:37, 40:41, 46:47 at offsets
   `0x5c`, `0x60`, `0x64`, `0x68`.
4. Mapper `0x40caac..0x40cda0` expands each of those 32-bit typed values into
   its 64-bit field-map entry. In particular, `0x54` maps to field 41 and
   `0x60` to field 44.
5. The independent driver symbol
   `llvm::AGX3::mapInstrSpecFields_rld` at
   `0x2168171dc..0x216817540` implements the same mapping. The functions are
   not byte-identical; the emulated full-output comparison verifies the
   relevant cross-version layout rather than assuming it.

The upstream native pass registration is separately traceable from the string
`agx3-sched-hints` at `0x450cd5f`, beside
`AGX3 Clique Scheduling Hints Placement` at `0x450cd38`. Registration at
`0x329e9c` installs factory `0x329f1c`; the factory's vtable points to pass
body `0x32a0ec`. That body assigns hint groups at metadata positions 32, 36,
and 40. Its transition and initial-placement helpers (`0x32b0a4`,
`0x32b360`) update two-bit fields through named operand `0x4b`, selecting
values 1 or 2. The pass uses instruction-group counts and cost thresholds;
the precise policy has not yet been reconstructed or independently tested.

## Consequences and remaining work

The previous fixed-first-RLD or fixed-result-slot hypothesis is unsupported.
Native captures put the flagged RLD first for one through six buffer pointers,
but later for seven or more; scheduling policy is now the concrete lead.
The current C `bool structural_flag` does not express the full two-bit metadata
domain. The verification accepts values 0–3 in the typed field map; this does
not establish that all four are legal packed instruction encodings.

The separate offline probe `probes/probe-rld-scheduling-values.c` tests the
existing generated encoder tables directly, bypassing the boolean descriptor.
For the same static TPS reload baseline in both encodings, compact `ep0`
accepts only hint 0; extended `ep1` accepts 0, 1, and 2 and rejects 3.
Results are preserved in `rld-scheduling-generated-table-values-20260916.jsonl`.
For AGX3b the extended bytes are `0488192000000000` (0),
`0488192060000000` (1), and `04881920c0000000` (2). For AGX3c they are
`248021102401a102`, `248021104401a102`, and `248021100401a102` respectively.
This identifies a representational gap in the typed boolean; it is generated
table evidence, not an independent native-encoder or hardware comparison.

Before enabling preambles:

- Verify native packed encodings for scheduling values and their hardware
  interpretation; do not silently truncate them to a boolean.
- Recover the relevant placement policy and check it against complete native
  instruction streams, including the seven-pointer transition.
- Keep dependency waits and result-slot lifetime accounting independent.
- Qualify actual resource handoff, output correctness, and performance on Neo
  after the current CTS run, using the prepared baseline/candidate probes.

Attempts to obtain native pass dumps with `-print-before=agx3-sched-hints`
and `-print-after=agx3-sched-hints` produced no dump for the one- and
seven-pointer captures. Their code and source bytes match earlier captures.
Successful compilation does not prove those diagnostic options were honored.
`metal-objdump` recognized AGX3 objects but reported no instruction printer;
none of those attempts produced a usable native assembly listing.

Evidence: `native-agx3-sched-hints-region-export.json`,
`native-preamble-scheduling-pass-20260916/`, the two inspector scripts, and
`native-rld-scheduling-field-verification-20260916.json`.
