# GL 3.0 CTS coverage priorities

Historical investigation before the DJ AGX3 update. The newer full-run result
is recorded in `DJ-AGX3-RETEST.md`: 40,599/43,901 Pass with no GPU fault.

## Result

Depth wrapping and shadow LOD together account for 147 of the 200 observed
Fail results (73.5%) in the last full official run. They are the highest-value
targets found so far, not proven quick fixes. No additional official case has
been made to pass in this investigation.

| Priority | Target | Unique failing cases | Observed failing executions | Assessment |
| --- | --- | ---: | ---: | --- |
| 1 | Depth texture wrapping | 36 | 108 | Two depth formats, two NPOT sizes, three levels and three wrap modes. All 126 colour-format cases in this family passed in reached configurations. Diagnose the shared depth-sampling/upload path before individual wrap modes. |
| 2 | Shadow LOD compilation | 12 | 36 | Concrete encoder refusals for array/cube bias and explicit LOD forms. Fresh Apple compiler witnesses are captured; operand layout and runtime correctness still need qualification. |
| 3 | Cube-array shadow rendering | 1 | 3 | Compiles but fails image comparison. Keep separate from the twelve compiler refusals. |
| 4 | Vertex texture LOD | 1 | 2 | Compiler rejection fixed; official bias case now reaches rendering and fails. Small source correction, no newly passing CTS case yet. |
| Later | Transform feedback and clip distance | Several | 41 combined | A missing GS-copy runtime program and additional compiler/runtime gaps make this a larger implementation task. |

The 36 depth cases occupy 1,800 scheduled executions across all 50 main
configurations; the 12 shadow compiler cases occupy 600. These are affected
schedule sizes, not promised pass increases. Counts above exclude InternalError
and ResourceError results.

## Retained implementation and validation

`mesa-j700/src/asahi/compiler/agx3_pack_binary.c` now admits a register-valued
explicit LOD in vertex shaders through the existing validated sampler encoder.
Compute-stage restrictions remain intact. The real NIR vertex regression uses
`float(vertex_id & 3)` so the LOD cannot disappear into a constant preamble.
It fails before the correction and passes after it. No additional waits,
draw splitting, capability reductions or official CTS modifications were used.

Apple native evidence: `native-vertex-lod-register/vertex-colour-lod-per-vertex/`.
Its receipt identifies vertex code `000c00-4-__TEXT-__text.bin`, with PACK_SMPIR
at 0x84 followed by SMP at 0x98. The native compiler was run offline; this is
encoding evidence, not Apple GPU execution evidence.

Official hardware evidence on healthy boot
`f80b3c8f-a9e7-4fe1-9024-1e03dcd756a4`:

- `quick-vertex-lod-before`: bias case fails vertex shader compilation.
- `quick-vertex-lod-after`: compilation succeeds; vertex and fragment loops
  each stop on their first failing bias comparison (black versus red). The
  log's two failures do not mean the remaining bias combinations passed.
- `quick-lod-selection-after` and `quick-lod-restored-unique`: official
  `KHR-GL30.texture_lod_basic.lod_selection` passes.
- None of these runs reported a GPU fault.

Final Asahi/poly CPU suite: 264 pass, 2 expected failures, 1 unchanged
pre-existing `asahi-g17p-precompiled-fill` failure. See
`quick-wins-final-regressions.log`.

## Experiment rejected as insufficient

Fresh native `2d-three-references` code uses SMP coordinate banks 0, 8, 16.
A bounded experiment allocated independent comparison staging by scoreboard
slot and extended staging metadata. Seven shadow packer checks passed, but
the official depth-wrap case still failed (`quick-depth-wrap-banks`) without a
GPU fault. Both production and test changes for that experiment were removed.
It did not prove that staging reuse was the depth-wrap root cause.

The earlier unfinished TF regression did not reproduce the intended missing
runtime-program failure. Its additions are preserved in
`deferred-xfb-regression.patch` and removed from the active suite until refined.

## Current state and next investigation

Neo is on the same healthy boot with the retained vertex-LOD candidate:
SHA256 `06b40609140b093f9c86af1c85f95a62ee4b413e70fb4df44f37b0000f374d38`.
The RAM kernel module remains `aa5b21014671517849cfa12c7415cd5b39f565283c7140f3b3ebaa367e96fb4b`.
Resident m1n1, SSD partition layout and the other Mac were not changed.

Next: capture actual depth-wrap pixel values and compare single versus three
references, with the same depth texture and sampler state, to distinguish
upload/format/compare-state errors from shader execution. Then qualify the
array/cube LOD operands using the captured native witnesses and official cases.
Reuse the healthy boot and Xorg for focused tests; rerun the full suite after
a fix recovers cases. Do not count focused runs as a new full-run total.

The full-run baseline remains 2,413 Pass out of 43,901 scheduled main
executions. Configuration checks are separate. One mistaken diagnostic case
name (`quick-lod-basic-after`) selected zero cases and supplies no pass evidence.
The first restore attempt hit the installer's existing-backup guard; a unique
candidate filename preserved that backup and restored successfully.
