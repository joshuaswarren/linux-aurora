# Neo diagnostic encoding coverage

Current evidence and target states are in [ENCODING_TARGETS.json](../ENCODING_TARGETS.json).
Detailed failures, corrections and device ownership are in
[ENCODING_EXPANSION.md](../ENCODING_EXPANSION.md).

Qualified at 320×240 with a reference-matched frame and 60 changing frames:

| Engine | Qualified output | Current limits |
|---|---|---|
| AVE | H.264 Main, HEVC Main | 8-bit NV12 source |
| AVE | HEVC Main10, Main42210 | Real 10-bit P010/P210; precision sequences force keyframes |
| APR | ProRes 422, HQ, LT, Proxy, 4444, XQ | 8-bit UYVY source; 4444/XQ alpha opaque |
| APR | ProRes 4444, XQ with alpha | Real 12-bit AYUV16 color; all 16 alpha bits preserved |
| JPEG0 | Baseline JPEG and Motion JPEG | Captured full-range NV12, 8-bit 4:2:0 |

Each engine/profile group passed H.264/HEVC/ProRes regressions, CPU0–5 and
Vulkan compute. These are diagnostic hardware paths. Standard application
integration and throughput qualification remain separate.

## Explicit diagnostic selection

These commands require the corresponding module loaded on native Neo Linux,
with no other KIS client. AVE must be configured and idle. Output directories
must be new. Unsupported profile/layout/precision combinations fail before DMA.

```sh
python3 neo-linux/video/encode-linux-video.py NEW_OUTPUT \
  --codec hevc --profile main10 --pixel-format p010le --bit-depth 10 --frames 60

python3 neo-linux/video/encode-linux-video.py NEW_OUTPUT \
  --codec hevc --profile main42210 --pixel-format p210le --bit-depth 10 --frames 60

python3 neo-linux/video/encode-prores-linux.py CAPTURED_PACKET MATCHING_REFERENCE NEW_OUTPUT \
  --profile hq --pixel-format uyvy422 --bit-depth 8 --alpha none --frames 60

python3 neo-linux/video/encode-prores-linux.py CAPTURED_PACKET MATCHING_REFERENCE NEW_OUTPUT \
  --profile 4444 --pixel-format ayuv64le --bit-depth 12 --alpha preserve --frames 60 \
  --target-packetgen

python3 neo-linux/video/encode-jpeg-linux.py CAPTURED_PACKET MATCHING_REFERENCE NEW_OUTPUT \
  --codec mjpeg --profile baseline --pixel-format nv12 --bit-depth 8 --frames 60
```

ProRes `--warm` applies only after an already successful APR initialization in
the same boot. A firmware/DMA failure requires reboot and diagnosis. Never
unload a submitted module or reuse failed state.

ProRes and precision HEVC support `--validate-only` in their dedicated tools,
which recheck saved hardware outputs without opening KIS. This is the path for
fixing extraction or decode validators; do not repeat hardware submissions to
repair a host-side parser.

## Remaining evidence

MV-HEVC, HEVC depth and disparity have independently decoded hardware references.
Depth and disparity are lossy float32 outputs (maximum relative errors 1.95% and
2.49% for the deterministic reference inputs). Native auxiliary formats still
need implementation. HEVC alpha needs a corrected hardware reference probe.
The remaining reference batch is prepared, but macOS boot failures currently
block new captures. Six-CPU boots fault in EL2; reduced-CPU configurations also
failed, including a captured XNU VA-to-PA translation panic. Repeated boot
attempts have stopped. See [results and blockers](../ENCODING_EXPANSION_RESULTS.md).

ProRes alpha qualification independently decodes both FFmpeg 12-bit pixels and
host AYUV16 pixels. The latter proves preservation of all 16 alpha bits; FFmpeg's
12-bit output alone cannot test the lower four alpha bits. `--target-packetgen`
uses the verified native file generator to reduce repeated input transfers;
it does not access DMA.

The separate AVE pool archive `artifacts/ave-pool-qualified-20260909` validates
the default 20 MiB and optional 32 MiB allocation lifecycles, larger-pool firmware
startup, bounds rejection and H.264/HEVC/ProRes regressions. This is preparation
for multiview; it does not qualify MV encoding. Existing full profile
qualifications use the original AVE module. Keep the original module for those
recipes until the larger-pool profile matrix is explicitly expanded.

All 16 exposed audio formats remain in the investigation. The current Neo
inventory advertises no hardware audio encoder. Explicit AAC manufacturer
selection also accepted a nonexistent manufacturer, proving that success from
that API alone does not establish hardware execution. The probe now fails
closed on this negative control. APAC and software availability are not counted
as hardware success.

Preserve resident Stage 1. Use guarded RAM boots, the pinned 25G76 macOS kernel
for reference work and the existing KIS transport. Do not apply the staged update.
