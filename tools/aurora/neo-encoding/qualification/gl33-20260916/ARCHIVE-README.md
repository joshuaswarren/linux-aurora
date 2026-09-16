# J700 OpenGL qualification source archive

This archive preserves the authored CTS tools, host-only diagnostics, engineering
notes, and selected original result receipts from the September 13–16 work.
`SOURCE_MANIFEST.json` verifies every imported file against its original workspace
copy and records hashes for 79 large raw QPA/log files retained locally.
Compiled kernels, Mesa libraries, extracted Apple binaries, toolchain packages,
Python environments, credentials, and large raw test logs are not Git payloads.

## Verified result and current scope

- The original complete `cache-identity-full-gl33-20260915` run **failed**:
  478,322 main Pass, 14,666 NotSupported, and 1,312 Fail. Its original
  `kernel_fault: false` receipt was wrong; the corrected independent audit
  identifies the firmware failure. The original receipt is preserved unchanged.
- The production timestamp-wrap kernel fix is commit `378b14ac9d89`.
  Its short hardware completion-wrap diagnostic passed. This is narrower than
  proving the original long-run failure's trigger.
- `stamp-wrap-full-gl33-20260916` was deliberately stopped on user direction
  after about 63 minutes, without a Fail result. Exit 143, one incomplete case,
  and unrun coverage reflect cancellation, not a complete conformance result.
- The requested same-boot `stamp-wrap-failures-only-20260916` replay **passed
  exactly 1,312/1,312 cases**, with zero accounting issues, in 180.2 seconds
  including supervision. It retained configuration 64, 113x47, seed 2 and EGL.
  All three preserved kernel logs independently scan clean and complete, with
  zero overruns. A prior replay also passed all 1,312 cases.

The replay's kernel module SHA-256 is
`51541904ae00503eb3c44d6bf5356e206ce7085296d047d489452247977b1594` and Mesa SHA-256 is
`362a359bb06f3a1432720bab738f60dd2095f6188cced7d27bcfd8a2f2c0cc56`.
The tested boot was `94c3f079-27bc-456d-93a2-5db1e7ed2171`. Neither binary changed
between these production tests.

The accumulated Mesa source on `mesa:j700` through `67e5e2a1210` includes an
upstream integration and additional preamble/compiler work. It is **not** the
installed, hardware-qualified replay library. Its recorded host build/testing
and remaining baseline failures are described in `AGX-SYNC-20260916.md`.
Runtime preambles remain disabled; the offline scheduling investigation is
paused. **Full OpenGL 3.3 conformance and M1/M2-level efficiency are unverified.**
The latest user direction was to test only the previous failures; do not
automatically launch another broad run or install a candidate.

Start with `HANDOFF.md`, `STAMP-WRAP-QUALIFICATION-20260916.md`, and
`GL33-TERMINAL-RESULT-20260916.md`. Older notes and receipts are timestamped
historical evidence and may describe stages superseded by the current handoff.
The isolated event-wrap fixture patch is diagnostic source, not an enabled
production kernel change.

## Workspace layout and reproducibility

The scripts were authored for
`neo-linux/artifacts/j700-gl30-cts-20260913` in an Aurora workspace, beside
`neo-update` and the separate Mesa/kernel checkouts. Preserve that relative
layout when restoring the source. Do not run hardware scripts directly from
this nested repository directory. To restore into a new, empty directory:

```sh
mkdir -p /path/to/workspace/neo-linux/artifacts/j700-gl30-cts-20260913
git archive j700 tools/aurora/neo-encoding/qualification/gl33-20260916 |
  tar -x --strip-components=5 \
    -C /path/to/workspace/neo-linux/artifacts/j700-gl30-cts-20260913
```

Matching ignored inputs, the pinned CTS build/inventory, existing SSH config,
the guarded KIS loader, native reference images, and toolchain environments are
separate prerequisites. Their paths and hashes remain in the source records.
There is no automatic hardware operation during export or source verification.
Console credentials are supplied interactively; none are archived here.
