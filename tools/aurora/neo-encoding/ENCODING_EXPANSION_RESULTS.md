# Neo encoding expansion — 9 September 2026

The diagnostic Linux paths below are hardware validated. The full plan is
partially complete: HEVC alpha and multiview implementation remain blocked
on missing reference captures. Standard FFmpeg/V4L2 integration, 4K throughput
and sustained streaming remain deferred.

| Linux qualification | Input/output precision | Evidence |
|---|---|---|
| ProRes 422, HQ, LT, Proxy, 4444, 4444 XQ | 8-bit UYVY source; opaque alpha for 4444/XQ | [All profiles](artifacts/prores-all-profiles-qualified-20260909) |
| JPEG and Motion JPEG | 8-bit NV12; Baseline 4:2:0 | [JPEG/MJPEG](artifacts/jpeg-mjpeg-qualified-20260909) |
| HEVC Main10 and Main42210 | Real 10-bit P010/P210; verified 4:2:0/4:2:2 | [Precision HEVC](artifacts/hevc-precision-qualified-20260909) |
| ProRes 4444 and 4444 XQ precision/alpha | Real 12-bit color; all 16 alpha bits preserved exactly | [ProRes precision](artifacts/prores-alpha-precision-qualified-20260909) |
| HEVC depth and disparity | Float32 input compacted through half-float/L010; 10-bit monochrome HEVC with depth metadata | [Auxiliary HEVC](artifacts/hevc-auxiliary-qualified-20260909) |

New video paths passed a reference-matched frame followed by 60 changing frames
at 320×240. Independent decoding checked frame counts, format and source quality.
Precision and auxiliary HEVC sequences force keyframes; inter-frame scheduling is not yet
qualified. Each group passed H.264/HEVC/ProRes regression gates, CPU 0–5 and
Vulkan compute. Archives contain inputs, outputs, traces, source, matching
binaries/symbols and checked hashes, with referenced dependency archives where
applicable. Existing [H.264/HEVC baselines](artifacts/ave-video-qualified-20260908)
remain available.

Depth/disparity use smoothly changing depth ramps. Their worst relative errors
are 3.30%/3.79%, with minimum range-normalized PSNR 54.30/51.51 dB. VideoToolbox and
FFmpeg plus the recovered companding LUT agree on every decoded float value;
the first frame matches the Neo's macOS reference exactly. This is lossy encoding,
not preservation of arbitrary float32 precision. A separate 60-frame row-wrap
depth stress run failed the 5% per-pixel gate at discontinuities (worst 19.35%).
That failure is preserved in the archive and is not counted as qualified.

The [larger AVE pool archive](artifacts/ave-pool-qualified-20260909) separately
qualifies 20 MiB (default) and 32 MiB allocation lifecycles, 32 MiB firmware/configuration,
end-of-buffer access and rejected overflow, plus three-frame H.264/HEVC/ProRes
regressions, six CPUs and Vulkan. The original qualified AVE module remains
unchanged. The larger pool is preparation for MV-HEVC, not proof of MV encoding.

## Remaining targets

- **HEVC alpha:** hardware advertised. The original PreserveAlphaChannel call
  was rejected. A corrected probe uses advertised EncodesAlpha and verifies
  readback; it has not run successfully because reference boot is blocked.
- **MV-HEVC:** two distinct eyes independently decoded from the hardware-required
  reference. Complete command/input capture and extra pointer relocations are
  still needed. The old capture was truncated and is rejected by the phase
  checker.
- **Hardware audio:** all 16 exposed encode formats were inventoried, including
  AAC profiles and APAC. None advertised a hardware implementation. AAC's
  requested hardware manufacturer was not enforced: a nonexistent manufacturer
  also succeeded at creation. The corrected probe refuses to count this as
  hardware encoding. No hardware/DSP audio encoder or native audio path is
  qualified; absence of hardware is not proven.

## Reference boot blocker

The six-CPU KIS macOS reference boot failed in EL2 during secondary CPU startup.
An isolated RAM-only SPTM serialization candidate built, but still faulted in
hv_pt_map_l3; it is not qualified or promoted. A cpus=1-only boot stopped in a
terminal kernel WFE loop. Removing unused guest CPU nodes with -C4 also failed;
its captured stack reaches _panic with the pinned kernel format string
`%s: VA->PA translation failed for va %p @%s:%d`. The exact failing VA was not
captured. These are distinct recorded failures, not a proven common cause.

No new codec probes ran during those failed sessions. Further reference boot
retries stopped after these bounded tests. Reuse the successful reference
captures; collect only missing AVE inputs/protocols after reference boot is
repaired. Do not repeat audio inventory, ProRes/JPEG captures or the failed
reduced-CPU configurations without a concrete correction.

The depth/disparity blocker was resolved without another macOS boot: static
VideoToolbox analysis recovered the logarithmic LUT, which matched all 460800
reference float values exactly. An owned CoreVideo L010 allocation established
that the extra 64 bytes trail 153600 visible pixels at offset 0. This agrees with
the frozen Neo descriptors. Native first-frame comparison then confirmed the
conversion and layout on the actual Neo hardware.

Exact boot evidence:

- [Six-CPU candidate fault](../neo-update/logs/20260909-072623Z-kis-11184/artifacts/hv)
- [cpus=1 stall](../neo-update/logs/20260909-073502Z-kis-14052/artifacts/hv)
- [Reduced-ADT panic context](../neo-update/logs/20260909-074619Z-kis-17613/artifacts/hv/context-04.json)
- [Failed candidate source and symbols](../neo-update/artifacts/kis-sptm-serialized-20260909)
- [Complete worklog](ENCODING_EXPANSION.md) and [target ledger](ENCODING_TARGETS.json)

The Neo remains in native Linux 7.1.6-aurora-neo. Auxiliary sessions and all three
baseline regressions passed. Final checks ran work pinned to each CPU 0–5 and
Vulkan compute successfully. APR, AVE and asahi remain
loaded, with the 32 MiB AVE pool configured and idle. No KIS client owns the device.
Resident Stage 1 is unchanged; all boots were guarded RAM loads. Kernel 25G76
remains pinned and the staged macOS update was not applied.

Existing full-profile archives retain the original AVE module. The live 32 MiB
module is recorded in the pool and auxiliary archives; depth/disparity and the
three baseline codecs passed with this module. Use --module-binary to check and
record an explicit candidate in the AVE diagnostic runner. Reboot before changing loaded firmware modules;
never unload modules after firmware/DMA submission.
