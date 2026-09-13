# M5 native typed shader arithmetic

## Compiler and rendering changes

The G17G backend now encodes scalar-half and mixed half/float LOG2 and EXP2,
half/float reciprocal square root, and the native square-root step. Square
root keeps Mesa's step followed by multiplication by the original input;
it is not replaced by an ordinary reciprocal-square-root approximation.
The multiply supports a float step with a half input and a half result.
Float multiplication by a minifloat constant can also write a half result.

The compiler emits a single native FMA for a float input multiplied by a
minifloat constant plus a register addend. Both input lifetimes, the addend
sign and all eight dependency slots follow the post-register-allocation
instruction. G17P retains its existing encodings. Half LOG2 no longer needs
the earlier input-widening move when its native mixed-width form applies.

The HAL300 draw setup now preserves actual UVS slot counts and CF binding
offsets for up to two vec4 outputs. Scalar and subvector fragment inputs
were previously rejected by a float4-only state template. The existing
Asahi varying linker supplies the VIT; its interpolation slot count covers
all exported vertex slots, including components a particular binding skips.
The component count, CF binding count and total vertex output size are
separate quantities. The `Varying Counts` and `Output Select` field meanings
match the existing Asahi command schema and linker.

No CTS shader, precision requirement or comparison tolerance changes.
There is no draw splitting, unconditional execution barrier or software
rendering substitute. Kernel context-lifetime guards remain in place.

## Native Apple reference

Metal Toolchain 27.1.5194.15, `applegpu_g17g`, macOS deployment target 26.0:

- `m5-half-transcendental-reference.metal` and `.mtlp-json`: 17 pipelines
  covering retained/last-use half LOG2, EXP2 and RSQRT; float RSQRT;
  half/float square root and absolute-input square root; narrowing multiply;
  and all half/float LOG2/EXP2 input/output combinations.
  Native binary SHA256:
  `92c2ae9a0cb579dc199c66153938dc0aa784d6da8a797b4bd3944a8aa182aeae`.
- `m5-typed-fma-reference.metal` and `.mtlp-json`: five pipelines covering
  FMA with constants 0.5 and 2.0, either input retained, and a negative
  register addend. Native binary SHA256:
  `4273be4fa1a217b6ca14461bc2c3ad8a6358dd79daf6575b8e8828165c624ac6`.

Independent field decomposition reconstructs all 64 unary and 20 FMA
instructions byte-for-byte. Four compact square-root multiplies and four
narrowing multiplies also reproduce Apple output. Mesa adds 28 unary,
10 FMA and 8 multiply byte vectors plus strict type-domain refusals.

Unary width fields are f10 (destination) and f26 (input): 1 is a scalar
half and 2 is a float pair. Lifetime fields remain f11/f27/f29 and wait
fields f35..f42. Native RSQRT uses opcode f34=66; SQRTSTEP uses f34=68.
Mixed-width RSQRT, half rounding and half-result SQRTSTEP remain refused.

## Validation status

All 112 unchanged functional exponential cases now compile and reach
submission in the compiler-only harness without instruction or draw-profile
refusals. The prior 2,304 blend cases also retain compiler coverage.
The shim deliberately rejects GPU submission; none of its image results
counts as a hardware pass.

The focused compiler, ALU, transcendental, FALU, sRGB epilog, raster-state
and HAL300 graphics-state suites pass. The Vulkan library also builds with
the shared state structure change; its existing four-component caller
retains the same values. These checks are not Vulkan runtime qualification.

All 2,435 cases passed on the physical M5: the unchanged 2,323-case
clear/blend baseline plus all 112 exponential cases. The run took 317.924
seconds and peaked at 44.537 degrees Celsius. All 5,394 submission generations
retired both queues, with 10,814 consecutive kernel records and no loss,
driver errors, thermal abort or timeout. The previous 2,323 cases retain
identical per-case submission draw counts; 5,372 submissions contain multiple
draws. This is a development run at RGBA8, 64x64, seed 1, not full GLES
conformance qualification.

Library SHA256:
`775517d87bda08f2bd2dca59a24e687bf2097611554f2927ed0aa9107e0c6769`.
Case list SHA256:
`8b3c87993d766981408c87cba14430b91c42e43c30262ba88002e200ad1146ec`.
The hardware ledger now records 2,435 of 54,799 passes. Raw CTS results,
the complete kernel stream and runtime provenance are retained as
`m5-native-gles3-typed-arithmetic.qpa.gz`,
`m5-native-gles3-typed-arithmetic-kernel.log.gz` and
`m5-native-gles3-typed-arithmetic.json`.
The uncompressed QPA SHA256 is
`c91707a987dde1e315cd255003d15c58ffbc4aeda1220d3690245a94d552baf9`.
