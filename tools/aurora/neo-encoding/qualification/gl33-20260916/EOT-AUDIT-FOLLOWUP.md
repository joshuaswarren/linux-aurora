# EOT audit follow-up — 2026-09-13

Two findings from IMPLEMENTATION-AUDIT.md have advanced: the native EOT recipe
is now reproducible from repository files, and the incorrect mixed-format EOT
has been replaced using independent compiler evidence. These are source and
CPU validation results. The candidate is not installed or hardware-qualified.

## Mixed-format correction

The admitted RGBA16F/R8 layout has offsets0/8 and sample stride16, verified in
`agx_g17p_classify_mrt`. Native UMD evidence already established that an EOT
partition is the physical offset divided by sample stride. Both attachments
therefore belong to partition zero.

The audit first reproduced the existing 238-byte EOT exactly using native
internal formats8/3, components4/1, mask3 and partitions0/1. Its text SHA256
is `eef32d3c9b7bca4de2efba861662e32c34f49af5877dabce0ad9da5cd50d5173`.
Changing only the second partition to zero produces 120 bytes with SHA256
`b3191cbbd673db5fb00d3e744f60ed59befad9bc19a2bf30aa9420effa63df12`.
The original and corrected input files differ in exactly one byte. This
preserves the existing format interpretation while correcting the independently
proven physical partition rule.

The code in `agx3_pack_binary.c` now emits that native witness. Its metadata
needs three GPR dwords instead of four; the existing conservative eight-half
allocation remains, and staging stays at16halves. The compiler still requires
the complete supported NIR/IR shape before selecting the builtin. No draw
splitting, extra synchronization or application-specific dispatch was added.

The loader now accepts the corrected 120-byte mixed program only with one
sample, stride16 and offsets0/8. The two-RGBA8 legacy contract requires stride8
and offsets0/4. Explicit compact masks also require the derived stride and
single-sample layout, so malformed metadata cannot fall through to a legacy
shape. The old 238-byte program is rejected for the mixed compact layout.

The new expected compiler bytes came from the native compiler, not Mesa's
output. Both affected tests failed against the old implementation, then passed
with the correction. See `mixed-eot-red.log` and
`mixed-eot-fixed-regressions.log`. The code-size reduction does not establish a
runtime speedup.

## Repository reproduction

New source files under `src/asahi/compiler/test/probes/`:

- `g17p-mrt-eot.json`: nine canonical recipes and pinned input/text/metadata
  expectations, including the seed digest and toolchain/platform identity.
- `capture-g17p-mrt-eot.py`: standard-library-only capture, validation and
  header regeneration. Only recapture needs the Apple offline compiler;
  validating existing captures does not need a GPU or a compiler invocation.

The script regenerates eight RGBA8 counts plus the mixed variant and retains
native input/object/section/command receipts. Validation checks the actual
native object, the GPR and staging quantities, both ESL argument ranges,
kind5 compact attachment offsets and the RT store mask. It compares the
resulting header and mixed production/test arrays to native output. Full
commands are in README-g17p-wide-mrt.md.

Fresh captures are in `reproduced-eot-audit-v2/`. All nine native input and
text digests reproduce. `reproduced-eot-source-red.log` records rejection of
the old mixed source; `reproduced-eot-source-green.log` records the corrected
source matching all nine native programs and resource contracts.

The first capture attempt stopped because the validation script read the RT
store mask from the outer load-metadata table. Inspecting the actual table
showed that it belongs to the common table referenced by outer field0. That
parser defect was fixed and a new capture directory was used. The failed
attempt remains preserved; it was not mistaken for a compiler/GPU fault.

## Validation and remaining work

`mixed-eot-fixed-build.log` is a successful full build. The complete Asahi/poly
run is **264 OK, 2 ExpectedFail, 1 Fail**. Compiler/graphics-program, MRT epilog,
tilebuffer, shared Honeykrisp and related tests pass. The remaining
`asahi-g17p-precompiled-fill` failure is its existing robust-index size/hash
golden mismatch:9/382 checks fail across three variants. Those expectations
were not changed or waived.

No hardware test, Neo installation, reboot, resident modification, branch
commit or push occurred. The official CTS ledger remains the earlier
807Pass/879reached result with43,022NotRun. No additional coverage is claimed.

The repository reproduction gap is closed for these nine builtins. A general
EOT lowering, missing MRT format/blending/mixed-operation behavior, disabled
G17P preambles, the robust-index failure, efficiency measurements and full
official hardware conformance remain unresolved. This follow-up does not
supersede those limits in the original audit or justify marking the goal
complete. Hardware testing remains paused for the audit.
