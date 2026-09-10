# M5 hardware video encoding

These tools adapt the Neo AVE tracing method to J813/T8142/Mac17,3 and the
exact macOS 25G76 kernel. Native Linux H.264 encoding passed on the physical
M5 on 2026-09-10: separate color-bar, gray and gradient 320x240 NV12 inputs
encoded, decoded independently in FFmpeg, and matched at 54.1, 74.7 and
54.0 dB PSNR. All three sessions closed successfully with no command or
DART errors. The gradient test passed with Wi-Fi and Bluetooth active;
gray and gradient inputs were absent from the macOS reference capture.
The final boot also passed Wi-Fi DHCP and five pings through wlan0,
Bluetooth's 90-command startup with no transport errors, keyboard/trackpad
registration and XFCE startup. See `M5_NATIVE_RESULT.json` for the results.
The initial experimental encoder used debugfs. The new
[V4L2 front end](V4L2.md) supports stock FFmpeg's `h264_v4l2m2m` and OBS's
built-in FFmpeg output through `/dev/video0`, including ordinary desktop-user
access and direct MPEG-TS timestamps. This remains a fixed 320x240/1 fps,
all-IDR profile. Configurable encoding, HEVC, higher resolutions and sustained
video throughput are not yet qualified. GPU rendering is independent of AVE.

The earlier OBS Studio recording was qualified through the dedicated
[`m5_ave_h264` adapter](obs/README.md): a ten-second MKV at 320x240/1 fps
preserved the hardware's H.264 packets exactly and independently decoded
at 51.4–54.3 dB PSNR against actual OBS input. This all-IDR test uses AVE
for encoding and llvmpipe for OBS compositing; it does not qualify normal
video frame rates or larger resolutions.

The V4L2 path removes that custom encoder and userspace broker. Its OBS test
helper only creates a synthetic scene and starts/stops recording. See
`M5_V4L2_RESULT.json` for hardware results, regressions and explicit limits.

## Native Linux experiment

`linux-ave/` contains the M5-specific module and device-tree supplement.
`build-m5-ave.sh` builds against the existing desktop-v7 kernel in the
J813 builder VM. `check-m5-dtb.py` verifies that the candidate preserves
all existing desktop DT properties, accounting for phandle renumbering.
The matching native m1n1 carrier must reserve both preloaded AVE firmware
segments; `boot-linux.py --video` verifies them before preparing the DT.
Use guarded RAM chainloads and preserve the resident boot image.

Transfer the module and scripts using the workbench's `linux-upload.py`.
Run `test-native.sh` once to qualify mapping/load-unload-reload before
firmware startup, then `test-ipc.sh` once to start firmware. Config is
generated with `ave-make-command.py config FILE` and submitted once with
`linux-ave/run-command.sh`. Check the successful reply before encoding.

From the controller, with firmware configured and idle:

```sh
python video/encode-linux-frame.py artifacts/video/NEW_RESULT --pattern color-bars
python video/encode-linux-frame.py artifacts/video/ANOTHER_RESULT --pattern gray
```

The controller checks every command reply, reads back the input buffer,
captures Linux DMA output, extracts H.264 parameter sets and slice bytes,
decodes them with the controller's CPU FFmpeg, and compares all NV12 pixels.
It accepts an optional exact 115200-byte `--input` for the same profile.
Each invocation opens and closes its own firmware session. The module and
buffers are pinned until reboot once startup is requested, including on
failure; do not unload or retry a failed firmware command on the same boot.

`ave-relocate-avc.py` consumes the private qualified guest42 capture and
checks hashes plus corresponding channel publications. It relocates 56
explicit Start surface records and four Process pointers into Linux-owned
memory. M5 uses a 0x528000 surface extent, with input at offset 0x600200.
Neo's smaller surface layout must not be reused. No captured firmware,
runtime packet binaries, input/output data or private logs are published.

## macOS reference

On 2026-09-10 that macOS reference passed on the physical M5: three distinct
frames, hardware use confirmed by the session property, independent CPU
decoding, and 54.1–60.8 dB PSNR against source pixels. The trace contained
31 64-bit ring publications with no errors or pending sends. The portable
descriptor codec compiled and ran inside an aarch64 Linux VM; every captured
descriptor and index transition matched, and invalid input cases were
rejected. The native experiment above uses this validated reference.

Use these files as the workbench's `video/` directory beside its existing
`hv/` directory. The private kernel, captured native boot policy, volume
inventory and qualified carrier-32 build are prerequisites; they are not
redistributed here. `launch-m5-video-hv.py` verifies their expected local
paths and pins the kernel, carrier and raw ELF used by the SPTM seed.

After the guarded carrier RAM chainload releases the M5 KIS link:

```sh
python video/launch-m5-video-hv.py hv/logs/NEW_PRIVATE_RUN
```

The command retains interactive stdin and writes console output privately.
In another host terminal, use `hv/wait-native-network.py` on that directory.
It checks the guest identity and starts a temporary sleep assertion. Once
successful, `video/capture-m5-h264.py` on the same directory uploads the
signed probe, waits for trace arm acknowledgement, runs the short encode,
disarms and checks pending ring sends, then retrieves the private output.
It uses SSH only; the HV process remains the sole serial/proxy owner.

Build the probe on macOS:

```sh
clang -arch arm64 -O2 -fobjc-arc -Wall -Wextra -Werror \
  video/m5-vt-codec-probe.m -framework Foundation -framework VideoToolbox \
  -framework AVFoundation -framework CoreMedia -framework CoreVideo \
  -o artifacts/video/probes/m5-vt-codec-probe
codesign -s - --force artifacts/video/probes/m5-vt-codec-probe
```

The probe refuses another Mac model. `list FILE.json` inventories encoders;
`encode avc1 NEW_DIRECTORY 3 nv12` requests the initial hardware-only test.
Independent decoding and pixel comparison are still required after a probe
pass. A macOS reference does not prove native Linux encoding.

The launch recipe retains the successful M5 headless baseline: all physical
CPU ADT nodes with `cpus=1`, the exact boot arguments, selected native macOS
volume and matched resident policy, display/camera isolation, MTP/ANE guest
exclusions, NVMe queue correction and the signature-guarded CPU accumulator
restore diagnostic. That last setting is `M5_SKIP_CPU_ENERGY_RESTORE=1`;
omitting it reproduced ApplePMGR SError before AVE activity. CPU energy
accounting is not qualified. Resident Stage 1 and installed disk contents
are preserved; experiments use RAM chainloads.

The AVE survey observes only guest writes to its main and wrapper registers.
Its translated ranges are checked against the M5 ADT, with resident AIC/MTP
cross-checks. Shared PMGR windows and large apertures remain metadata only.
Command/ring observers use four exact-instruction hardware breakpoints,
validate bounded memory reads through translations into DRAM, and disarm on
errors or limits. M5 allocation tags are normalized for reads; PAC and user
pointers remain invalid. Host phase/state acknowledgement comes from the
observer's live namespace. Input-plane DMA snapshots remain disabled until
M5 command offsets are qualified.

The M5 firmware segments captured before guest start have lengths 0x14c000
and 0x12c000; the second differs from the Neo driver's 0x130000 expectation.
The initial M5 driver profile must use its own firmware and measured topology.
Keep raw firmware, kernel caches, traces, network receipts and generated
video in private artifact directories outside this source package.
