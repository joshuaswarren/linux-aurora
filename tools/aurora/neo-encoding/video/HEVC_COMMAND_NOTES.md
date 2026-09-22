# J700 HEVC command evidence

Reference: `../artifacts/ave-hevc-capture-20260908/`, captured from the
Neo's hardware-required VideoToolbox session under the verified macOS
25G76 kernel. Three moving 320x240 NV12 frames at 30 fps, 1 Mbps, Main
profile, no reordering. Independent FFmpeg decoding passed at 66.22 dB
PSNR. All 31 IPC sends paired with channel sends and ring publications;
capture disarmed with zero pending sends and no errors. The Linux C ring
codec matched every published descriptor and index transition.

Native Linux HEVC now passes a three-frame session with decoded MP4 and
Matroska output. Evidence: `../artifacts/ave-linux-hevc3-20260908/validation.json`.
Per-frame PSNR is 68.32, 63.41 and 69.48 dB. Each Linux-produced slice is
byte-identical to the corresponding independent macOS reference slice.
All commands, Stop/Close, six CPUs and Vulkan passed. The 60-frame extension
also passed (`../artifacts/ave-linux-hevc60-20260908/validation.json`):
MP4 and Matroska, 30 fps, two seconds, minimum PSNR 63.41 dB. A subsequent
60-frame H.264 session passed on the same boot. The preloaded firmware TEXT hash equals the
qualified Linux/AVC reference, `aeb4671e...7cc197`.

The existing command numbers apply: Config 1, Open 3, Start 6, Process 8,
Complete 9, Stop 7, Close 4. Common byte +0x1c is codec 2 instead of AVC 1.
Complete/Stop/Close byte +0x2c is 0x20 instead of 0x140. Open keeps 0x1e0.
Replies have the same layout, including Process response subtype 0x15.

Start is 0x13f28 bytes, Process 0xb1c0. The Linux command staging area now
accepts up to 128 KiB and checks its end against the arena start. Channel-1
ACK payload lengths are zero, with the original command address. Response
storage bounds apply to channel 2. An initial, incorrect equality check
against submitted length rejected Config; `hevc-boot-init.log` records it.
The earlier qualified native AVC logs prove the correct zero-length ACK.

`ave-relocate-hevc.py` verifies all four captured packet hashes before
generating any packet. Its explicit descriptor list covers 31 Start surface
addresses. Surface span is 0x350000 from captured DART base 0x100002a8000.
Process contains a 256-entry address table at +0xd88, 8 bytes per entry,
with captured addresses 0x100003e0000 + index * 1024. Preserve the table's
alias relationships inside the owned arena; do not scan and replace every
word resembling an address. Scalar values elsewhere resemble DART pointers.

Process fields: Y +0xa300, UV +0xa320, bitstream +0xac88, metadata +0xac90.
Y and UV sizes are 76800/38400, stride 320. The input is relocated to arena
+0x400200; output and metadata keep arena offsets +0x48000 and +0x108000.
Start descriptors prove output capacity 0x3c000 and metadata capacity 0xc000.

HEVC output is split: the bitstream surface contains CABAC payload, while
tile-table entry 0 (arena +0x138000) contains the Annex B slice header.
Metadata +0x120/+0x12c both report payload bytes; +0x128 reports header
bytes (16, 19, 17 in the first three frames). Join the live Linux header
and payload. VPS/SPS/PPS are in metadata, starting at +0xb2a4. No macOS
encoded data is copied into the Linux result. The first extraction attempt
incorrectly expected the Annex B prefix at the payload start; the saved
buffers were reassembled and validated without rerunning hardware commands.
`validate-linux-video.py` supports this offline validation workflow.

All three Process packets differ only in local request/time fields, the
first-frame flag +0x5d90, input addresses, ordinal +0xacf8, and presentation
counter +0xad40. Counter values are 0, 480000, 960000 at captured 30 fps;
its unit is not established. The generator checks those captured values.

The corrected HEVC-capable Linux module is SHA-256
`9432ecf841f2eb5398a56c8df2b4d04321b71e267b39f45361ddef3bbee15245`.
The initial `321e67f5...` candidate contained the incorrect ACK check and
must not be treated as qualified. There are 637 ACK observations in the
qualified 60-frame AVC logs; every one has zero payload bytes.
Incremental build and DT validation passed. Common commands match captured
bytes after local token/time replacement; the revised H.264 extractor
reproduces its previously qualified first frame byte for byte.

Current Linux RAM boot: `../logs/20260908T230200Z-native-linux/`, launcher
`../logs/boot-linux-ave-hevc-zero-ack.log`. The H.264 regression closed
client 1, HEVC three-frame session closed client 2, HEVC 60-frame session
closed client 3, and H.264 60-frame session closed client 4. Firmware is
configured and idle; KIS is free. Another encode selects client 5.
The macOS HV controller 55332 is closed.

Read the full `../../neo-update/PROGRESS_WEBHOOK_GUIDE.md` after compaction.
The 60-frame H.264 milestone was delivered HTTP 204; do not duplicate it.
