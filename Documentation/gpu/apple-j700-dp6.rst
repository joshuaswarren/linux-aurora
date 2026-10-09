J700 external DisplayPort bring-up (DP6)
======================================

This branch carries the driver sources used for visible external output and
one successful physical unplug/replug on a MacBook Neo (J700, T8140), using
Apple 25G83 firmware. The external mode was 1920x1080 at 60 Hz. The internal
2408x1506 display and the GPU-accelerated Omarchy session remained usable.
This is an experimental bring-up result, not a general hotplug qualification.

Base: ``acc8b496cfacae9dc64aab0e5a20ac18090395be`` in aurora-silicon/linux.
The companion m1n1 branch is ``neo-j700-external-display-dp6-20260928`` in
https://github.com/aurora-silicon/m1n1-aurora . Its calibration handoff is
required; kernel sources alone do not supply device calibration.

Changes
-------

* Restore the previously developed T8130-layout AUX, DP calibration, lane
  training and cleanup path on top of the current ATC driver, retaining its
  fixed USB2 path. Consume firmware-provided training tables rather than
  inventing drive settings. Preserve the existing source attribution.
* Decode the H17P DPTX training payload with bounds checking and propagate
  PHY failures. Older firmware retains its existing packet handling.
* Use the current per-Type-C-route binding, route selector 2 and PHY 0.
  Place both scanout DART streams in the shared DRM DMA domain.
* Keep the internal H17P mode workaround scoped to the internal controller;
  apply real modesets to DCPEXT. The qualified runtime uses
  ``appledrm.send_modeset=N`` for the existing internal-display path.
* Quiesce H17P external output before HPD removal and avoid a second
  clear-surface shutdown from delayed CRTC disable. Preserve the unplug
  marker through quick replug until the serialized CRTC enable.
* Return DCPEXT's own BWR/boost resources in D003 and map-reg replies:
  BWR agent 2 at ``0x302828000`` (8 bytes), boost agent 3 at ``0x302870000``
  (4 bytes), descriptor indices 4/5. The previous internal scratch reply
  did not fit the external descriptor and firmware faulted during poweroff.
  Keep the internal controller's existing aperture workaround scoped to it.

The board DTS now expresses the route, shared scanout domain and bandwidth
windows that were applied to the tested DTB. No generated DTB, Apple firmware,
device calibration, keys, raw firmware crash dump or binary module is included.

Validation
----------

The changed ATC and DRM C/header files match the hardware-tested module sources
byte for byte. Those modules were built for
``7.1.12-aurora-wifi-20260928+``. Packaging version tags are retained so logs
identify the qualified modules:

* ``phy-apple-atc``: ``20260928-dp1``, SHA-256
  ``e67c8f02544057e86aff38a9ada65104182ff4bf44ea03662dc43af59bcc5278``.
* ``appledrm``: ``20260928-dp6``, SHA-256
  ``fdf1e95990415ecf682478008b7adc29092f1232e67280c446c16bda6f6b2f9d``.
* Hardware-tested DTB: SHA-256
  ``162dedbab775643f03208b04c1e7f2c16c3104d0679b2666385dab70f693221d``.
* Source-rebuilt DTB: SHA-256
  ``7f4a7459c333aca1b90d08bc6203c5d12938d25e57f2f017c26cef6bc0c1eab9``.

The source-rebuilt DTB compiles and its properties match the hardware-tested
DTB after resolving phandle renumbering, apart from the new ``dcpext_bw``
symbol. That rebuilt artifact has not had a separate physical monitor test.
Keep DT symbols enabled when building for the existing radio overlay.

Run the portable callback checks from the repository root with Python 3,
Clang (ASan/UBSan support), and dtc's ``fdtget`` on PATH::

    python3 tools/testing/selftests/drivers/phy/test_t8130_aux.py
    python3 tools/testing/selftests/drivers/phy/test_t8130_dp_state.py
    python3 tools/testing/selftests/drivers/phy/test_t8140_dptx_training.py
    python3 tools/testing/selftests/drivers/gpu/test_t8140_display_unplug.py
    python3 tools/testing/selftests/drivers/gpu/test_t8140_display_bandwidth.py --dtb path/to/t8140-j700.dtb

The display checks compile the actual changed callbacks with mocked firmware
calls. They also run pre-fix function excerpts and require the old ordering
and bandwidth-address invariants to fail. They do not emulate DCP firmware.

Physical evidence and remaining work
------------------------------------

On 28 September 2026, external enable was recorded at uptime 563.63 seconds,
disconnect at 616.67, and re-enable at 628.67. External poweroff completed at
615.742; the next 1080p60 modeset completed at 627.956. The user confirmed the
image returned after the explicit unplug/replug test. The captured interval
had zero display DART faults, DCP stalls, poweroff timeouts, flip timeouts,
crash-endpoint events or DPTX validation timeouts.

The capture stopped shortly afterwards. A later journal inspection attributed
the subsequent network loss to lid-triggered suspend. The bring-up session
therefore inhibits sleep, idle and lid handling while doing display tests;
real suspend/resume remains unqualified.

Still required: repeated hotplug cycles, both cable orientations, additional
sinks and modes, DPMS/menu stress and long-duration operation. USB3 capability
and connector detection alone are not evidence of a visible display. The
working run used host-assisted chainloading; installation of a standalone
Stage 1 and repair of the separately failing Recovery entry remain separate
work. None of these commits writes Apple boot policy or installs firmware.
