# OBS recording with the M5 AVE encoder

The experimental `m5_ave_h264` OBS encoder sends actual OBS NV12 frames to
the native M5 AVE driver through a bounded local broker. There is no software
video-encoder fallback. Three physical J813 recordings each produced a
ten-second MKV containing ten 320x240 H.264 frames at 1 fps. Independent CPU
decoding matched the OBS input at 51.4–54.3 dB PSNR, and every recorded IDR
slice was byte-identical to its hardware output. The final recording
contained six distinct input images. See [the recorded results](M5_OBS_RESULT.json).

This adapter is a fixed-profile experiment, not general V4L2 support. It
currently accepts only **320x240 NV12 at 1 fps**, and starts a fresh AVE
session for every frame, producing all-IDR output. Normal video frame rates,
larger resolutions, inter-frame compression, HEVC and streaming are not
qualified. OBS compositing uses llvmpipe on the current M5 desktop; the video
encoding itself uses AVE. The test source is animated color bars inside OBS.
Desktop/window capture is untested: the current fbdev session lacks
XComposite. OBS also reports delayed cleanup of the fixture's color sources
on exit; the log confirms their destruction when the main canvas closes.
This warning occurs after recording has stopped and the MKV is complete.

## Components

- `ave-broker.py` runs as root in the RAM desktop. It owns the debugfs
  interface and exposes only `/run/j813-ave-obs.sock` to desktop group 1000.
  It checks boot identity, relocated-template hashes, every command reply,
  DMA input readback and exclusive command counts. A firmware failure ends
  the test without retry. Its default ceiling is 24 OBS frames.
- `obs-m5-ave.c` registers the encoder in OBS. It sends NV12 planes and PTS
  through the socket, then returns the hardware slice and real parameter
  sets to OBS for muxing. `M5_AVE_AUTOTEST=1` additionally creates an animated
  scene and requests recording stop after eight hardware frames. OBS may
  drain additional frames before stopping; the validated first run encoded
  eleven frames and muxed ten, plus a separate hardware warmup frame.
- `setup-test-profile.py` builds the plugin with warnings as errors and
  creates a separate OBS profile with the tested encoder, resolution and
  frame rate. It preserves existing profiles and accepts `--run-name` for
  a fresh repeat test.
- `validate-recording.py` checks the MKV with CPU FFmpeg, matches its actual
  IDR payloads to the saved AVE output, and compares all decoded NV12 pixels
  against the input supplied by OBS. The result includes per-frame PSNR.
- `run-test.sh` coordinates a fresh profile, one broker and one OBS instance
  from the initramfs shell. It waits for recording to stop, exits OBS, and
  checks that the broker finished successfully. It does not reboot the M5.

## Reproducing the bounded test

First boot the qualified video candidate, start/configure AVE, and verify
that the last firmware reply is Config or Close. Do not unload the module
after firmware startup or use another AVE command writer concurrently.
All M5 KIS upload/download/command operations must be sequential.

Install `obs-studio`, `libobs-dev`, `gcc`, `libc6-dev` and `pkg-config` in the
RAM desktop. The tested package version is OBS 32.1.0. Use the existing
`ave-relocate-avc.py` to generate this boot's `start.bin` and `process.bin`
from its measured `arena_dma` and `arena_bytes`. The private `manifest.json`
must contain those two numeric values, this boot's
`/proc/sys/kernel/random/boot_id`, and a `sha256` object with `start` and
`process` hashes. Never reuse a manifest after reboot. Capture binaries and
firmware are not distributed in this source directory.

Transfer the templates, manifest and adapter sources into
`/desktop/tmp/m5-obs` and put `run-test.sh` in `/tmp/m5-obs-run.sh`. From the
initramfs shell, the coordinated test is:

```sh
sh /tmp/m5-obs-run.sh m5-ave-obs-test m5-obs-ave-result
```

Use fresh names beginning with the same prefixes for repeats. For manual
operation instead, run setup as root inside `/desktop`, then start the
broker with a new result directory:

```sh
python3 /tmp/m5-obs/setup-test-profile.py --run-name m5-ave-obs-test
python3 /tmp/m5-obs/ave-broker.py /tmp/m5-obs /run/NEW_OBS_RESULT
```

Start OBS as the desktop's `aurora` user through its existing session
namespace helper, retaining `DISPLAY=:0`, `XAUTHORITY`, `XDG_RUNTIME_DIR`
and the llvmpipe environment. Set
`XDG_CONFIG_HOME=/home/aurora/.config/m5-ave-obs-test` and
`M5_AVE_AUTOTEST=1`. Select the `M5 AVE hardware test` profile. The helper
must remain running separately while OBS records. No desktop or microphone
capture is added by this synthetic fixture.

After OBS reports Recording Stop, exit the test application to release the
encoder connection, then retain the broker's JSON, input NV12 files, hardware
H.264 files, OBS log and original MKV. Do not treat plugin enumeration or a
recording-start message as a successful encoding test. On the controller:

```sh
python3 video/obs/validate-recording.py PRIVATE_EVIDENCE_DIRECTORY
```

The implementation follows the public [OBS encoder API](https://docs.obsproject.com/reference-encoders)
and the matching [32.1.0 frontend API](https://github.com/obsproject/obs-studio/blob/32.1.0/frontend/api/obs-frontend-api.h).
