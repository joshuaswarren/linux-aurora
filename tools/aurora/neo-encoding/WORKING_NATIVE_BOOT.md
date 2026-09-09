# Working Neo Linux and Vulkan compute boot

Hardware verified 2026-09-08, with Neo J700 / T8140 / Mac17,5 connected to the
M3 Max J516cAP controller hpm0. The other attached Mac is on another controller
and must remain separate. Rediscover topology if cables or controller hosts
change. Use KIS `/dev/cu.kis-00100000-ch-0`; CDC is not required.

## Proven result

`logs/20260908T175648Z-native-vulkan-smp/` contains the successful run:

- Linux `7.1.6-aurora-neo`, 16 KiB pages, all six CPUs online (`0-5`).
- Interactive DockChannel/KIS shell; advancing uptime and RAM mounts checked.
- Both AGX firmware roles started; Asahi owns `/dev/dri/renderD128`.
- Pinned Mesa Vulkan compute: all 32 exact output values passed.
- Queue-reuse test: 1,000 submissions, 32,000 exact values. Before every
  dispatch, CPU code restored all output sentinels. It reused one Vulkan
  queue, reset the fence, and checked the results after completion.
- Subsequent shell/CPU health checks succeeded. No fault appeared in the
  saved final kernel-log tail. This does not claim full rendering support.

The exact source pins, VM setup and builds are in `BUILDER.md`. Active binary
hashes are recorded in `artifacts/linux/manifest.json` and
`artifacts/m1n1-linux/manifest.json`. The repeat test source is
`video/g17p-compute-repeat.c`, derived from the pinned Mesa compute test;
its ARM64 binary is `artifacts/g17p-compute-repeat`.

## Boot

Close any process owning the Neo KIS channel, then run:

```sh
bash /Users/mcruz/Developer/aurora-silicon/neo-linux/j700-boot-linux.sh
```

The helper now selects guarded Stage 2 placement from each boot's live RAM
layout instead of reusing a fixed address. `J700_LINUX_BUNDLE` optionally
selects another manifest-verified bundle; the baseline remains the default.
The AVE candidate `artifacts/linux-ave/` booted successfully on 2026-09-08
with this helper, retaining the same kernel/initramfs and Vulkan result.
See `video/ENCODER_HANDOFF.md` for current target state and AVE qualification.

The wrapper reproduces the verified sequence, checks artifact manifests and
the controller, reboots into installed Stage 1, RAM-chainloads the separate
Linux Stage 2, boots Linux directly, then loads AGX and checks Vulkan compute.
It writes logs under `logs/`. The complete wrapper passed during the AVE
candidate boot; earlier qualification also verified its individual commands.

Watch the boot console:

```sh
tail -n 40 -F /Users/mcruz/Developer/aurora-silicon/neo-linux/logs/native-live.log
```

After the wrapper exits, issue a Linux command:

```sh
/Users/mcruz/Developer/aurora-silicon/neo-update/.venv/bin/python \
  /Users/mcruz/Developer/aurora-silicon/neo-linux/console.py \
  'uname -r; cat /sys/devices/system/cpu/online; /opt/neo/bin/g17p-vulkan-compute'
```

Only one process may own KIS. `console.py` uses the Linux serial shell, not the
m1n1 proxy protocol. `push-linux.py SOURCE /tmp/NAME` can transfer a file into
the RAM root and requires the target SHA-256 to match; no SSH is needed.

## Required differences from stock helpers

1. Keep the proven 1024-byte KIS transport workaround and guarded Stage 2 base
   selection in `../neo-update/kis-chainload.py`. Reboot before retrying after
   an ADT push. Do not remove overlap checks or pad the raw image.
2. Build a separate Linux Stage 2 with `CHAINLOADING=1`, `T8140_KIS_PROXY=1`.
   Include `build_cfg.h` and skip normal USB initialization at native handoff.
3. Apply `artifacts/m1n1-linux-gpu.patch`. The original GPU fixup returned early
   on T8140 and left four UAT reservations zero. The patch copies the actual
   ADT regions after checking nonzero, alignment, overflow and table size.
4. Boot the Linux Image directly (`--direct`). U-Boot reached native execution
   but encountered peripheral initialization failures before Linux.
5. Start all secondary CPUs but retain the original boot CPU
   (`--keep-boot-cpu`). Switching from CPU 4 to CPU 0 caused an ADT/MMU fault.
6. Include the Vulkan loader explicitly: these programs use `dlopen`, so
   packaging only dependencies reported by `ldd` missed `libvulkan.so.1`.

The old raw `asahi-submit-g17p-add3` diagnostic faulted at GPU virtual address
`0x10000000480`, then output zero. Its recovery left subsequent submissions
failing. Do not run it ahead of the verified Vulkan gate. The earlier
`asahi-vulkan-g17p-enumerate` bounded-profile assertion also does not match
the current driver. Neither is substituted for actual output validation.

## Storage and remaining work

This is a RAM development system, not a persistent Linux installation or a
desktop. No internal Linux partition was created or mounted. Persistent
Stage 1 is unchanged; never install a qualification Stage 2 there. The known
macOS 25G76 hypervisor recipe and artifacts remain in `../neo-update/`.
Do not restore native boot or apply the staged 25G83 update.

The current org tree still has incomplete fragment rendering, three Gallium
build errors and nine failing CPU-side Mesa test programs. This qualification
establishes the Vulkan compute path only. AVE hardware encoding is not yet
implemented; its inventory and initial reference are under `video/` and
`artifacts/encoding-reference-20260908/`.
