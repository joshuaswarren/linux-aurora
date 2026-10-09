# J700 native display checkpoint — 2026-09-10

This is a development checkpoint, not a claim of working native DCP/GLX.

## Source and attribution

The `j700` history retains the original contributor commits. The GPU/display
merge is `48ff22970bd9a172e36b846a41ab01c8e48d56d7`, including `drm-agx3`
`feaeef21ab812797fba5dca793644e80ce83f920` and `neo-dcp`
`276889114e900ebb8ba9a359f0793b67a6e2aaec`. Ryan Murray's Wi-Fi/PIODMA work,
Liam's speaker work, and DjDeveloperr's GPU/display/input work remain in
their original history. New local integration commits identify the work they
extend; they do not replace the original authorship.

The two inherited-firmware experiments are `a056dfec99e4` and `6399a02923ba`.
The first credits DjDeveloperr's IOP shutdown protocol evidence. The second
adds the type `0xC` idle fence observed in pinned macOS 25G76 RTBuddy.

`neo-dcp` subsequently advanced to `bf8e5b83d722ea511d8e63ba371531402b2a66db`.
That commit was fetched and reviewed, but is not part of this checkpoint.

## What was tested

- The merged kernel with the carried GPIO, DockChannel, DART, mt76 and PIODMA
  changes built successfully: Image, J700 DTB and all modules. Toolchain:
  Clang 21.1.8, Rust 1.93.1 and bindgen 0.72.1.
- The adapted C1FE parser harness passed AddressSanitizer and
  UndefinedBehaviorSanitizer, including short/malformed input allocations.
  Three older source-shape tests remain stale against the incoming driver.
- Trackpad operation was confirmed on the physical Neo. The restricted Wi-Fi
  profile associated successfully and provided SSH with private runtime files.
- Both RTKit candidate kernels compiled and were tried through guarded RAM
  boots. The metadata handoff allowed DCP to get past framebuffer lookup.
  IOP OFF acknowledgement and the idle fence arrived, and ASC stopped, but
  firmware restart produced no new RTKit HELLO and failed with `-62`.
- After these probes, the earlier software XFCE boot was restored. All six
  CPUs executed an affinity check, Wi-Fi/SSH worked, Asahi was loaded, and
  no hardware fault signatures appeared in the restored validation log.
  `appledrm` was unloaded; simpledrm/fbdev drove the desktop. VirtualGL was
  not running. This restoration did not validate native accelerated Xorg.

## Reproduction material

`native-context-test.c` is a standalone EGL device diagnostic. It selects an
explicit render node, requires an Apple/AGX/Asahi renderer, draws and reads
60 changing frames, then destroys the context. It compiled for ARM64 but
has **not** been run on the Neo. Example build on the Linux builder:

```sh
cc -O2 -Wall -Wextra native-context-test.c -o native-context-test \
  $(pkg-config --cflags --libs egl glesv2)
```

Run it only in a deliberate hardware session after checking device ownership.
Run separate processes to exercise reopening; one process is not sufficient.
Stop and reboot after a firmware/DMA fault instead of retrying a damaged
device. No hardware diagnostic is run automatically by this directory.

`asahi-one-render-pair.patch` preserves the VM-only experiment used by the
running Asahi module. It is **not applied** to this branch's driver defaults.
It limits the existing queue ownership machinery to one render pair and does
not fix the later GPU firmware translation fault during client reopening.
The deployed diagnostic module therefore differs from an unmodified build
of this branch. Its SHA-256 is
`b1e2622874d5f8d307798d254e2d06b9ffd0970d0f582b9dcbf00ff220a79ab7`.
The saved patch uses zero context; check it with
`git apply --check --unidiff-zero tools/j700-native-display/asahi-one-render-pair.patch`
before applying it deliberately in an isolated diagnostic build.

Matching binaries, symbols, configurations, patches and private capture logs
are retained in the local campaign archives `j700-gpu-update-20260910`,
`xfce-agx-20260910` and `native-desktop-20260910`. Firmware dumps, factory
calibration, credentials and network configuration are not included here.

## Remaining gates

1. Establish the complete inherited H17P reset/boot or live-session ownership
   protocol. OFF plus idle acknowledgement alone did not restart firmware.
   Do not label the handoff quiesced when no quiesce occurred, or weaken the
   existing ownership and DMA guards.
2. Diagnose the GPU firmware translation fault on the next client generation.
   The firmware crash precedes the queue retirement timeout; extending that
   timeout is not an established fix.
3. Qualify repeated native EGL contexts, DCP scanout, and native GLX in XFCE
   without VirtualGL. Then repeat the compute and encoder regressions.

Preserve resident Stage 1 and pinned macOS 25G76. Use the existing guarded KIS
RAM loader with 1024-byte transport transactions and overlap checks. This
checkpoint performs no persistent installation or hardware state changes.
