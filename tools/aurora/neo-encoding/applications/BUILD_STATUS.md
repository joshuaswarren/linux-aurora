# Initial native ARM64 application suite

Prepared on 2026-09-13 for the J700 Plasma image. These are installation and
software-control results; physical J700 qualification remains pending.

| Application | Installed version/source | Initial scope |
| --- | --- | --- |
| Google Chrome | 153.0.8010.36-1, official ARM64 Debian payload | Native payload repackaged for pacman; original sandbox retained |
| Firefox | 155.0.1-1, signed Arch Linux ARM package | Stock native package |
| Blender | 5.2.1-4, official source archive | EEVEE, CPU Cycles, OpenGL and Vulkan compiled |
| OBS Studio | 32.2.2, `ba2f32bdf791005443988a4955e963663e16b1ed` | OpenGL, PipeWire, recording, x264, WebSocket, Lua/Python |
| VK-GL-CTS | main snapshot `1942832354c96f0de379f491519734256fbd4f9d` | Vulkan, EGL, GLES2/3/3.1 and OpenGL CTS; X11 EGL target |

Chrome payload SHA-256:
`1fcf6ec51a9d52e26ff1ad807225f725be55069ad2b68c5d69361a2746665188`

Blender source archive SHA-256:
`14ca60236b029f575c2a8b6042554c956736206adef5ecb396445a394c0c9213`

Blender uses system glog (`WITH_SYSTEM_GLOG=ON`) to avoid duplicate flag
registration with system libraries. Its Audaspace FFmpeg writer uses
`avcodec_get_supported_config` with FFmpeg 9, retaining the older API for older
versions. The compatibility patch is retained beside the packaging recipes.
System Python requires numpy, cattrs and requests. Optional Embree, OIDN, OSL,
USD, MaterialX, Hydra and path guiding were disabled because the corresponding
distribution libraries were absent or incompatible. This build does not
establish ANE or Cycles GPU compute support.

OBS selects `MbedTLS_DIR=/usr/lib/mbedtls3/cmake/MbedTLS`. CEF browser sources,
AJA and NVENC are omitted. Its generated-scene recording uses software x264.

All ten installed application/CTS executable headers report AArch64 and have
no missing linked libraries. The package database consistency check passed.
The builder's Xvfb/llvmpipe controls produced these results:

- Firefox: both WebGL pixel readback tests passed with a software renderer.
- Chrome: launched and submitted results, but its default blocklist prevented
  both WebGL contexts. No sandbox or blocklist bypass was introduced.
- Blender: EEVEE rendered the small cube scene. Generated FLAC and AAC exports
  were recognized as 48 kHz audio and decoded successfully.
- OBS: generated video/audio recording decoded to 368 frames with 11 distinct
  video frame hashes.
- CTS: executables and resources installed; no GPU case results yet.

Run the suite from the physical Neo's desktop session and retain the receipts,
renderer strings, render/recording artifacts and CTS QPA logs before updating
hardware status. An all-NotSupported CTS batch cannot pass the runner. A bounded
batch does not establish Khronos conformance.

The first hardware boot exposed a missing `iw` utility in the image. The signed
`iw` 6.17-1 package was added to the installed system and offline root, and future
image construction now checks for it. WLAN firmware registration succeeded
before that userspace command failed; the continuation does not reload WLAN.

The first persistent SSD image uses the previously qualified GPU1 kernel/Mesa
baseline. The installed m1n1 resident is preserved; independent untethered SSD
boot is not yet qualified.
