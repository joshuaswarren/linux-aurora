# Neo hardware video encoding

Target: J700 / T8140 / Mac17,5. Build in the dedicated `neo-builder` VM.

Native Linux now boots over KIS with all six CPUs. AGX firmware starts after
the T8140 UAT handoff fix. Vulkan compute passed 1,000 submissions with 32,000
exact output values. See `../WORKING_NATIVE_BOOT.md` for the recipe and
evidence. The older raw add3 diagnostic faults and should not be run ahead
of the working Vulkan tests.

The dedicated video encoder is AVE2. The macOS 25G76 capture identifies
`com.apple.driver.AppleAVE2`, with H.264 4:2:0/4:2:2/4:4:4 and H.264/HEVC
capability properties. These are advertised capabilities, not Linux results.
`../artifacts/neo-ave-hardware.json` records the observed five MMIO regions,
five interrupts and clock/power IDs, with the original capture's hash.

The pinned Linux tree has no Apple AVE encoder driver. Its AVD decoding code
and AGX graphics driver do not provide an AVE encoding API. A working encoder
therefore needs AVE command/firmware transport, memory mapping, submission,
completion and buffer ownership to be recovered and implemented.

Start with an 8-bit NV12 input and H.264 output. Keep a deterministic input,
encoder parameters, encoded stream, decoder result, frame count, timestamps,
quality measurement and driver log for each experiment. A registered device
or a completed queue is insufficient: require a decodable stream whose
frames match the intended input and identify which hardware performed it.

The saved macOS hypervisor remains the tracing reference in
`../../neo-update/WORKING_KIS_BOOT.md`. When returning there for AVE capture,
use the verified 25G76 kernel and KIS boot path, with the required MT7932
bootstrap before AGX tracing. Do not apply the staged update or replace the
installed Stage 1. Gather AVE/DART activity around one controlled encode
before attempting undocumented encoder MMIO writes from Linux.

The encoder firmware and 31 actual command-ring publications have now been
captured during a successful three-frame macOS hardware encode. The Linux
userspace codec in `ave2-wire.h` matched all 31 descriptors, padding bytes
and ring-index transitions; the output decoded 3/3 frames at 53.13 dB PSNR.
This qualifies the observed 64-bit descriptor format. Linux AVE firmware
startup and frame submission remain unimplemented. The native kernel
foundation in `linux-ave/` now initializes the two DART connections and owns
20 MiB IPC plus 64 KiB queue allocations. All 1,284 IOVA pages matched CPU
pages on load and reload; cleanup passed. This tests memory mapping and
allocation ownership, not firmware DMA transactions or ring completion.
See `ENCODER_HANDOFF.md` for current state and
`../artifacts/ave-capture-20260908/three-frames-rings/overall-validation.json`
for the saved result.

`encoding-baseline.py OUTPUT_DIRECTORY` creates a deterministic 320x240,
30 fps, 60-frame NV12 clip, encodes it with software libx264, decodes it and
checks dimensions, frame count and PSNR. The first run in `neo-builder`
passed at 54.84 dB. Input, output, decoded bytes and tool versions are hashed
in `../artifacts/encoding-reference-20260908/result.json`. This is a software
reference from the build VM, not a Neo CPU or AVE performance measurement.
