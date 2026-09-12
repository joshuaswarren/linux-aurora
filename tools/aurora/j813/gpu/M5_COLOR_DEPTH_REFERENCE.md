# M5 color interpolation and depth reference

Native Metal execution on M5 independently passed the color, smooth-depth and
flat-depth cases on 2026-09-12. Each case checked 60,928 pixels away from triangle
edges, including 16,200 interior pixels, with zero mismatches. The depth cases
draw a near colored triangle before a farther white triangle and require the
colored result to survive D32 LESS testing with depth writes enabled.

These are macOS hardware references. They do not establish Linux rendering,
moving GLX gears, Vulkan conformance or accelerated DCP composition.

## Reproduce the native oracle

Build `m5-color-depth-reference.m` with the macOS SDK:

```sh
xcrun clang -arch arm64 -mmacosx-version-min=26.0 -fobjc-arc \
  -framework Foundation -framework Metal m5-color-depth-reference.m \
  -o m5-color-depth-reference
./m5-color-depth-reference color color.rgba
./m5-color-depth-reference depth depth.rgba
./m5-color-depth-reference flat-depth flat-depth.rgba
```

The executable checks the target model and GPU. `M5_REFERENCE_WAIT_STDIN=1`
pauses before submission; `M5_REFERENCE_HOLD_AFTER=1` retains its resources
after validation for a separately controlled m1n1 capture. Use the existing
graphics capture workflow and exclude system queues when selecting this client.

Run the independent capture verifier against the resulting directory and image:

```sh
python3 inspect-m5-color-depth-capture.py --capture CAPTURE_DIRECTORY \
  --rgba flat-depth.rgba --mode flat-depth --output verified.json
```

It verifies saved page hashes, page-table translations, input vertices,
pre-submit output sentinels and the post-submit pixel oracle. Captured native
programs, memory pages and device data are intentionally not distributed here.

## Hardware-derived state

The color PPP profile uses the full 48-bit, 64-byte-aligned varying-table
address. Smooth float4 color has coefficient-table words `505/c/1011f`; flat
color uses `405/103`, `405/11b` or `405/10b` according to the provoking vertex.
The profiles preserve the complete 172-byte native PPP sequence in Mesa tests.

D32 LESS/write changes the PPP control from `40200` to `200` and both face
controls from `07200f00` to `01000f00`. A 256x256 attachment uses depth dimensions
`00ff00ff`, ZLS control `80044`, a 256 KiB data allocation and one 16 KiB metadata
page. Native single-layer depth stride fields are zero. The Linux admission
check validates the complete writable ranges before normalizing those strides.

M5 advertises no attachment-feedback-loop support. This lets ordinary Zink D32
attachments use the captured compressed ZLS path; color PBE compression remains
outside the current implementation. The first supported Linux depth envelope
is 256x256, one layer and sample, clear depth 1.0, no depth load or stencil.

Host validation includes exact native PPP records, M5 depth-dimension packing,
13 M5 render register tests and a focused production depth-admission harness.
Passing these checks establishes command construction and rejection behavior;
actual Linux output must be validated separately.

## Linux execution boundary

A reduced Vulkan draw with color loaded from a vertex buffer completed two
consecutive submissions on Linux. Both frames matched all 65,536 RGBA8 pixels;
the copied second-frame output was independently checked on the host. The
trace contains a live `505/c/1011f` varying table for each frame. An earlier
constant-in-shader variant also passed, but its varying was optimized away and
does not establish interpolation support.

The same draw with compressed D32 also passed two consecutive frames. A further
four-frame run exercised smooth/flat color, each with and without point-size
output. Every frame drew orange, then white at equal depth. All 65,536 pixels
remained orange in all four frames, demonstrating LESS rejection and depth
writes, with both queues retiring for generations 1 through 4.

Build `m5-vulkan-color-depth-test.c` on Linux/aarch64 using
`sh build-m5-color-depth-test.sh OUTPUT_DIRECTORY`. With the experimental M5
Mesa and kernel selected, run the executable with `ASAHI_M5_BRINGUP=1`,
`AGX_RENDER_UNCACHED=1`, `AGX_RENDER_DEPTH=1` and `AGX_RENDER_REPEAT=4`.
`AGX_RENDER_OUTPUT=output.rgba` saves the final raw image;
`AGX_RENDER_HOLD=1` retains the process for inspection. Uniform vertex colors
make the expected output independent of tiling. This does not qualify varying
gradients, other attachment sizes, depth loads or arbitrary render modes.

The full GLX gears workload now compiles and reaches the M5 firmware. Its first
render submission requests GPU recovery on fragment queue 11. Moving gears and
accelerated desktop composition remain unproven. The next comparison targets
gears' fragment uniforms, provoking-vertex selection and broadcast outputs.
