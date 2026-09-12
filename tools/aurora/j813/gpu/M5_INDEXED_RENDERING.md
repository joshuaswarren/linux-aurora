# M5 indexed rendering: fragment recovery fix

On J813/T8142, a 256x256 RGBA8 indexed draw of 64 overlapping full-screen
triangles completed the vertex/tiling stage but recovered before fragment
completion. The equivalent native macOS 25G76 draw completed successfully.

The admitted Linux application profile inherited auxiliary-framebuffer
control register `0x15021 = 0x10c001` from an earlier native triangle witness.
The native indexed draw used `0x10c000`. Using that value for M5 application
submissions fixes the reduced failure. The precise hardware meaning of bit
zero remains unknown; the result does not establish other auxiliary modes.

## Hardware comparison

The same Vulkan executable, shaders, index buffer, image, and Mesa library
were used for the before/after comparison. Indices cycle through 0, 1, 2;
each triangle covers the full viewport and writes RGBA `(255,71,13,255)`.

| Application profile | Result |
| --- | --- |
| Original auxiliary control, 32 triangles | Correct pixels and completed queues |
| Original auxiliary control, 64 triangles, fresh VM | GPU recovery; TA completion only |
| Fourfold parameter-pool capacity, original control | Same failure |
| Native fragment USC reservation, original control | Same failure |
| Native tile-buffer count alone, original control | Same failure |
| Native auxiliary control alone, original pool/TIB/USC | All nine frames pass, 1 through 160 triangles |

The passing sweep uses 3, 6, 12, 24, 48, 96, 192, 384, and 480 u32 indices.
Each of the nine frames checks all 65,536 pixels. Both GPU queues retire in
generations 1 through 9. An independently downloaded final attachment also
matches every pixel and has SHA256
`8ebe40523a836923ad5f331124b2caf011a971c9ce6232acfa6a188331125e36`, identical
to the native full-screen output.

Earlier reduced-coverage controls render 64 inside-clip triangles and 64
clipped thin triangles correctly. Independent edge equations, after decoding
Mesa's 64x64 Morton-tiled RGBA8 layout, match all 65,536 pixels in both cases.
Thus clipping alone was not sufficient to trigger the original failure.

## Validation scope

The fix remains inside the existing M5 application admission checks: 256x256,
single layer, single sample, 32x32 utiles, with the existing color/depth,
address, ownership, and completion checks retained. It changes no shader,
fence result, pixel data, parameter-pool capacity, or firmware reservation.

Depth regression and real GLX gears presentation are the next qualification
gates. A passing offscreen test does not establish an accelerated desktop,
general Vulkan conformance, other resolutions, or GPU power management.

## Local evidence

Evidence is retained in the J813 workbench, outside the source repository:

- `logs/m5-draw-count-fresh64-result-20260912.log`: original fresh-VM failure.
- `logs/m5-pb12-failure-reports-20260912/`: firmware reports and retained TA
  completion; three obsolete fixed user-address capture attempts are absent.
- `logs/m5-draw-count-native-hv-20260912/`: successful native reference and
  ordered register programs, including repeated register writes.
- `logs/m5-aux0-sweep-result-20260912.log`: nine passing application frames.
- `logs/m5-aux0-fullscreen160-spatial-20260912.json`: independent final image check.

The passing kernel module SHA256 is
`33143d20e4f3b10b45a9f9cfe93922439d0f08b2477370099521ceb57b294ad9`.
The draw-count test SHA256 is
`be6ea604e37fa73f83ec3dde3d6b8498f0078e5f3f39e196caefeb5bcd9da614`.
The compressed Mesa overlay SHA256 is
`793e9f8363ced5397f3c0547f0f358cf5bd0fb78938157ed5c486931fac822e4`.
