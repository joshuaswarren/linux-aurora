# MSAA sample source encoding

The register-sourced sample operand was encoded incorrectly in
`agx3_tex_stage_ms_sample`. Field f5 selects texture operand bank 4, while f16
selects the architectural half-register containing the sample index. The old
helper changed f5 with the requested source and left f16 zero. Thus the
instruction read r0 and could publish it into the wrong operand bank.

Fresh own-MSL native G17P fragment captures use sample indices derived from
`uint(position.x) & 3` and `(uint(position.x) + uint(position.y)) & 3`. Both
contain `1705540400100000d0c00800`, with register source 8. The earlier
`1705540400000000d0c00800` witness has source 0; its old test annotation
incorrectly treated the bank number 4 as the source register.

The corrected helper fixes f5 at 4 and encodes the requested source in f16,
using the same high register-bit handling as the existing typed-LOD helper.
It preserves instruction count, dependency waits, source liveness and normal
draw batching. No descriptor ABI, sample count, capability or CTS change is
included.

## Evidence

- `native-msaa-register-witness/`: initial two independent fragment captures.
- `reproduced-msaa-sample-source/`: both reproduced by the new repository-owned
  `src/asahi/compiler/test/probes/capture-g17p-msaa-sample.py` tool, with source,
  native objects, extracted sections, commands, logs and hashes retained.
- `msaa-fetch-abi/regression-red.log`: corrected native-byte regression fails
  with the old compiler at the register-0 witness.
- `msaa-fetch-abi/regression-green.log`: both register-0 and register-8 witnesses
  pass after the correction.
- `msaa-sample-source-regressions.log`: 264 passes, 2 expected failures and the
  existing `asahi-g17p-precompiled-fill` failure. Its robust-index goldens remain
  unchanged and unresolved.
- `libgallium-msaa-sample-source.so`: SHA256
  `468edaf312ce469e189f29b66cfe80fa4336e4988c83e80925603bc51d05cc68`.

The fresh native fetch opcode `25800ca090008014000100001200` was also reproduced
exactly in an isolated control. Relative to the driver descriptor ABI, byte12
selects Apple's ABI and byte9 selects sampler 1. Changing just these fields in
an artifact-only encoder produces the native bytes with texture0/wait1. That
comparison does not justify changing the production fetch opcode or adding
waits. See `msaa-fetch-abi/sampler-control-receipt.json` and the field-map logs.

## Hardware status before the first CTS rerun

The initial RAM upload lost the Neo USB connection during Stage2 readback,
before the jump. The resident had passed its 100-query/readback gate. VDM on
the verified Neo port restored the connection, and a new guarded RAM boot
reached Linux (`9769a016-09c5-4974-926e-b1123d615724`). The candidate installed,
but DCP's inherited CPU was not running: component bind failed with EPROTO and
the dedicated Xorg exited. No CTS case ran. This is not a GPU test result.

`restore-dcp-cts.sh` now also requires the actual appledrmfb after probing:
module insertion success alone previously obscured the failed component bind.
The normal Linux reboot's first resident query raced shutdown and stopped
before any upload. The later `boot-msaa-sample-source-display-ready` boot passed
resident verification and the Stage2 jump; its Linux upload is in progress.
Hardware results will be appended after the display and CTS gates succeed.

## Hardware results

`boot-msaa-sample-source-display-ready` completed on boot
`c787cb00-913b-4cf3-aad2-78f68d1f7ab3`. Both kernel and initramfs readbacks passed.
Root login, Wi-Fi and the once-per-boot preparation completed. The updated
display gate verified appledrmfb. The dedicated test server was Xorg711.

- `fast-iterations/msaa-source-maxcolor-ready`: exact recorded maximum-height
  colour case, candidate SHA468edaf..., **Fail, no GPU fault**, CTS0.545s,
  whole iteration3.644s. R8 still reads zero at(0,16383).
- `fast-iterations/msaa-source-maxdepth-ready`: same recorded session options,
  exact depth case, **GPU fault**, CTS2.316s, whole iteration4.588s. Readback
  still returns depth0 where0.25 is expected. Firmware again blames DM1 and
  loses a render submission. The QPA is unfinished; do not count it as a
  completed test or as a pass. Xorg711 was stopped by the harness.

The source batch still contains four draws in one submission. The real
sample-register encoding defect is corrected, but these results demonstrate
it is not sufficient to fix the zero MSAA readback or the depth fault. The
full official suite was not rerun after a known reproduced GPU fault; its
last totals are unchanged. No further GPU commands may run in this boot.
Read-only SSH confirmed the boot identity and no Xorg/glcts/cts-runner
processes. All boot/build/capture/test/console handles in this continuation
are terminal.

Next investigation: establish whether the MSAA source depth/storage path
or its sampler descriptor interpretation loses the values. Do not replace
the already explained fetch ABI or add barriers on speculation. A fresh
guarded RAM boot is required for the next GPU diagnostic.

## Separate clip-distance diagnostic correction

The older handoff's wording that the driver reports `gl_MaxClipDistances=0`
is too strong. In the pinned official CTS source, `glcClipDistance.cpp:329`
initializes a transform-feedback buffer to zero, draws the shader constant
into it, then compares the buffer readback at line344. The zero in the QPA
is this readback value, not a direct capability query. Mesa's state tracker
sets MaxClipPlanes=8 for GLSL130 or later (`st_extensions.c:1277`), including
drivers whose hardware clip-plane cap is zero. Therefore investigate the
transform-feedback data path before changing clip-distance limits. Actual
clip-distance execution support remains unproven; this only corrects the
interpretation of that particular failure message.
