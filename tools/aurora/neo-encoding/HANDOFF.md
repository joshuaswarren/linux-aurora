# Neo Linux and hardware media encoding bring-up

Scope update (2026-09-09): hardware audio encoding is also part of the target.
See `audio/HARDWARE_ENCODING.md` for the evidence gates and pending inventory.
No Neo hardware audio encoder has been enumerated or qualified yet. The video
coverage audit in `video/OTHER_CODECS.md` also tracks HEVC alpha/depth/disparity,
10-bit/4:2:2 profiles, and the unresolved ProRes RAW encode question.
Read `A18_ENCODING_TARGET_AUDIT.md` for the Apple source audit: core targets
remain H.264, HEVC, ProRes variants and device-proven JPEG; add MV-HEVC and
AAC-LC discovery, and inventory APAC without assuming hardware execution.

Current campaign (2026-09-09): **native Linux ProRes 422 now passes 60 moving
frames**, including independent decoding and CPU/Vulkan checks. Read
`video/OTHER_CODECS.md` first for the live state and exact artifacts. The Neo
is back in native Linux in `logs/20260909T033655Z-native-linux/`, with the
ProRes module pinned and 62 completed frames. H.264 and HEVC each passed
three-frame regressions on the same boot, and ProRes passed again afterward.
Both AVE sessions closed; the next AVE client is 3. KIS is free between Linux
console commands. Do not reuse client numbers from the old native run below.

Qualified checkpoint (2026-09-08): **native Linux hardware H.264 and HEVC both
encode and decode correctly in the experimental AVE path.** Both codecs
passed 60-frame MP4/Matroska tests: 320x240, 30 fps, two seconds. H.264 also
passed after switching back from HEVC on the same boot. CPU/Vulkan checks pass. Read
`video/WORKING_LINUX_ENCODING.md`, `video/ENCODER_HANDOFF.md` and
`video/HEVC_COMMAND_NOTES.md` before acting; current-state sections supersede
historical attempts below. The completed native run was
`logs/20260908T230200Z-native-linux/`. The module and DMA/power references were
pinned until the intentional capture reboot. Standard V4L2/FFmpeg encoder
integration and a continuous streaming API remain unfinished. Preserve
Stage 1 and use KIS console commands.

Active objective: develop hardware video and audio encoding on native Linux
on J700 / T8140 / Mac17,5, preserving the verified Linux boot and AGX compute.
Use the dedicated `neo-builder` VM for builds. See `BUILDER.md` for pinned
Linux/Mesa/U-Boot revisions, build commands, file locations and test results.

**Historical capture campaign:** macOS 25G76 AVE2 command capture completed.
The Neo has since returned to native Linux and encoded the frames above.
Do not repeat those AVE captures to demonstrate the completed H.264/HEVC tests.
The current macOS session is a separate ProRes/JPEG investigation requested
after that qualification.

**Qualified Linux state:** all six CPUs, AGX loaded and Vulkan compute verified.
The first 32-value test passed; then
1,000 queue-reuse submissions passed 32,000 exact values. The boot controller
had exited before the AVE capture switch. The complete working Linux recipe
is `WORKING_NATIVE_BOOT.md`; do not restart the earlier failed experiments.

## Preserved working reference

The macOS 25G76 KIS hypervisor recipe is saved in
`../neo-update/WORKING_KIS_BOOT.md`. Its host controller PID 15683 was stopped
intentionally on 2026-09-08 for native Linux qualification. Do not assume that
old macOS guest is still running. Its original Stage 2 artifacts and source
worktree remain intact. Persistent Stage 1 is unchanged. Do not install any
qualification Stage 2 as Stage 1, restore native boot, or apply the staged
macOS update.

Host VDM service: `/Users/mcruz/.local/bin/macvdmctl`, confirmed hpm0 on this M3
Max. Neo KIS channel is `/dev/cu.kis-00100000-ch-0`. Another Mac is attached on
the other controller; do not touch it. Rediscover topology if cables change.

## Current artifacts and evidence

- Linux `7.1.6-aurora-neo` build exited 0, ARM64, 16 KiB pages. Image, DTB,
  matching vmlinux, configuration, System.map and initramfs are under
  `artifacts/linux/`, with SHA-256 manifest.
- Mesa Vulkan built and staged successfully. CPU-side Asahi tests: 99 passed,
  9 failed. Those failures are unresolved, not hardware results. The same
  branch also has three Gallium compile errors; the active profile is Vulkan.
- U-Boot J700 stock build reached native execution on the actual Neo.
  Stock artifacts are retained in `artifacts/u-boot-stock/`. The active
  `artifacts/u-boot/` now uses `ENV_IS_NOWHERE=y`, `ENV_IS_IN_FAT=n` for RAM boot.
- Native Linux Stage 2 is separate under `artifacts/m1n1-linux/`. Source is in
  `~/neo/sources/m1n1-linux-kis` inside the VM. Base commit is the verified J700
  m1n1 pin; source patches live in `artifacts/m1n1-kis-base.patch` and
  `artifacts/m1n1-linux-kis.patch`. Build uses Clang/LLD 21, `BUILDSTD=1`,
  `CHAINLOADING=1`, `T8140_KIS_PROXY=1`. The latter requires explicitly including
  `build_cfg.h` in kboot.c so normal USB is skipped at native handoff.
- `artifacts/m1n1-linux-gpu.patch` passes through the four T8140 UAT memory
  regions from live ADT. This fixed the hardware-proven zero-region handoff.
- Updated initramfs includes the explicit Vulkan loader and Vulkan compute
  tests. Initial RAM root is preserved in `artifacts/linux-initial-root/`.

## Hardware attempts

1. `logs/20260908T170813Z-native/`: Stage 2 KIS worked and all four payloads
   matched full readback. All six CPUs started, but switching the boot CPU from
   4 to 0 was followed by an EL2 ADT read fault before device-tree preparation.
   PC offset `0x7966c` maps to Rust `ADTNode::from_path_trace`; FAR
   `0x10005568000`, ESR `0x96000002`. Exact cause is not proved.
2. `logs/20260908T171329Z-native-single-cpu/`: Keeping original boot CPU 4
   passed device-tree preparation and reached U-Boot. The last U-Boot output
   was loading its environment from FAT and adopting the inherited NVMe RTKit
   session. KIS then disconnected. U-Boot had enabled its 60-second watchdog.
   Do not yet claim the NVMe driver itself is the proven cause.
3. `logs/20260908T172350Z-native-ram-env/`: RAM environment loaded, then U-Boot
   suffered asynchronous SError after `mtp_kbd_probe`. The PC was in a timer
   read; this does not establish the timer instruction as the cause.
4. `logs/20260908T173233Z-native-direct/`: **Native Linux shell verified**.
   `boot-linux.py --stage2-base 0x10003b74000 --single-cpu --direct` bypasses
   U-Boot and preserves the original boot CPU. Kernel 7.1.6-aurora-neo,
   CPU online `0`, advancing uptime, and RAM filesystem mounts passed.
   The controller exited 0 and closed KIS. AGX module load reaches G17P MMU
   initialization but device probe fails EINVAL; no `/dev/dri` exists yet.
   See `agx-probe.log`. Module insertion returning 0 is not GPU success.
5. `logs/20260908T174458Z-native-gpu-regions/`: GPU UAT fixup verified. Both
   firmware roles started and the render node registered. The old raw add3
   diagnostic faulted at GPU VA `0x10000000480` and output zero. Subsequent
   Vulkan submissions in that faulted session returned EIO. Missing Vulkan
   loader packaging was also found and fixed. Do not use raw add3 as the
   current qualification gate.
6. **Current successful run:** `logs/20260908T175648Z-native-vulkan-smp/`.
   `--keep-boot-cpu --direct` starts secondaries without switching the primary.
   Linux reports CPU `0-5` online; all six started at EL2. `vulkan-first.log`
   proves 32 exact values. `vulkan-repeat-1000.log` contains 1,000 individual
   PASS lines plus `NEO_VULKAN_REPEAT_PASS submissions=1000 exact_values=32000`.
   The bounded queue-reuse test restores output sentinels each submission.
   `post-compute-health.log` confirms a responsive shell and CPUs still online.

`logs/latest-native-run.txt` names the current attempt. `logs/native-live.log`
is a symlink for `tail -F`. The boot controller has exited. Only one
owner may open the Neo KIS channel at a time.

## Next steps and useful commands

The current Linux shell has `NEO_NATIVE_LINUX_SHELL_VERIFIED` in `console.log`.
`boot-linux.py` sends a bounded verification command after the initramfs
prompt, then closes the serial channel. After that,
`console.py` can issue plain shell commands over KIS; it must not overlap a
boot controller. It sends no m1n1 proxy requests to a running Linux shell.

The RAM root includes BusyBox, kmod, the exact AGX module, Vulkan loader/driver,
vulkaninfo and compute/offscreen test executables. GPU module loading is
explicit (`modprobe asahi`). The working gate is `g17p-vulkan-compute` and
the saved `video/g17p-compute-repeat.c` test. The latter is currently installed
in RAM as `/tmp/g17p-compute-repeat`; its binary is in `artifacts/` and can be
copied back with the SHA-verifying `push-linux.py` after another boot.

Always reboot before retrying a chainload after ADT push. Keep all overlap
guards in `../neo-update/kis-chainload.py`; use 1024-byte transactions and
original gzip length. The selected Stage 2 base is `0x10003b74000`, still
subject to the live loader guards. Keep the host GNU stub prefix nonempty.

The device tree still reports ATC tunable errors; full USB/peripheral support
is unverified. Fragment rendering and Gallium are not qualified. Preserve the
working compute session instead of running known-failing render diagnostics.

For video encoding, the captured macOS inventory is
`artifacts/macos-ave-inventory.txt`: T8140's `ave2` node and active
AppleAVE2Driver with H.264/HEVC capabilities. The checked Linux branch has AVD
decoding and ISP support but no Apple AVE encoder driver. AGX is the graphics
and compute target; AVE is the dedicated hardware encoder to investigate.

## Progress contract

Read `/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md`
in full after compaction. The endpoint is runtime-only and must not enter
artifacts. Initial Linux campaign amber and native U-Boot milestone amber
were sent successfully; don't duplicate them. A green native Linux shell
milestone was sent after hardware verification (HTTP 204). A second green
milestone confirmed all six CPUs and 1,000 Vulkan submissions (HTTP 204).
Encoding is not implemented yet. The AVE hardware map and validated 60-frame
software reference from the build VM are saved under `artifacts/`; see
`video/README.md`. No AVE MMIO writes or encoder firmware experiments have
been performed. No persistent Linux installation exists yet.
