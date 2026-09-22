# Faster native M5 CTS iterations

The measured v23 boot spends 29.741 seconds transferring and checking the
kernel and 120.431 seconds on the initramfs. U-Boot and DTB add 1.511 seconds.
The 244-case physical test selection itself takes 22.095 seconds. Rebuilding
the RAM environment, rather than test execution, dominates short runs.

## Reduced headless runtime

`build-m5-cts-runtime-root.py` derives a headless runtime from the existing
hash-verified desktop archive without modifying its source. It retains
Python, SSH and PAM, command-line diagnostics, locales and system shared
libraries. Package caches, desktop applications and distribution GPU driver
binaries are omitted. Native Mesa and the unchanged CTS are still installed
as separate hash-verified payloads.

The resulting archive is about 140 MiB instead of 346.5 MiB, a reduction of
about 60 percent. Gzip headers are deterministic. The exact final digest is
recorded by the build and pinned in the restore helper.

Final runtime SHA256:
`b516ad9186d06120bc09f3e5d9d29c89314364a055298f25344e934bedf33714`.
Archive size: 146,726,585 bytes; unpacked files: 399,381,592 bytes.
This includes the original distribution's `grep`, required by target identity
verification before the driver/CTS installer runs.

Physical M5 qualification of the runtime contents establishes:

- Python, shell, hash checks and archive tools execute.
- OpenSSH authenticates with the existing public key using normal PAM account
  checks; SCP transfers the 244-case list with its exact original hash.
- Native surfaceless CTS enumerates the exact same 48,300 dEQP GLES3 cases.
- Enumeration does not submit additional GPU work: the completion count
  remains 503 after the rendering run.

Enumeration is not a conformance pass. Rendering results reported in
`M5_NATIVE_GLES3_ADDITIVE_BLEND.md` used the established full runtime.

The v24 fresh boot measured 57.59 seconds from reduced-root download start
to SSH ready: 19.38 seconds download, 5.71 seconds verification, 30.28 seconds
extraction and 2.22 seconds SSH setup. These are target monotonic timestamps;
the earlier full-root restore was not separately phase-timed. This boot used
the preceding archive plus its missing original `grep` binary, transferred
before testing. The final archive above includes that binary directly.

The reduced-root run passes all 1,171 clear/default-framebuffer blend cases
in 129.818 seconds with no compiler refusal or GPU fault. See
`M5_NATIVE_GLES3_DEFAULT_BLEND.md` for retained hardware evidence. The
compiler changes increase batch coverage without weakening test checks.

The host restore uses this runtime for headless thermal testing and retains
`--full-desktop-root` as an explicit fallback. The generic bootstrap accepts
an archive basename and SHA256 and verifies the download before extraction.

## Remaining iteration limit

The following v25 boot restores the corrected archive directly in 55.90
seconds. Its larger native test batch finishes 2,323 cases in 313.970
seconds (2,275 Pass / 48 Fail), while temperatures stay below 44.1 C. The
final kernel ring has wrapped by then, so future larger runs must stream
the kernel log during execution as well as retaining the final snapshot.

The experimental graphics queue retains the first submitting VM and rejects
context replacement. A different test process therefore still needs a fresh
boot. Removing the VM check alone is not a valid fix: queue retirement,
firmware context ownership and memory teardown must be implemented together.

Until that lifecycle is qualified, compiler-only checks identify unsupported
instructions before booting, and larger exact case lists amortize a boot
across many hardware tests. Neither shortcut changes CTS behavior or counts
offline results as hardware passes.
