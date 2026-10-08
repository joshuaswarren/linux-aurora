# J700 readback producer audit — 2026-09-15

The late-readback defect is still present. A new in-memory journal captured
the exact Mesa producer/wait history at six real pixel changes. Every captured
history shows the correct mapped BO in the render attachment list, its submit
returning success, and its writer sync object successfully waited before the
map returned. This narrows the investigation; it is not a fix or CTS coverage.

## Real hardware result

Boot `5a46d969-07c9-4cb1-b307-8adae3456a74`, J700/T8140, stamp-gated Asahi
module `6e8621eb0c4147778d243870907cffccdee5b9b74711516c451fdc9c88937cbe`.
Diagnostic Mesa `db9e4485582b55275f9a3e15802a245ded665a6f8b1828ee8d0c56c7a951a480`.

The 3,000-iteration experiment completed in 5.79 seconds on CPU 0. Of 6,000
observed CPU mappings, **21 changed pixel bytes after synchronization**. The
first six changes include a 128-entry producer journal. The observed buffer
is BO 11 at GPU VA `0x5ffff34000`, CPU map `0x7fff8a13c000`, with a 16-KiB
allocation and a writeback CPU mapping.

| Captured change | Submission sequence | Attachment extent | Observed extent |
| --- | ---: | ---: | ---: |
| 1 | 1313 | 1920 bytes | 1807 bytes |
| 2 | 1565 | 1280 bytes | 1162 bytes |
| 3 | 1813 | 1920 bytes | 1807 bytes |
| 4 | 1873 | 1280 bytes | 1162 bytes |
| 5 | 2011 | 1920 bytes | 1807 bytes |
| 6 | 2045 | 1280 bytes | 1162 bytes |

Each chain is: writer assigned to BO 11 → CPU requests that writer → BO 11
included as a fragment attachment → submit succeeds → sync object 4 is waited
successfully → batch cleanup → read-only CPU map. The attachment starts at the
same GPU VA and covers the entire observed extent. The journal distinguishes
submissions by sequence even though the batch and binary sync handles are reused.

The journal records events in its owning Gallium context, preserving history
across threaded-context handoffs. Recording adds ordinary memory stores, with
no logging, waits, system calls or GPU commands. It prints the ring only after
the existing observer detects a change. The observer's 200-us busy observation
interval is diagnostic: it lets the final application read become stable.
The application's zero mismatches during this experiment therefore **must not
be treated as a fix**. The observer itself found 21 changes.

`readback-journal-observe-3000.log` is raw evidence.
`analyze-readback-journal.py` verifies each captured chain and writes
`readback-journal-analysis.json`. It verifies six histories, not all 21 changes.

## Source audit

- Mesa `agx_batch.c:599` looks up the resource BO's writer. `agx_sync_batch`
  waits its sync object and then cleans up the batch. The live journal agrees
  with this source path for the six captured changes.
- G17 uses its own queue implementation. `g17_drm.rs:2384` publishes the render
  watch scheduler's finished fence. `g17_job.rs:280` returns the real render
  completion fence to that scheduler. `install_output_fences` replaces the
  binary sync object and attaches the same fence to the requested timeline
  point. The common M1/M2 queue path is not the G17 submit implementation.
- `g17_live_boot.rs:4703` polls the translated render completion, including
  the new fragment stamp gate, before settling the deferred render and
  signalling its completion. The stamp A page is zeroed during initialization;
  no host write of the target completion value was found in this source path.

These checks make a missing Mesa writer wait less likely. They do not prove
hardware-fence timing or exclude a lower-level sync-generation error.

## Independent native cache evidence

`probes/native_b1.py` reads the saved 25G83 firmware and kernelcache with strict
SHA-256 checks. It does not extract files, contact hardware or alter firmware.

- B1 `0x16b6c..0x16bcc` builds two SGX `+0x18030` commands from EventControl
  fields. The first is `(field_24 << 32) | 0x680000`; the second is
  `(field_24 << 32) | ((field_08 & 0x3f) << 24) | 0x70000`.
  Both poll SGX `+0x18038` bit 1 before advancing, except the existing recovery
  escape. The fragment stamp write remains later at `0x16ea0`.
- B1 `0xc9e8..0xc9f4` loads fragment descriptor `+0x0c` as the context value
  and `+0x20` as the EventControl pointer. `0xd250..0xd254` copies that context
  value to EventControl `+0x24`. The meaning and initialization of EventControl
  `+0x08` need further investigation; do not assume it is the aperture selector.
- Native AGXG17P symbol `AGXAcceleratorG16::halMCacheApertureSetup` at
  `0xfffffe00088c7018` packs the two range words exactly as Linux currently
  does: start address in 128-byte units, selector at bit 41, HWDSID at bit 47,
  bit 55 set; end address plus bits 41 and `42 + min(range_shift, 4)`.
  This validates packing, not the choice of HWDSID/selector or when the GPU
  finishes those ranges. The generic base-class implementation is a stub and
  must not be mistaken for the concrete encoder.

Next investigate native cache context/HWDSID ownership and completion visibility.
Do not replace this with per-draw barriers, sleeps, or a first-read retry.

## Restoration

The diagnostic source overlay is outside the Mesa worktree. The builder
verified the three original files before applying it, then restored them and
reproduced normal Mesa SHA
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
The Neo was restored to that same library. The official vendor smoke passed
after restoration, Xorg PID 762 remained healthy, and the receipt reported no
GPU fault or kernel-log overrun. The stamp-gated kernel remains loaded.
No production source change was introduced by this journal investigation.
