# Compiler-generated external preamble regression

Isolated Mesa commit `a3f2a8616a9`, on `j700-preamble-20260915`, extends the
compiler regression beyond the earlier 36-byte preamble. No candidate from
this branch is installed on the Neo; the full GL33 campaign remains unchanged.

## Workload and checks

The compute fixture reads 32 runtime uniform values from a constant buffer,
combines them through a dependent integer multiply/XOR chain, and combines
the result with each invocation index before storing it. The calculation
cannot be folded to a compile-time constant. It generates a real preamble
larger than the native 256-byte inline threshold without padding the fixture.

The same NIR workload is compiled with preambles enabled and disabled on AGX2
and G17P. In addition to presence and exact extent, the regression checks that
main and preamble ranges fit the binary and do not overlap, that the large
preamble and its main need no scratch, and that compact metadata preserves
the extent. G17P checks the END instruction at the preamble boundary.

Results from `preamble-external-candidate-agx3_compile_fail_closed_test.log`:

| Encoding | Preamble | Main bytes | Preamble bytes | Main 16-bit registers | Preamble 16-bit registers |
|---|---|---:|---:|---:|---:|
| G17P / AGX3B | disabled | 840 | 0 | 16 | 0 |
| G17P / AGX3B | enabled | 36 | 834 | 4 | 16 |
| AGX2 | disabled | 560 | 0 | 16 | 0 |
| AGX2 | enabled | 36 | 558 | 4 | 16 |

Register units follow `agx_shader_info`: these are 16-bit registers, not
32-bit register pairs. These numbers describe generated code and register
allocation. They are not timing, occupancy, output-correctness, or hardware
preamble results. This test does not execute the shader or external branch.

## Validation and restoration

Command from the workspace root:

```
python3 neo-linux/artifacts/j700-gl30-cts-20260913/build-preamble-size.py --prefix preamble-external
```

`preamble-external-host.log` records baseline, candidate, and restored passes
for the complete `agx3_compile_fail_closed_test` and the 487-check
`asahi-g17p-precompiled-fill` test. Candidate builds produced no warnings or
errors. The shared builder's three source files and two test executables were
restored byte-for-byte. Builder Mesa remained SHA-256
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
`git diff --check` passed before commit.

The earlier broader Honeykrisp tile-layout failure was not rerun or resolved
by this change; it remains documented in `PREAMBLE-NATIVE-AND-SIZE-AUDIT.md`.
The next hardware work still needs to validate preamble input state and the
ordinary inline/external handoff after the active full CTS campaign ends.
