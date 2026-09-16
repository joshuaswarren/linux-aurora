# J700 native OpenGL game qualification, 2026-09-16

## Ownership fix and scope

`0259778d7295107764d500b582155adb6be13d4b` fixes cached render-graph selection
when sibling logical queues in one VM use different descriptor contexts. Fixed
RT/TPC storage belongs to the VM and physical queue pair. Cache selection still
requires the current render-pool identity; restored storage then retargets its
descriptor context through the existing context-retirement transaction.

The previous matcher rejected valid storage retained for a sibling queue and
attempted to allocate overlapping fixed mappings. A fresh four-second native
Mupen64Plus-Next run reproduced 1,045 persistent RT allocation ENOSPC failures.
The candidate eliminated those allocation failures and reached captured Mario
title, file selection, introduction, world, HUD and movement.

This fixes that allocation bug. It does not qualify all application stability
or complete OpenGL conformance. A subsequent launch exposed another GPU fault,
documented below.

## Identified binaries

| Component | SHA-256 |
|---|---|
| Baseline Asahi module | `51541904ae00503eb3c44d6bf5356e206ce7085296d047d489452247977b1594` |
| Candidate Asahi module | `669400dab39b45f631d7da1dfd5a043810ade12954f4c9eeb93afdbf97957592` |
| Unchanged native Gallium library | `362a359bb06f3a1432720bab738f60dd2095f6188cced7d27bcfd8a2f2c0cc56` |

The module was built in an ARM64 Linux VM on the M3 Max and tested on J700.
The loaded ELF build note was checked against the installed module. The game
requests desktop OpenGL Core 3.3 through GLideN64. The runtime reports
`Apple A18 Pro GPU`, Mesa 26.1.4, with the pinned native Gallium and GLX libraries
mapped. Mesa returns a 4.6 core context without version overrides; this is not
a conformance claim. The core's `2.8-Vulkan` build label is unrelated to the
selected API. Xorg uses Zink internally, separately from the native game.

## Focused verification

- Executable cached-VM matcher regression: old predicate fails, fix passes.
- Ten of eleven selected ownership/handoff host scripts pass. The remaining
  source assertion in `test_g17_render_hardware_buffer_id_ownership.py:90`
  fails identically on the untouched baseline and candidate.
- Single-context 1024x1024 RGBA8 FBO: 20 correct RGBA readbacks.
- Two shared contexts, same FBO dimensions: 200 correct RGBA readbacks.
- First candidate game run: visible world/HUD and movement, zero allocation,
  submission or fatal GPU markers in the captured kernel stream.
- Focused `KHR-GL33.texture_swizzle.smoke*`: five Pass, four NotSupported,
  one incomplete case when the 90-second timeout expired. No GPU fault during
  this sample. This is not a completed CTS group. The emulator was paused
  during CTS and resumed afterward.

The historical minimal FBO crash did not reproduce on a fresh baseline boot;
its relationship to the allocation bug remains unestablished. Cached CPU
interpretation bypasses a separate emulator dynarec SIGSEGV. Performance and
long-play stability have not been qualified.

## Remaining relaunch failure

The next game run reached the intro screenshot, then firmware requested
stamp-blamed recovery at kernel time 1774.366855. The fragment-side completion
was not retired; the driver marked `render-submission-lost` at 1774.368880,
recovery stamp 3, physical slot 1. Persistent RT allocation ENOSPC stayed zero.

Mesa subsequently aborted in `agx_encoder_bind_vdm_alias` after VM_BIND returned
sticky EIO from the earlier GPU failure. That assertion is a consequence, not
the initial fault. The cause of the lost submission is not isolated. No claim
is made that the ownership patch fixes this additional failure. Fault evidence
was saved before a clean qualified recovery boot; no post-fault rendering is
counted as valid native evidence.

Minecraft's separate missing-UI/compiler failures and inventory-triggered GPU
freeze remain unresolved. Physical-panel confirmation of the candidate boot
was pending when the evidence was recorded; screenshots prove rendered output.

## Evidence receipts

Campaign artifacts: `neo-linux/artifacts/gallium-game-tests-20260916/driver-fix/`
in the parent engineering workspace. ROMs are not part of this source report.

| Archive | SHA-256 |
|---|---|
| `candidate-evidence.tgz` | `17a86a6be5c53410c695b4b73ca31ee3d8219334e2511e06bf7e3f215bb1ae62` |
| `reopen-fault.tgz` | `c97339c0a1c775c1f45260a641516ce55bcb673997ed96b47daa0d44ccb9ce8d` |

The campaign worklog and qualification receipt distinguish initial gameplay,
the later failure, recovery status and the remaining conformance work.
