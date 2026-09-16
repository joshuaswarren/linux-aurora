# Candidate kernel interface and synchronization review

Candidate: `mesa-j700-agx-sync-20260916` at
`67e5e2a121028b2f1d95c53ee0383fa035008ab4`.
Kernel source: `neo-encoder-linux-j700-readback-20260915` at
`4863a3c90a3b52963bf00fb31376629e02984940`.
Running-Mesa source reference: `b79a17bbe784cda8e34b3471317fc690505d993a`.
The full CTS campaign still uses the previous installed Mesa binary. No candidate
installation or GPU workload was performed for this audit.

`audit-agx-kernel-interface.py` produces `agx-kernel-interface-audit.json`.
The candidate's `include/drm-uapi/asahi_drm.h` is byte-identical to the running
Mesa source header (SHA-256
`c993d916769fdc10d2d29c8169d1011540623e139d6a4b43fa0eb02c76cd7115`).
The kernel header differs in comments; removing comments and normalizing
whitespace yields identical declarations. This establishes agreement for these
shared UAPI declarations only. Firmware command structures, binary identity,
and runtime behavior still require hardware qualification.

The synchronization review compares `agx_state.c` and `agx_batch.c` with the
pinned M1/M2 design reference, Mesa 26.1.4 commit
`6dfbc555b4128ee51139c5f78c5aba2594c9701b`:

- The additional `agx_flush_writer_except` tracks a separate stencil plane's
  producer before a different batch reads it. It excludes the consuming batch.
- Added blocking waits around shared buffers are error fallbacks: failure to
  export/import or allocate a synchronization object for an incoming fence,
  or failure to transfer the submitted batch's release fence to a shared buffer.
  The normal path passes synchronization objects to submission. The added
  `drmSyncobjWait` is guarded by `transfer_failed` at lines 1364–1379; the incoming
  fallback is guarded by export/import failure at lines 1000–1028.
- G17P still disables shader preambles at the Gallium compiler call site.
  Native input-handoff and execution qualification remain open.

This limited source inspection finds resource dependencies and error fallback
waits, not a new per-draw blocking wait in these paths. It is not a whole-driver
efficiency audit and does not establish M1/M2 performance parity. Preamble
qualification and measured hardware comparisons remain required; no barrier,
draw splitting, or synchronization behavior was changed by this review.
