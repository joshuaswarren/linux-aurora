# Neo ProRes hardware diagnostic

The native Linux module drives the dedicated T8140 APR encoder. ProRes 422
has passed one reference frame and 60 changing 320x240 frames. See
`../OTHER_CODECS.md` for the authoritative current result and machine state.
This is a fixed-format experiment, not a standard V4L2/FFmpeg encoder.

The successful initialization installs the descriptor ring before MODE 0xd
enables the engine. All packet addresses are replaced with Linux-owned DMA
buffers behind DART. The ring advances by 384 bytes per frame. Submission
waits at most 5 seconds for completion, checks the ring tail and ProRes frame
header, and refuses any retry after a hardware error. Once started, the
module, buffers and power domain stay pinned until a full target reboot.

Rebuild only the module and DT in the dedicated VM:

```sh
limactl shell neo-builder bash /Users/mcruz/Developer/aurora-silicon/neo-linux/scripts/build-prores.sh
```

From the workspace root, a cold RAM boot uses the qualified baseline kernel
and initramfs, with the additional APR nodes. It reboots the Neo, so first
verify that no controller or experiment owns KIS:

```sh
env -u J700_STAGE2_BASE J700_LINUX_BUNDLE=/Users/mcruz/Developer/aurora-silicon/neo-linux/artifacts/linux-prores bash neo-linux/j700-boot-linux.sh
neo-update/.venv/bin/python neo-linux/push-linux.py neo-linux/artifacts/linux-prores/apple-prores-test.ko /tmp/apple-prores-test.ko
neo-update/.venv/bin/python neo-linux/console.py 'insmod /tmp/apple-prores-test.ko && cat /sys/kernel/debug/apple-prores-test/status'
python3 neo-linux/video/encode-prores-linux.py \
  neo-linux/artifacts/prores-linux-attempt-20260909/frame-000.bin \
  neo-linux/artifacts/other-codecs-reference-qualified-20260909/prores-native-reference \
  neo-linux/artifacts/my-prores-test --frames 60
```

For a healthy already-started ProRes module, omit the boot/load steps and
add `--warm` to the encode command. Every output directory must be new.
The diagnostic currently permits only as many complete 384-byte entries as
fit before the 128 KiB ring wraps; it returns ENOSPC instead of guessing the
hardware wrap protocol. The current 62-frame run still has ample room.

The packet is derived from a bounded macOS reference capture. The capture
contained 16 identical chroma rows, used to reconstruct the constant-row
test image; the native first decoded frame matches the independent macOS
reference exactly. Subsequent test frames shift the verified input rows.
The controller's FFmpeg only decodes and muxes the hardware output.

Saved output includes a ProRes MOV, individual raw ProRes frames, original
and decoded pixels, exact packets, native command/status logs, and PSNR
results. Runtime includes KIS transfers and validation; it is not an encoder
throughput benchmark. Stage 1 and persistent storage are unchanged.
