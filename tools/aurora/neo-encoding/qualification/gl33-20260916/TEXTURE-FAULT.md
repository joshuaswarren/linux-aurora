# Native GL3.0 CTS texture fault

Official case: `KHR-GL30.texture_repeat_mode.r8_49x23_0_clamp_to_edge` from unmodified Khronos tag `opengl-cts-4.6.8.1`, commit `067e8832315e79817ede1c4863804e440f5d1c80`.

The case reproduces a GPU memory fault independently on a clean J700 Linux boot. Earlier transform-feedback failures are not required to trigger it. The later VM-bind EIO, allocation failures and VBO assertion follow the original GPU fault.

## Recorded submission

`hardware-inventory/texture-r8-async-trace` records boot `d23511d5-b5df-438a-bb96-931661f2d602`, the logging-only Mesa library, unchanged CTS executable hashes, kernel output and decoded command stream. `asynctrace` follows G17P compact VDM aliases; the older synchronous `trace` path omitted those aliases. The diagnostic run is separate from the full conformance procedure.

The R8 source and RGBA8 render target have `semantic_compressed=false`, yet their wire descriptors both set bit 59 (`Compressed` in the Gen9 XML). The render target reload view contains:

```
26880a22 0c011009 fffe5400 c0000005 fffe7800 00000005 00000000 00000000
```

The target allocation is `0x5fffe54000..0x5fffe78000` (147×69 RGBA8, mipmapped backing). The descriptor's secondary address is `0x5fffe78000`. The hardware reports a read fault at `0x5fffe78100`, 256 bytes beyond that secondary address and beyond the allocation.

`analyze-texture-trace.py` extracts the semantic and packed values, checks both packed addresses and records offsets from every fault. The receipt and exact descriptor words are preserved in `texture-analysis.json`.

## Candidate and regression

Gallium previously forced bit 59 for every extended Gen9 descriptor. The candidate uses the actual compression state for 2D textures, preserving the separate mip-level and extended fields. It leaves other dimensions' existing encodings for separate qualification. Honeykrisp's Gen9 packer already separates these fields.

`test_gen9_uncompressed_mip_views` covers a base-only reload view, a complete mip chain and a nonzero first level. Before the fix it fails at `!decoded.compressed`; after the fix it passes. The existing native `mip_2` capture bytes remain unchanged; their test input now declares compression to match the captured bit.

Asahi/poly regression results: 264 passes, two expected failures, and the same pre-existing `asahi-g17p-precompiled-fill` robust-index size/hash mismatch. No reference bytes were regenerated or waived.

Validated library: `59bd55a15dccb41f0d74f314684c598dae92dc07eee6b04bea5ce6e2eacc9802`. On clean boot `fafdad9c-3a9b-42f7-8264-edbf3ec6dd47`, the same unmodified official case passes (1/1, zero failures, unsupported cases, warnings or waivers) without any GPU fault. See `hardware-inventory/texture-r8-compression-fix`.

The before/after trace has identical texture addresses and descriptor words except bit 59: each of the three descriptors differs only by word1 XOR `0x08000000`. All three compression mismatches disappear. This isolates the fault fix from resource padding, layer alignment, shader changes or altered test expectations.

The complete official X11/EGL `cts-runner --type=gl30` has been restarted with this library, without diagnostic flags or case filtering. Full OpenGL 3.0 conformance is not established.
