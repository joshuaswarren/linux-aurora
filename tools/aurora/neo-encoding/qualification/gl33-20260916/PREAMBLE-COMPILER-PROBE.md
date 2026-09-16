# CPU compiler comparison: uniform-address preamble

This probe runs on the build VM, not on the Neo. It changes a temporary compiler
test source only; the live GL33 campaign's Mesa and kernel remain unchanged.

## Workload and results

The existing `check_preamble_compute` workload loads a uniform 64-bit output
pointer from a root pointer, then writes the invocation index to the corresponding
output element. The constant load permits speculation. Each compilation uses a
fresh instance of the same NIR, with compiler preamble formation enabled or
disabled. The comparison includes the AGX2 backend used for the M1/M2 design
baseline and the G17P AGX3B backend.

| Encoding | Preamble | Main bytes | Main GPR count | Total binary bytes | Push halves | Preamble GPR count |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| AGX2 | Disabled | 40 | 8 | 40 | 8 | 0 |
| AGX2 | Enabled | 30 | 2 | 164 | 12 | 4 |
| G17P / AGX3B | Disabled | 36 | 8 | 36 | 8 | 0 |
| G17P / AGX3B | Enabled | 22 | 2 | 164 | 12 | 4 |

All four compilations succeeded. Both preambles begin at offset 128, leaving a
36-byte tail in this particular no-rodata fixture. Main and preamble scratch
sizes are zero. Alignment makes the total split binary larger even though the
per-invocation main program is smaller. These are compiler metadata/code-size
measurements, not execution-time or occupancy measurements. In particular, the
GPR-count change does not establish a fourfold speed or occupancy improvement.

G17P preamble bytes:

```
67005400000000001900004002000b0107010020008000004b0001045b0201040e000000
```

Artifacts:

- `build-preamble-cpu-probe.py`: guarded source-overlay build and restoration.
- `probes/preamble-cpu-overlay.c` and `.patch`: exact diagnostic source.
- `preamble-cpu-probe-results.jsonl`: complete metadata and binary bytes.
- `preamble-cpu-probe-build.log`, `preamble-cpu-probe-stderr.log`.

## Restoration and validation

The first invocation stopped before mutation because the helper expected
`main(void)` while the test uses `main(int argc, char **argv)`. That was corrected.

The comparison completed successfully on the next invocation, but its final
pre-existing executable-hash assertion failed. The source was restored exactly:
SHA-256 `9613141b313e5189573a50a9d97f30c28ab6b4fca73e80400da7ae7565729141`.
The restored executable rebuilt to
`07251d81e15821938cb0e0e090f2c5cce4ac088fc4b9cd3d4e7aec5d1c6c297a`,
different from the previously present test executable. A second forced rebuild
of the unchanged original source reproduced that same new hash. The complete
restored compiler test then passed; see `preamble-restored-compiler-tests.log`.
The helper now forces an original-source baseline rebuild before recording the
executable hash for future comparisons.

The qualified builder's Mesa library remained exactly
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
No candidate library was built or installed by this probe. The Neo continues to
use the already-qualified cache-identity library.

## What still needs proof

The compiler records `preamble_offset` and `has_preamble`, but no explicit
preamble byte length. Deriving length from the tail happens to work for this
fixture; runtime code must establish an unambiguous segment boundary for general
binaries. It must also preserve any constant-data sections and relocations.

The native Ioto suffix packer expects compiled constant-program bytes and can
inline them or branch to an external program. Its CPU packing tests do not prove
that an arbitrary compiler preamble has the correct execution/resource contract.
Required next evidence includes register and uniform-file handoff, the native
prelude-selection rules, resource lifetimes, and completion semantics before the
main shader executes. The no-preamble guard remains in the live driver.

The pinned kernelcache reader has no defined symbol containing
`ConstantCalculation`; that lookup provided no additional handoff evidence.
The repository has a saved G17G user-mode Metal binary, but its filename alone
does not establish a J700 constant-program contract. Do not substitute a guessed
handoff or an added compute dispatch/barrier for the native execution path.
