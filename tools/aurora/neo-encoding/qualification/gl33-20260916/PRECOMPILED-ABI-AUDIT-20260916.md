# Precompiled G17P ABI fixture audit

Test-only correction `19df5b1a7ed` follows production candidate `cf32dcf7b18`.
The targeted executable passes all 584 checks. The prior complete 297-test run
reported 277 OK, 18 FAIL and 2 EXPECTEDFAIL; that full run has not been repeated
for this fixture-only change. One of its 18 failing tests now passes separately.
The candidate Mesa library is unchanged and has not been installed on the Neo.

## Evidence for the corrections

- `9e8064a38f2` moves precompiled arguments above twelve hardware-owned uniform
  halves. Resolve, statistics, tessellation factors and indirect setup had
  retained their older push counts and full-code hashes.
- `104fe548a86` places returned-atomic storage directly above the ordinary
  spill frame and publishes the complete extent. Triangle variant 1 uses
  52 bytes of spills plus 16 bytes for the returned atomic; quad variant 1
  uses 104 plus 16. The actual TPS extents are therefore 68 and 120 bytes.
- The same CLC path disables G17P preambles and supplies a printf pointer at
  uniform half 16, after the twelve-half hardware prefix and four-half kernel
  argument. Its total push count is 20. The old tessellator fixture retained
  a 128-byte prefix, push count 24, and the older scratch interpretation.
- `1d7521a2e20` adds coherent memory encoding. The timestamp binary differs
  from its old 32-byte vector only at byte 11: 0x40 becomes 0x80. This is the
  same coherent LDDEV f77 contract already checked by the native memory-core
  vector and the corrected event-poll fixture.
- The robust-index sizes/hashes restore the vectors already present in the
  qualified J700 snapshot `8df91891ab7`, instead of the stale upstream goldens.

`probes/run-agx-sync-c-probe.py kernel` was rerun for both the current candidate
and unmodified pinned upstream `33d4d8d9f13`. All fourteen inspected kernels
have identical bytecode hashes and common metadata fields across the two
independent builds. The candidate-only scratch fields agree with the compiler
frame calculation and compact metadata conversion. This comparison supports
the fixture update; it does not establish hardware correctness of the kernels.

The test retains its complete byte hashes, sizes, register counts, workgroup
dimensions and terminators. New assertions independently require the printf
address to fit the push range and the returned-atomic reservation to follow
the spill frame without overlap. No production path was changed.

## Artifacts

- `agx-sync-kernel-probe.log` and `.json`: current generated metadata and hashes.
- `agx-upstream-kernel-probe.log` and `.json`: pinned upstream control.
- `precompiled-abi-build.log`: targeted rebuild.
- `precompiled-abi-test.log`: 584 checks passed.
- `agx-sync-20260916-linux/testlog.json`: preserved prior full-suite result.

Next qualification remains the completed strict CTS audit on the existing
installed pair, followed by focused hardware testing of the isolated candidate.
Do not replace the driver or interrupt the live full campaign to test this change.
