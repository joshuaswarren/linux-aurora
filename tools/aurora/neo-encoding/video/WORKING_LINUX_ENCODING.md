# Working native Linux AVE encoding on MacBook Neo

New 2026-09-09 checkpoint: ProRes 422 also passes 60 moving frames in native
Linux. Read `OTHER_CODECS.md` for its separate APR driver, current target
state, artifacts and limitations. The AVE H.264/HEVC recipe below remains
the established path for those codecs.

Verified on 2026-09-08: the J700/T8140/Mac17,5 encoded two different H.264
frames under native Linux 7.1.6-aurora-neo. FFmpeg independently decoded both
320x240 NV12 outputs. Gray: 74.6629 dB PSNR, 82-byte Annex B stream. Color
bars: 70.2696 dB PSNR, 214-byte stream. A second session opened, encoded,
flushed, stopped and closed without rebooting. CPU online mask remained
0-5 and the Vulkan test passed its 32 exact values after encoding.

The multi-frame extension also encoded 60 moving frames in one session.
`artifacts/ave-linux-video60-20260908/validation.json` records matching MP4
and Matroska output: 320x240, 30 fps, two seconds, minimum PSNR 60.18 dB.
Both containers decode to the same pixels as the elementary H.264 stream.

HEVC also passed 60 moving frames with minimum PSNR 63.41 dB and matching
two-second MP4/Matroska outputs. A subsequent 60-frame H.264 session passed
on the same boot, proving a successful switch back between codecs. The
matching source, module, ELF symbols and evidence are frozen under
`artifacts/ave-video-qualified-20260908/`. Both codecs use the Neo's
dedicated AVE hardware.

This is a working experimental hardware path with fixed captured AVC
parameters. It does not register V4L2/FFmpeg encoders, support arbitrary
resolutions or a continuous streaming API, or measure throughput. Encoding executes
on the Neo's AVE hardware; FFmpeg on the controller only decodes the output
for validation. No persistent boot installation changed.

## Current machine state and another encode

Check `video/ENCODER_HANDOFF.md` for the active machine state. The Neo has
returned to native Linux after capturing the HEVC reference. Both codecs
have 60-frame results, including MP4/Matroska and independent decoding.
`video/HEVC_COMMAND_NOTES.md` records the command and output-buffer layout.
The commands below require a configured, idle native Linux boot.
During Linux operation the module and its firmware/DMA/power references
remain pinned until reboot. KIS is free between console commands. Do not
open an m1n1 proxy against the Linux console or unload a started module.
The host is the M3 Max, hpm0/RID0; the other attached Mac
must not be touched. Recheck topology after cable changes.

From `/Users/mcruz/Developer/aurora-silicon`, a new output directory is required:

```sh
python3 neo-linux/video/encode-linux-frame.py \
  neo-linux/artifacts/my-next-encode --pattern color-bars
```

The command selects the next client, checks that the last firmware reply is
Config or Close, reads this boot's DMA arena address, regenerates bounded
packet relocations, verifies uploaded input, encodes and flushes one frame,
retrieves the DMA output, closes the session, decodes with FFmpeg, compares
pixels and runs the CPU/Vulkan checks. It does not reboot or install anything.
An error stops the flow without automatically retrying. Inspect that output
directory before taking further action.

`--pattern gray` selects the first test image. `--input input.nv12` accepts an
exact 115200-byte 320x240 NV12 image (Y stride 320, UV stride 320); custom
inputs remain experimental. Results include `frame.h264`, `decoded.nv12`,
the input, exact packets, relocation manifests, DMA snapshot and logs.
`validation.json` is the final pass/fail evidence, including PSNR and hashes.

For a moving-video session with MP4 and Matroska outputs:

```sh
python3 neo-linux/video/encode-linux-video.py \
  neo-linux/artifacts/my-next-video --frames 60
```

Add `--codec hevc` for HEVC; the default is H.264. Each format uses exact
captured parameters and this boot's explicitly relocated DMA addresses.

The host prepares all packets, then uploads each frame, submits Process and
Complete, and retrieves that frame's output. Timings include these transfers;
they do not represent the hardware's sustained encoding rate.

## Cold RAM boot and initialization

Reuse the existing kernel/Mesa build. Incremental module/DT build only:

```sh
limactl shell neo-builder bash \
  /Users/mcruz/Developer/aurora-silicon/neo-linux/scripts/build-ave.sh
env -u J700_STAGE2_BASE \
  J700_LINUX_BUNDLE=/Users/mcruz/Developer/aurora-silicon/neo-linux/artifacts/linux-ave \
  bash neo-linux/j700-boot-linux.sh
```

The boot helper intentionally reboots into the resident Stage 1, performs a
guarded RAM Stage 2 chainload, validates live firmware reservations and loads
Linux. It preserves the 1024-byte KIS transport workaround and original
compressed length. Do not set a fixed Stage 2 address. Wait for
`NEO_NATIVE_LINUX_SHELL_VERIFIED` before using the console or transfer tool.

Then, sequentially (KIS has one owner):

```sh
neo-update/.venv/bin/python neo-linux/push-linux.py neo-linux/artifacts/linux-ave/apple-ave.ko /tmp/apple-ave.ko
neo-update/.venv/bin/python neo-linux/push-linux.py neo-linux/video/linux-ave/test-native.sh /tmp/test-ave.sh
neo-update/.venv/bin/python neo-linux/push-linux.py neo-linux/video/linux-ave/test-ipc.sh /tmp/test-ipc.sh
neo-update/.venv/bin/python neo-linux/push-linux.py neo-linux/video/linux-ave/run-command.sh /tmp/run-ave.sh
python3 neo-linux/video/ave-make-command.py config neo-linux/artifacts/ave-config-linux.bin
neo-update/.venv/bin/python neo-linux/push-linux.py neo-linux/artifacts/ave-config-linux.bin /tmp/ave-config.bin
neo-update/.venv/bin/python neo-linux/console.py --timeout 30 \
  'sh /tmp/test-ave.sh /tmp/apple-ave.ko && sh /tmp/test-ipc.sh && sh /tmp/run-ave.sh /tmp/ave-config.bin'
```

Require five powered domains, `AVE_NATIVE_IPC_PASS`, and a successful Config
reply before running `encode-linux-frame.py`. Its first cold-boot client will
be 1. After any started-firmware timeout or exception, buffers stay pinned:
diagnose the saved evidence, prepare the complete fix, and use a fresh RAM
boot. Never force-unload or repeat a failed command on the same buffers.

## Changes that enabled encoding

- Full 64-bit mailbox negotiation, bounded ring ownership and command replies.
- The actual captured startup configuration: active units 1/1, mask 1, and
  the **0x24000 page-aligned log allocation**. The internal log header uses
  0x20200; advertising that internal size caused an alignment mismatch.
  Changing the startup scalars together resolved the Config assertion;
  this experiment did not isolate the scalar changes individually.
- All five Linux encoder power domains (`dma`, `pipe4`, `pipe5`, `me0`, `me1`)
  attached and held active. With only DMA/SYS active, AVC Start faulted on
  codec MMIO at 0x285450004. Start succeeded after the domains were powered.
- Explicit relocation of 37 Start surface fields and four Process surface
  fields into a verified 19.5 MiB DMA arena, with input readback and bounds.
- Actual encoded slice length from metadata, plus firmware-produced SPS/PPS.
  No captured macOS encoded bytes are used in the Linux output.

The matching macOS command capture supplies the fixed configuration template;
it is not a generic encoder implementation. Next substantial work is dynamic
AVC parameter construction, streaming buffer/completion management, then a
standard Linux media API. Preserve this working reference while doing that.

Read `/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md`
in full after compaction. The first Linux encoding milestone was delivered
HTTP 204; do not repost it or store the runtime webhook endpoint in artifacts.
