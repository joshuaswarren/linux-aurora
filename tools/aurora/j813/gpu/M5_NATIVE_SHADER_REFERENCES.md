# Native M5 shader references

These small Metal kernels reproduce instruction forms encountered while
compiling real Zink `glxgears` shaders for M5. They run through Apple's offline
compiler on the host; they do not boot macOS on the target or submit GPU work.

The 2026-09-12 witnesses used Metal Toolchain 27.1.5194.15 with an explicit
macOS 26.0 deployment target and `applegpu_g17g`. Pin both the Metal frontend
and native compiler deployment versions: the default SDK emitted AIR 2.9,
which the native platform-26 pipeline rejected in favor of AIR 2.8.

From this directory, with an installed Metal toolchain:

```sh
shader_output=$(mktemp -d /tmp/m5-native-shaders.XXXXXX)
for name in m5-half-compare-reference m5-popcount-reference m5-add64-reference \
            m5-float-scale-reference m5-vertex-varying-reference; do
    xcrun -sdk macosx metal -mmacosx-version-min=26.0 -c "$name.metal" \
        -o "$shader_output/$name.air" || exit
    xcrun metallib "$shader_output/$name.air" \
        -o "$shader_output/$name.metallib" || exit
    xcrun metal-nt -arch applegpu_g17g -platform_version macos 26.0 26.0 \
        -N "$name.mtlp-json" "$shader_output/$name.metallib" -save-temps \
        -o "$shader_output/$name.gpubin" || exit
done
```

The native container has Mach-O `__text` sections. Do not interpret bytes at
a previously recorded file offset without locating those sections again:
adding a pipeline changes the container offsets. The existing disassembler's
GPU1 operand rendering is incomplete; instruction boundaries and raw bytes,
plus the independent AGX3c composer, were used for these comparisons.

## Evidence represented in Mesa tests

- `m5-half-compare-reference`: four half-result EQ/NE-to-zero instructions,
  then three word-result selections using half comparands. The latter cover
  immediate/register, register/immediate and register/register selections.
  Native mixed selections with half comparands keep field 11 clear; field 28
  is set. Four further EQ-one vectors cover mixed selections, register pairs,
  and a boolean. Eleven exact half-comparison byte vectors are in
  `test-agx3-isel-core.c`. Other conditions and comparands above one remain gated.
- `m5-popcount-reference`: twelve native `cbs` instructions cover uint and
  ushort results, odd/even half registers, a slot-8 wait, discarded inputs,
  and live uint inputs/cached destinations. Exact bytes are in
  `test-agx3-alu-core.c`; unsupported waits and half-input cache forms remain
  rejected by the encoder.
- `m5-add64-reference`: Apple lowers full-width unsigned addition to low-word
  add, carry comparison and high-word addition. M5 uses the existing NIR
  64-bit-add lowering, covered by `test-agx3-int64.c`. This does not qualify
  a native `iadd64` instruction or hardware execution of the result.
- `m5-float-scale-reference`: eight native floating-point scale instructions
  cover constant multiplication, a slot-eight dependency wait and retained or
  discarded sources. Exact byte checks are in `test-agx3-alu-core.c`.
- `m5-vertex-varying-reference`: two render pipelines emit position plus one
  float4 color, with and without point size. The vertex `__GPU_LD_MD`
  FlatBuffer's root.field1.field3 is 8/9 and field4 is absent, matching the
  all-f32 output footprint used by `hk_agx3_g17g_vertex_program_info`.
  The VDM tests validate both profiles; fragment coefficient state and hardware
  interpolation are separate requirements.

Saturated floating adds additionally have four AGX3c composer vectors in
`test-agx3-alu-core.c`. Those are independent encoding checks, not native
Apple-compiled execution witnesses.

These tests establish code generation and byte agreement. GPU execution,
readback correctness and application presentation are separate gates.
