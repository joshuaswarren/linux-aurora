# OpenGL 3.3 qualification: still failed

The completed official run failed 1,312 cases in the last of 50 configurations.
Its GPU session failed during a firmware rebuild, and the original detector
incorrectly reported a clean kernel. The corrected terminal audit remains the
authoritative result. There is no passing full CTS result for the new candidate.

## Candidate

Kernel branch `j700-render-stamp-wrap-20260916`, commit `378b14ac9d89`, based
on the tested `4863a3c90a3b`:

- Carry the full 40-bit native timestamp in AddKicks and TA-to-3D dependencies.
- Wrap the 32-bit completion value while retaining the full software ordinal.
- Remove the artificial 24-bit firmware-restart threshold. Preserve native
  ring ownership, exact completion checks, and existing publication ordering.
- Log the actual reason and ordinal if a retained render queue needs recycling.

The exact trigger of the original rebuild is still unproven. The old log did
not record that render ordinal; aggregate submission counts cannot establish it.

`verify-native-render-stamps.py` verifies nine instruction witnesses in the
pinned 25G83 firmware: unaligned u64 AddKicks timestamp loading, the 40-bit
timestamp and QID split, 40-bit scheduler wrap, and u32 completion publication.
The firmware hashes, exact instruction bytes and offsets are recorded in
`native-render-stamps-25g83.json`. These establish field widths, not runtime
lifetime.

Seven focused scripts passed after the final width changes: 33 compiled Rust
test cases plus the persistent-TPC lifetime checks. Earlier focused lifecycle,
descriptor, retirement and ownership checks also passed. The kernel module
compiled. These are software checks, not hardware qualification.

## Short hardware diagnostic

The separate branch `j700-render-event-wrap-probe-20260916`, commit
`cd6595e2a555`, adds only an event-value seed and bounded completion logging.
Its module SHA-256 is
`fc3c7367555041557df6b593e5f3ed98bcbd309c09aaa5472cab9f9b32279d51`.
The production fix remains independently available at `378b14ac9d89`.

The diagnostic starts host-visible completion values near their 32-bit wrap;
the GPU's hardware timestamps, queue initialization, dependencies, and ring
cursors advance normally. It must **never** be used for CTS conformance evidence.
It tests a real completion-value wrap through zero after 256 submissions. It
does **not** prove full hardware queue timestamp wrap or long-run lifetime.

`probes/render-event-wrap.c` draws 768 fullscreen triangles into a 113x47 RGBA8
framebuffer, changing the color each time. Every first read checks all pixels.
There is no extra glFinish, barrier, or retry-read. Passing requires all
4,078,848 pixels to match and kernel completion witnesses at ordinals 252–258
for the same physical queue, including zero, with no kernel fault or log loss.

`probes/qualify-event-wrap.py` runs once on a fresh qualified boot. The one-shot
service disables itself before testing; no full CTS runner is launched.
Mesa remains the original full-run binary with SHA-256
`362a359bb06f3a1432720bab738f60dd2095f6188cced7d27bcfd8a2f2c0cc56`.
The prior kernel module is backed up on the Neo as
`/home/mcruz/gl30-cts-20260913/asahi-backup-03abec7b984762e6b32a27951652d4c0445e8300e7447a795e5c45dc5291d762.ko`.

Hardware diagnostic **passed** on boot `a888fccf-9641-467e-84eb-5c9356f53d65`.
The renderer was `Apple A18 Pro GPU`. All 768 first-read checks matched all
4,078,848 pixels. QID 3 recorded consecutive completion values
`0xfffffd00, 0xfffffe00, 0xffffff00, 0, 0x100, 0x200, 0x300` at ordinals
252–258. The continuous kernel log and post-test snapshot independently
rescanned with zero fault matches, no lost records and no stream errors.
The complete evidence is in `event-wrap-qualification-20260916/`, including
`receipt.json`, `pixels.log`, `prepare.log`, and both kernel logs.

Production module build also completed with SHA-256
`51541904ae00503eb3c44d6bf5356e206ce7085296d047d489452247977b1594`.
It is loaded on production boot `94c3f079-27bc-456d-93a2-5db1e7ed2171`;
`stamp-wrap-production-runtime.json` records the verified loaded build note.
The temporary diagnostic service was removed; no diagnostic seed exists in
this module. The 1,312-case replay `stamp-wrap-replay-20260916` PASSED in
179.7 seconds using the original failed settings: configuration 64, 113x47,
seed 2, EGL. Strict accounting records exactly 1,312 expected and observed
Pass results, zero issues, exit 0 and zero kernel stream overruns. Independent
rescan of kernel-before, kernel-stream and kernel-after logs reports no faults
and complete evidence. Replay analysis SHA-256:
`9c6317b213380032f712f9d612930fbeddb8e3867109786f77af7d28ee30b2f8`.
It is a diagnostic subset and does not preserve the full campaign's preceding
workload; its `conformance_passed` field correctly remains false.
The first production boot preflight timed out before upload. A scoped VDM
recovery restored the unchanged resident; the retry verified 491,520 immutable
resident bytes and 100 chip-identity queries before starting the kernel upload.

The complete official campaign `stamp-wrap-full-gl33-20260916` was deliberately
stopped on user direction after 3,760.8 seconds, with no Fail results. Its
exit 143, incomplete active case, and unrun coverage reflect cancellation.
Kernel evidence is clean with zero overruns. The same-boot failure-only replay
`stamp-wrap-failures-only-20260916` **PASSED all 1,312 original failures** in
180.2 seconds including supervision (176.9 seconds in CTS). It retained
configuration 64, 113x47, seed 2 and EGL. Exactly 1,312 expected and observed
Pass results, zero accounting issues, exit 0, zero kernel overruns, and an
independent clean/complete scan of kernel-before, kernel-stream, and kernel-after
establish the diagnostic pass. Its replay-analysis SHA-256 is
`9c6317b213380032f712f9d612930fbeddb8e3867109786f77af7d28ee30b2f8`.
The tested Mesa/kernel pair and boot are unchanged. Full conformance remains
unverified; do not automatically restart the broad campaign.
