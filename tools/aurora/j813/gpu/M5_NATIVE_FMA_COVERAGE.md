# M5 native FMA and GLES coverage

## Hardware change

G17G now uses native fused multiply-add instructions with a minifloat
addend, including absolute and negated multiplicands, and with independent
minifloat multiplier/addend constants. The existing shared encoder preserves
post-register-allocation source lifetimes and all eight M5 dependency slots.
The G17P encodings and byte vectors remain unchanged.

The operation remains fused with one rounding. There are no extra temporary
constant registers, blanket barriers, draw splitting, software fallbacks,
CTS changes or tolerance changes.

## Independent reference and regression evidence

`m5-fma-addend-reference.metal` and `.mtlp-json` contain eleven native Apple
reference programs. Metal Toolchain 27.1.5194.15 targets `applegpu_g17g` and
macOS 26.0. The native binary SHA256 is
`1572d9c5c8985adbe6fc6b8389c6ca8b6706d6ae8a31bc4d740088e4f16a3d10`.

All 44 FMA instructions independently decompose and reconstruct byte for
byte. They cover live/dead operands, positive/negative constants, source
ABS/NEG, compact instructions and the extended slot-8 wait form. Mesa tests
retain these Apple bytes and reject invalid registers/lifetime combinations.
The FALU, ALU, transcendental and AGX3 packing suites pass.

## Coverage selection

A compiler-only survey examined 27 groups containing 17,038 catalog entries.
The report records incomplete/aborted families; this is not 17,038 completed
tests or hardware passes. FFMA was the first observed opcode refusal in
2,992 cases. The implementation reduces cases with compiler/profile errors
by 2,249 in the same survey. Remaining failures include later instructions
revealed by the new FMA support.

The native candidate batch contains 8,878 unchanged tests:

| Group | Cases |
| --- | ---: |
| Full previously passing clear/blend/exponential regression | 2,435 |
| Arithmetic, shifts, min/max and geometric shader groups | 4,152 |
| State queries outside the still-blocked shader-query group | 904 |
| Invalid implicit shader conversions | 768 |
| Shader keyword tests | 356 |
| Preprocessor groups | 148 |
| Shader declarations | 54 |
| Implementation limits | 49 |
| Negative shader tests | 12 |

Only complete compiler-covered child groups are selected. Every excluded
case remains in the 54,799-case catalog without pass credit. This is a
development selection using seed 1 and a 64x64 RGBA8 pbuffer, not a full GLES
conformance run. Results require native M5 renderer/library provenance,
unmodified QPA results and a complete error-free kernel stream.

Candidate library SHA256:
`f0bb18c8f87fec779e527807220ce4fb37b532c82ffbe47be3d5bf16e570a8c4`.
Case list SHA256:
`6df90b90b3ce2543e868ec7ce0ef72d424a742b2d65c5cc0c84d1311a870f57f`.

## Native hardware result

The physical M5 completed all 8,878 cases: **8,734 Pass and 144 Fail**.
All 2,435 baseline cases pass with identical per-case submission draw counts.
The new selection adds **6,299 passes**, including 4,008 shader-operator
cases and all 2,291 new language/API cases. These include already implemented
features newly tested on hardware as well as paths enabled by the FMA fix.

The run took 646.157 seconds and peaked at 44.974 degrees Celsius with the
CPU0 frequency cap and thermal protection active. All 11,837 submission
generations retired both queues. The 23,700 consecutive kernel records have
no gaps, overruns or driver errors; there was no timeout or thermal abort.
The run contains 24,319 draws and 9,532 submissions with multiple draws.
The full campaign ledger is **8,734 Pass, 144 Fail, 45,921 NotRun** out of
54,799. This remains development coverage rather than conformance qualification.

The exact native report and raw logs, including every failure, are retained:
`m5-native-gles3-fma-coverage.json`, `.qpa.gz` and `-kernel.log.gz`.
The uncompressed QPA SHA256 is
`fef171e26ecc36f693eedccbc88da75d7ffdbf18a2318f0d40598ff4af4b859f`.

All 144 failures are image mismatches in scalar-first integer/vector add,
AND, OR and XOR: 36 cases per operator across signed/unsigned types, vector
sizes, precision levels and vertex/fragment stages. Corresponding scalar
operand reuse/lifetime handling is a focused next investigation; the cause
has not yet been established by a corrective hardware run.

## Next shared compiler gaps

The updated survey's leading first-opcode refusals are floating comparison
and selection (1,813 cases), integer multiply-add (608), conversion (481),
reciprocal (434) and bit operations (433). These are investigation priorities,
not promised pass gains: a later instruction or rendering issue may become
visible after the first blocker is removed.
