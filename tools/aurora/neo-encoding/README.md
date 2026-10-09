# Neo Linux hardware encoding

The [September 16 OpenGL qualification archive](qualification/gl33-20260916/ARCHIVE-README.md)
preserves the CTS harness, diagnostics, corrected failure accounting, and two
passing replays of the 1,312 original failures. Full GL 3.3 conformance remains
unverified; the broad rerun was intentionally stopped at the user's request.

Native J700 / T8140 / Mac17,5 bring-up and diagnostic hardware encoders for
Aurora Silicon. Builds run in the dedicated ARM64 `neo-builder` VM; the Neo
runs the actual hardware qualification through KIS and guarded RAM boots.

The versioned source lives in `tools/aurora/neo-encoding/` on the lowercase
`j700` branch of `aurora-silicon/linux-aurora`. The branch starts from the
hardware-tested `drm-agx3` kernel commit
`4ac2664258f370cf7b03327da742b5885f91ca0b`. The drivers under `video/linux-*`
are external diagnostic modules with their own build scripts; they are not
enabled through the kernel's media Kconfig.

See [encoding results](ENCODING_EXPANSION_RESULTS.md), the machine-readable
[target ledger](ENCODING_TARGETS.json), [build instructions](BUILDER.md), and
[native boot instructions](WORKING_NATIVE_BOOT.md). The append-only engineering
record is [ENCODING_EXPANSION.md](ENCODING_EXPANSION.md).

Qualified paths include H.264, HEVC Main/Main10/Main42210, the six ProRes
variants, JPEG/Motion JPEG, ProRes 12-bit color with 16-bit alpha, and HEVC
depth/disparity. Their exact tested input patterns, error limits and limitations
are recorded in the results. These are diagnostic encoders; application-facing
FFmpeg/V4L2 integration and sustained streaming are still pending.

The paired MV-HEVC transaction currently passes host ordering/fault tests and
builds as a separate candidate. Native multiview validation is unfinished.
HEVC alpha and hardware/DSP audio are not qualified. Software audio codec
enumeration is not evidence of hardware encoding.

## Local evidence and dependencies

Large builds, matching symbols, captures, reference inputs/outputs and hardware
logs remain under ignored `artifacts/` and `logs/`. `QUALIFICATION_ARCHIVES.json`
records the local qualification manifests and their hashes. Each qualification
archive has its own file manifest and, where needed, pinned dependency archives.
Cloning the kernel repository alone does not transfer those artifacts.

The current build scripts expect a separate `neo-linux` working directory next
to `neo-update`, shared into the builder VM. Export this directory there before
using the recorded commands, and transfer the matching ignored dependencies:

```sh
# Run from the linux-aurora repository root; choose an empty destination.
git archive j700 tools/aurora/neo-encoding | tar -x --strip-components=3 -C /path/to/neo-linux
```

Host paths and VM mount paths in the build scripts still describe that verified
workspace. Adapt them together when setting up another host; do not run the
hardware scripts directly from this nested kernel-tree directory.

The sibling `../neo-update` workspace supplies the existing KIS loader, Python
environment and boot material. Its resident Stage 1 is preserved. Never install
a watchdog or diagnostic Stage 2 as persistent Stage 1. Reboot after firmware
or DMA faults; never unload modules after submitting firmware work.

Progress webhook endpoints and credentials are supplied only at runtime.
The current session's reporting contract is
`../neo-update/PROGRESS_WEBHOOK_GUIDE.md`; no endpoint belongs in this repository.

## Current desktop follow-up

As of 2026-09-11, the Neo runs native KDE Plasma X11 with DCP scanout,
KWin GPU compositing, GPU gears and Wi-Fi visible in the Networks panel.
The [composited cursor helper](desktop/README.md) addresses the pointer gaps
during redraws and restores the native cursor while Plasma is locked.

See the repository's [J700 overview](../../../J700.md) for the qualified
versions, source map and remaining Wi-Fi startup and DCP resume limitations.
The host-side `neo-linux/HANDOFF.md` retains the deployment and recovery
record. macOS and resident Stage 1 remain preserved.
