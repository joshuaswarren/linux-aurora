# G17P render completion stamp audit — 2026-09-15

Hardware testing shows that requiring the firmware's exact post-maintenance
completion stamp **does not resolve the readback race**. The candidate is
implemented, built and tested on the Neo. Normal Mesa is restored and the
stamp-gated kernel remains loaded. The firmware evidence, implementation and
hardware results are recorded below.

## Independent firmware evidence

Source: the saved `UniversalMac_26.6.2_25G83_Restore.ipsw`, member
`Firmware/agx/armfw_g17p.im4p`, B1 (`b100`) image. IM4P SHA-256:
`9b2afdfbf6c3d4b2892a1e4b98766d57a322c95a2a3a535403c50b2dbfa2955b`.
Addresses below are relative to `0xfffffc0000000000`.

The read-only `verify-native-render-completion.py` checks the image digest,
parses the RTOS table and Mach-O mappings, and checks 17 instruction witnesses.
Capstone 5.0.6 is available in the Context Mode Python runtime.

The completion decoder at `0xab64` selects the fragment stamp-info structure
at descriptor `+0x2140` (`0xc9f4`, `0xca2c`), retains it in x28 (`0xcf88`),
and queues it in EventControl's fragment bank (`0xd218`, bank pointers at
`+0x50`, mask at `+0x4c`). The timestamp-queue handler starts at `0x168d0`:

1. `0x16b28..0x16b48`: unlink EventControl and clear membership at `+0x94`.
2. `0x16b90` and `0x16bcc`: issue cache-maintenance commands through
   SGX `+0x18030`; loops at `0x16ba0` and `0x16bd0` examine bit 1 at
   SGX `+0x18038`. Fault/recovery handling follows these loops.
3. `0x16e60..0x16ea0`: process the fragment bank, load the descriptor's two
   stamp pointers and value/QID, then store the value through the **first**
   pointer. KTrace event `0x0c` identifies the fragment stamp publication.
4. `0x16f58`: dispatch the accumulated completion notification.

The compute bank's corresponding store is at `0x16f18` (KTrace `0x0e`).
`0x16e28` is the TA bank's store; older compute comments referring to that
address describe the shared ordering but identify the wrong bank-specific store.

The critical ordering is list unlink -> cache maintenance -> fragment stamp
publication. An empty list therefore does not itself prove CPU visibility.
This audit establishes instruction order in the pinned firmware. It does not
establish that every observed readback failure has this cause.

## Linux side

In `neo-encoder-linux/drivers/gpu/drm/asahi/g17_resources.rs`,
`apply_g17p_generic_descriptor_tail` already puts the A/B stamp pointers in
TA `+0x8a6/+0x8ae` and fragment `+0x2140/+0x2148`. Each points at the
corresponding page plus **physical QID multiplied by four**. Page A is
GPU/firmware shared; page B is firmware private. Both have CPU WC mappings.

`g17_render.rs::apply_g17p_retained_descriptor_fields` sets the expected
value to `(submission_ordinal + 1) << 8` and rejects ordinals exceeding the
24-bit representation. Physical render-slot state carries the ordinal.

The installed `g17_manager.rs::poll_translated_render_completion` checks queue
indices, a completed job list, nonzero timestamps and EventControl unlinking.
It does not check either render stamp. `render_stamp_lanes` only appears in
timeout diagnostics. By contrast, the asynchronous compute path requires its
completion stamp before returning success.

## Candidate implementation and offline validation

`g17_resources.rs::render_fragment_completion_stamp` now loads only the
selected physical fragment QID's A word using an aligned atomic acquire load.
`g17_manager.rs::poll_translated_render_completion` compares that word with
`(submission_ordinal + 1) * 0x100` before it releases any job ownership or
reports success. A mismatch leaves the existing job and fence pending for
the post-stamp type-1 notification. Existing queue, timestamp, and
EventControl checks remain in place. No delay loop, GPU barrier, draw split,
timeout-success fallback, or new periodic work was introduced.

The existing `park_render_sksm_backing_for_graph_rebuild` retains the physical
slot's monotonically increasing ordinal across VM ownership changes; slot
save/restore carries it too. The descriptor encoder rejects 24-bit stamp
overflow. Since only one job is in flight on a physical pair, equality is
required: an unrelated older or later nonzero stamp is insufficient.

- Native firmware witness script: **17/17 verified**. B1 image SHA-256:
  `c1d35018d1226f8b7db374b0cc4332c6e43479ad7a0d001c5a7d0ecda1354412`.
- Existing pure completion module: **35/35 tests passed**. These tests cover
  the decoder and ownership model; they do not validate the new hardware load.
- Kernel module build: **exit 0**, no errors; see `kernel-render-stamp-build.log`.
- Stripped candidate: `asahi-render-stamp.ko`, 3,727,944 bytes, SHA-256
  `6e8621eb0c4147778d243870907cffccdee5b9b74711516c451fdc9c88937cbe`.
- Vermagic: `7.1.6-aurora-neo-gpu1 SMP preempt mod_unload aarch64`.
- `git diff --check`: passed. Kernel source edits remain uncommitted.

The candidate also includes the already qualified MSAA descriptor fix in
the builder. It was subsequently staged and tested after stopping the partial
failing baseline, as recorded below.

## Acceptance criteria still unmet

- Verify queue-slot reuse and reset behavior on hardware. A stale stamp from
  another generation must never satisfy completion.
- Verify the post-stamp notification reaches
  the pending job; do not introduce delay loops, timeout-success fallbacks,
  extra GPU barriers, or split draws.
- Retest the readback race reproducer and fragment-invocation
  query case after resolving the remaining visibility problem. Require
  repeated zero-mismatch runs and another complete official configuration sweep.

The firmware-only audit left the running kernel and resident unchanged while
CTS continued. The subsequent hardware test is recorded next.

## Hardware results on the stamp-gated kernel

After saving the failing baseline, the full run was intentionally stopped
after five complete configurations and part of a sixth (54,683 Pass,
1,484 NotSupported, 54 Fail; 23 unique failed cases). It is a partial,
failing campaign. Its QPAs and receipt are in
`partial-depth-storage-tess-full2-20260915/`.

The candidate module was backed up/staged with
`stage-asahi-render-stamp.py` and loaded by a qualified RAM boot. The
installed resident, Stage2 and kernel Image were unchanged. New boot:
`5a46d969-07c9-4cb1-b307-8adae3456a74`. The module file digest and actual
loaded build note were both verified. GNU build ID:
`2c04e3df3acddeac820b5ff5371217c44780d29d`.

| Test | Result | Evidence |
| --- | --- | --- |
| Checked readback probe, 3,000 iterations | 5/2,988 mismatches; all five second reads correct; no GL error | `render-stamp-readback-3000.log` |
| Checked readback probe, 12,000 iterations | 277/11,988 mismatches; all 277 second reads correct; no GL error | `render-stamp-readback-12000.log` |
| Official compressed packed-pixel cases | 570 Pass, zero other statuses; no GPU fault | `fast-iterations/render-stamp-compressed-cts/results/render-stamp-compressed-cts/diagnostic.qpa` |
| Official fragment-invocation query, original failing FBO/seed | 1 Pass; no GPU fault | `fast-iterations/render-stamp-fragment-invocations/results/render-stamp-fragment-invocations/diagnostic.qpa` |
| Same CPU mapping observed continuously on CPU 0 after map/sync returned | 12/6,000 mappings changed actual pixel bytes without another GPU submission during each observation | `render-stamp-observe-same-map.log` |

The checked probe adds setup/error checks and fails on any unstable second
read. Its loop has no extra calls between the compared readbacks. GL errors
are checked once after the loop. These changes prevent untouched output
buffers or setup failure from being reported as a successful test.

The same-map observer is the prior diagnostic overlay, rebuilt on the current
Mesa. Its only source differences are the observation helper and its map
call site. The observer's explicit 200-us interval is diagnostic and was
never proposed as a production synchronization fix. The normal Mesa source
and binary were restored on the builder after creating the diagnostic
candidate, then restored on the Neo after testing. Normal Mesa SHA remains
`2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2`.
The restoration smoke test passed and Xorg PID 762 remains healthy. No CTS
or probe process remains active at this checkpoint.

No `fragment stamp pending` messages were observed, but that diagnostic is
conditional on `g17p_verbose` (default zero and not exposed through sysfs),
so its absence does not establish that the new check never waited.

The focused CTS passes do not establish that the nondeterministic failures
are fixed. The same-map evidence shows that the visibility/producer-ordering
problem persists even with exact fragment-stamp gating. Next work must trace
the producing batch and CPU visibility/cache-maintenance path; another full
campaign would not yet establish a reliable fix. Do not claim a measured
pass-rate improvement from these unpaired, nondeterministic probes.
