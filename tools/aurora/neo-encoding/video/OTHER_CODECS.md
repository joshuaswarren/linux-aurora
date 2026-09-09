# Additional Neo hardware codecs — 2026-09-09

Read `/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md`
in full after a resume. Never save the runtime webhook endpoint in artifacts.

## Verified results

**ProRes 422 now works in native Linux:** 60 moving 320x240 frames, 30 fps,
two seconds, all independently decoded. Minimum PSNR is 35.51 dB. The first
decoded frame is byte-identical to the macOS hardware reference. All six
CPUs remain online and Vulkan passes its 32 exact buffer values.

H.264 and HEVC each passed a three-frame regression on this same boot, with
clean Stop/Close, CPU and Vulkan checks. Their earlier 60-frame qualification
archive remains intact. ProRes uses the dedicated APR engine; H.264 and HEVC
use AVE. FFmpeg on the controller decodes and muxes these hardware outputs.

| Codec | Native Linux | macOS hardware reference |
|---|---|---|
| H.264 / HEVC | Earlier 60-frame qualification; new 3-frame regressions | Established |
| ProRes 422 | 60 moving frames pass | 3 frames pass |
| ProRes HQ / LT / Proxy / 4444 / 4444 XQ | Not ported or tested yet | 3 frames each pass |
| JPEG | Not ported or tested yet | 3 frames pass, JPEG0 activity captured |
| HEVC with alpha (`muxa`), depth (`deph`), disparity (`dish`) | Not ported or tested yet | Advertised hardware encoder wrappers; no encode tests yet |
| HEVC Main10 / Main42210 | Not qualified | Advertised selection profiles; no matching input qualification yet |
| ProRes RAW | No encode path established | Not in the saved encoder list; general hardware acceleration claim does not establish encoding |
| AV1 / VP9 / VP8 | No supported hardware encode path established | Hardware-required creation returns encoder not found |
| Compressed audio | No hardware encoder qualified | Inventory pending; see `../audio/HARDWARE_ENCODING.md` |

This is an experimental fixed-format path, not a registered V4L2/FFmpeg
hardware encoder. Arbitrary sizes, alpha, high-bit-depth user input, sustained
throughput, rate control and a streaming API remain unqualified.

## Coverage audit and expanded scope

Online target audit: `../A18_ENCODING_TARGET_AUDIT.md` checks Apple A18 Pro
specifications and developer documentation. A18 Pro is the correct Neo
reference. It adds an MV-HEVC hardware probe and leaves RAW encoding as
unproven; Apple's current RAW recording workflow requires iPhone 17 Pro or
later, which does not establish A18 Pro encoding support.

The user explicitly included hardware audio encoding on 2026-09-09. It is
part of the Linux media target; it has not yet been tested on the Neo.

The frozen `encoders.json` identifies Mac17,5 and macOS 26.6.1 build 25G76.
Its 12 entries marked hardware comprise six ProRes variants, H.264, HEVC,
JPEG and three HEVC wrappers (alpha, depth and disparity). The HEVC encoder
also advertises Main, Main10 and Main42210 selection profiles. Enumeration
establishes exposed capabilities, not successful encoding of those profiles.
The existing 4444/XQ reference tests used 8-bit 4:2:2 input, so true 4:4:4,
alpha and higher-bit-depth input remain separate qualification gaps.

JPEG sequences/Motion JPEG, still-image HEVC/HEIC, and MP4/MOV/Matroska
packaging belong on application/integration tests; do not count a container
as a new hardware codec. Multiview HEVC is not independently established by
the saved encoder list. Probe it explicitly: Apple documents spatial video
recording on iPhone 16 Pro and identifies spatial video as MV-HEVC, but actual
Neo hardware execution has not been tested.

[Apple's Neo specifications](https://support.apple.com/en-us/126322) list AV1
decoding, not an AV1 encoder. They describe ProRes RAW hardware acceleration
without resolving RAW encoding in the API tested here. Do not mark RAW as
impossible, or as a proven missing encoder, from either observation alone.
The audio formats on that page are explicitly under Audio Playback and do
not establish compressed audio hardware encoding.

## Current device state

Native run: `../logs/20260909T033655Z-native-linux/`. The boot controller exited
normally. The Neo remains in native Linux; KIS is its console, not an m1n1
proxy. ProRes and AVE stay pinned with their DMA buffers and power references
until reboot. Never force-unload them or retry a failed hardware command.

AVE client 2 closed successfully; the next AVE encode selects client 3.
ProRes completed the initial frame, the 60-frame sequence and a final frame
after both AVE regressions: 62 completed frames, no error. That final pass is
recorded in `../logs/prores-after-ave.log`. All runners have exited; KIS is
free. `../logs/all-codecs-final-state.log` records the healthy final state.

The host remains the M3 Max, hpm0/RID0, Neo KIS endpoint
`/dev/cu.kis-00100000-ch-0`. Another Mac is attached on the other controller;
do not touch it. Stage 1 and persistent storage are unchanged. Do not install
a qualification Stage 2 as Stage 1 or apply the staged macOS update.

## Working artifacts and commands

- `../artifacts/prores-linux-60-20260909/`: validation, exact packets, raw
  compressed frames, original/decoded pixels and playable `video.mov`.
- `../artifacts/prores-linux-orderfix-20260909/`: first successful native frame.
- `../artifacts/ave-prores-h264-regression-20260909/` and
  `../artifacts/ave-prores-hevc-regression-20260909/`: same-boot AVE regressions.
- `../artifacts/linux-prores/`: matching DT, module and unstripped ELF.
  Module SHA256:
  `777b166b972028e6dbc92888cc2842f8aafc746028aaa37cd8758a840703df51`.
- `../artifacts/other-codecs-reference-qualified-20260909/`: immutable macOS
  reference archive, 75 files / 5,139,839 bytes, all hashes read back.
- `../artifacts/prores-linux-qualified-20260909/`: matching ProRes module,
  unstripped ELF, DT, source, input captures, successful outputs, failed
  attempt, regressions and manifest: 429 files / 69,761,176 bytes, all hashes
  read back. Transfer the dependencies listed there.
- `../artifacts/ave-video-qualified-20260908/`: earlier H.264/HEVC qualification.

Use `linux-prores/README.md` for the cold RAM boot/build recipe. On the
healthy running ProRes module, from the workspace root:

```sh
python3 neo-linux/video/encode-prores-linux.py \
  neo-linux/artifacts/prores-linux-attempt-20260909/frame-000.bin \
  neo-linux/artifacts/other-codecs-reference-qualified-20260909/prores-native-reference \
  neo-linux/artifacts/my-next-prores-test --frames 60 --warm
```

Use a new output directory. Omit `--warm` only for an idle freshly loaded
module. The diagnostic advances its ring without wrapping; it returns
ENOSPC before exceeding the 128 KiB ring. Timings include transfers and
validation, so they are not throughput benchmarks. Reuse the existing
kernel/Mesa build; `../scripts/build-prores.sh` builds only the module and DT.

For H.264 or HEVC, use `WORKING_LINUX_ENCODING.md` and
`encode-linux-video.py NEW_DIRECTORY --codec h264|hevc --frames 60`.

## What resolved the ProRes failure

The first Linux module enabled MODE 0xd before installing its ring. The
submission timed out after five seconds: DART SID 0 reported write protect
at address zero, tail stopped at 0x100, IRQ 0, output length 0. The failed
module, source, packets and logs are preserved in
`../artifacts/prores-linux-first-20260909/`.

The successful module installs the relocated ring address/size in the same
order as macOS, before MODE 0xd. Warm submissions advance the producer head
instead of resetting an enabled ring. The same packet then encoded correctly
and passed the 60-frame test. No DART protection or overlap guard was removed.

The Neo's ring is 128 KiB. Its selected descriptor is v1, 384 bytes, with the
frame header at 0x7c, linear 16-bit YUV 4:2:2 input (flags 0x363b), and stride 640
on both planes. Alpha is disabled. Only output/luma/chroma pointers are
relocated; the disabled alpha pointer is cleared. The raw native bitstream
matches the macOS payload, apart from macOS adding two trailing padding bytes
and updating the frame-size field. Decoded pixels match exactly.

The input capture contains all 240 luma rows plus 16 complete chroma rows.
A rounded chroma read crossed its last mapped page and was refused. The
constant-row test pattern permits reconstruction: `make-prores-packet.py`
verifies all 16 observed chroma rows are identical before repeating the row.
The later native comparison validates the reconstructed image. Future tracer
code reads exact lengths and tracks the previous producer head; those later
fixes were not loaded in the final reference guest. Use only apr-01 from it.

## Capture and remaining codec work

Reference run `../../neo-update/logs/20260909-024939Z-kis-90524/artifacts/hv/`
enumerated the hardware encoders and captured JPEG0 writes. All six ProRes
variants produced three frames and decoded with minimum PSNR 33.42–37.27 dB.
The 4444 tests used 8-bit 4:2:2 input; alpha/high-bit-depth input is untested.

JPEG does not implement the usual using-hardware status property (-12900).
The initial guarded probe submitted no frames. Explicitly selecting the
advertised `com.apple.videotoolbox.videoencoder.jpeg.ajpeg` hardware ID with
RequireHardware enabled produced three frames; 1,662 JPEG0 register writes
correlated with that encode. An unsupported query is not software-fallback
evidence. Hardware-required AV1/VP9/VP8 creation returned -12908.

APR reference run
`../../neo-update/logs/20260909-031749Z-kis-96031/artifacts/hv/` captured the
working ring and inputs. Its controller is closed. The preceding run 94037
refused the larger ring because of a 64 KiB guard; encoding still succeeded.
The tracer now allows bounded rings up to 256 KiB, preserving RAM checks.

Neo APR: `0x295000000+0x4000`, wrapper `0x294000000+0x8000`, DART
`0x295020000+0x20000`, interrupts 1260/1259 and DART 1257, power domain
`ps_prores`. Its ADT names are singular `apr` / `dart-apr`. Shared PMGR is
excluded. JPEG0 is `0x28d000000`, its DART `0x28d020000`; JPEG1 is
`0x28d060000`, DART `0x28d080000`, Linux power domain `ps_jpg`.

Existing org code provides useful starting points:
`../../neo-update/m1n1-kis/proxyclient/experiments/prores.py`,
`experiments/jpeg.py`, `experiments/jpeg_doc.md`, and `m1n1/hw/{prores,jpeg}.py`
under that proxyclient tree. Do not run their old-chip initialization unchanged.
The local scripts can extract AppleProResHW and AppleJPEGDriver from the
pinned 25G76 kernelcache. Do not repeat completed macOS captures merely to
demonstrate them; use the frozen evidence for subsequent implementation.

The native ProRes milestone was delivered to the configured Discord workflow
with HTTP204. Record: `../logs/prores-native-milestone-delivery.json`. Do not
duplicate that milestone or the earlier additional-codec reference update.
