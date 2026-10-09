# Dense MRT integration checkpoint

> EOT interpretation corrected: see EOT-PARTITION-CORRECTION.md and IMPLEMENTATION-AUDIT.md. EOT physical partitions are offset/sample-stride quotients. Earlier EOT class-sequence and cross-partition conclusions below are historical and superseded; fragment synchronization evidence remains separate.

Current candidate `libgallium-dense-mrt-ra.so`, SHA256 `4356d88ba1e6e59eba1cc79e23289abf00483a63582f93a60890109614890f3d`, is installed in the private CTS prefix. Source changes are uncommitted in mesa-j700. Resident m1n1, SSD layout, and the other machine are unchanged.

## Implemented

- Admit contiguous, single-sample, non-layered three-to-eight RGBA8/BGRA8/X8 UNORM targets with the allocator's exact offsets, sample size and tile budget. Holes, spilling, mixed formats and unsupported geometry still fail closed.
- Capture complete native cross-partition EOT builtins for all dense counts, using partition IDs 0,1,2,0,... . Text lengths for counts3..8 are396,554,722,890,1058,1226; staging half counts16,16,24,32,40,48. `make-native-mrt-header.py` validates compiler metadata and hashes before generating `src/asahi/lib/agx_g17p_wide_mrt_eot.h`.
- Bind every PBE and its imageblock offset using the native ESL contract: pointer at uniform half0, offset dwords at halves4,8,12,... . Allocate the complete native EOT code container and adequate clear/resource-program buffers. Native captures and metadata remain under `native-mrt-witness`.
- Upload distinct clear colors and indexed reload descriptors for every active dense attachment. Mixed per-attachment operations remain unsupported.
- Fix the eight-target reload's register allocation. All sixteen low-bank halves are live packed colors; shared texture coordinates previously occupied that same bank and could not be evicted because they also have a bank constraint. Place those coordinates above half15 within their existing seven-bit operand domain. No hardware register limit is relaxed.

## CPU proof

New tile-layout and loader tests cover counts3..8 and malformed geometry/resource metadata. Seven/eight-color clear generation initially caught the old512-byte temporary body capacity; the conservative eight-chunk bound is624bytes and the temporary now accommodates it.

Real NIR tests cover four/eight-target clear, ordinary fragment epilog, and partial-render reload. The reload reproduces the exact hardware `no constrained region for count=2 minimum=0 limit=16 bound=104` abort before the RA fix, then passes afterward. The epilog compiled before the RA fix and was not the cause. `wide-reload-cpu-ir.log`, `wide-reload-before-test.log`, and `wide-reload-ra-test.log` preserve the evidence.

Final Asahi/poly regressions:264Pass,2ExpectedFail,1unchanged pre-existing `asahi-g17p-precompiled-fill` robust-index golden failure. See `wide-reload-ra-regressions.log`. `git diff --check` passes. Build VM is Lima **neo-builder**, accessed with `limactl shell`; only neo-linux is host-shared, so transfer source changes with `limactl copy` or tar streamed to `limactl shell neo-builder tar`.

## Hardware proof and limits

All hardware iterations used boot `bac77c46-149d-42d4-aba1-dcae3a6422cb`, Xorg1820, native Asahi CTS, and reported kernel_fault=false. No reboot was needed.

| Iteration | Whole iteration | CTS process | Result |
| --- | ---: | ---: | --- |
| dense-mrt-reconnected |16.58s|0.98s|Upload plus exact official case; partial-reload RA abort |
| dense-mrt-trace |4.86s|0.96s|Same abort with diagnostic trace |
| dense-mrt-ra |15.62s|0.97s|Upload plus RA fix; later tile-layout rejection |
| dense-mrt-ra-trace |4.94s|1.20s|Eight-target clear/reload/EOT prepared; batch1 submitted |
| dense-mrt-full |14.62s|10.71s|Full unfiltered attempt, unchanged official ledger |

Trace proves clear302bytes, reload606bytes and EOT1226bytes were selected, with the first batch writing all eight attachment BOs and reaching SUBMIT. It does not independently prove the stored pixels are correct. The next batch rejects its layout. The official case's source clears all attachments, changes draw-buffer1 to GL_NONE, then draws and checks pixels. Sparse output integration is therefore the next boundary; do not describe the whole draw-buffer case as passing.

Latest full evidence: `fast-iterations/dense-mrt-full/results/dense-mrt-full`. All43,916 ledger entries exactly equal the preceding wide-mrt-full run:807selected Pass,68Fail,3InternalError,1Incomplete,43,022NotRun, plus15metadata Pass. Only2/52sessions complete. No new official Pass claimed.

## Connectivity recovery

The first upload attempt stopped at SSH identity, before candidate transfer. USB confirmed the same live Linux boot. Wi-Fi reported connected but could not reach its gateway. A bounded NetworkManager disconnect/reactivation of the existing connection restored gateway replies and SSH, without module reload or reboot. Logs: `dense-mrt-connectivity-*`, `dense-mrt-network-state.log`, `dense-mrt-wifi-reconnect.log`. The original `dense-mrt-first` directory has no hardware receipt because no hardware job was launched.

## Next

Keep the healthy dedicated Xorg running. Use `fast-iteration.py` with a unique label for candidates, `--trace` only for focused diagnostics, and `--full` for the unfiltered official attempt. One serial owner; J700 KIS00100000 only.

Sparse support needs matching physical tile offsets, PBE descriptor indexing, BG operation masks, EOT builtin selection and fragment loader release classes. Existing eight-slot native sparse capture omits shader output1 but retains all attachment formats; its loader release is0x233, while dense is0x203.

The new `native-mrt-probe.py --missing-attachment` capture sets pipeline attachment1 pixel_format=Invalid. It succeeds offline and produces322byte fragment text, SHAc456719fde9d15f603aa4e6229b24492048ba8f20bd7eb5e39614d3af17f4bcf. Compared with the prior sparse-output/full-attachment capture, exactly six bytes change: physical target offsets above the hole reduce by4bytes. Synchronization instructions and loader release0x233 remain identical. Native behavior therefore compacts physical tile offsets while preserving logical attachment synchronization classes. Carry logical target identity separately from physical offset; a count-only class selector cannot express this. This is compiler evidence, not a sparse hardware pass. No arbitrary class compaction or attachment-limit reduction.
