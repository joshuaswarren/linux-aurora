J873g M6 Mac mini firmware preparation
====================================

The local ``j873`` branch starts from M5/J813 commit
``a4faee845ed1890a0d4bafa9f155ebbbe5f0715a``. Its publication remote is
``aurora`` (``aurora-silicon/linux-aurora``). No J873 hardware validation has
been recorded in this workbench.

Apple's macOS 27.0 RC ``26A428`` restore firmware confirms ``Mac18,5``,
``J873gAP``, ``T8152``, and board ID ``0x24``. The J873g IM4P device tree
matches the SHA-384 digest in the archive's root BuildManifest. The sibling
``j873sap`` tree is a distinct T6050 platform and must not supply M6 data.

The source baseline has no matches for ``T8152``, ``t8152``, ``pmgr2``,
``service-gates``, ``dart,gen3``, or ``ascwrap-v8`` in its drivers and Apple
device trees. Firmware compatibility strings need driver and binding analysis;
they must not be copied into a Linux DT with unsupported fallback assumptions.

Port inputs
-----------

* Static ADT CPU descriptors: 6 E, 2 P, 4 M. Enabled topology and CPU features
  remain untested on hardware.
* UART0: physical ``0x331200000``, size ``0x4000``; uses ``service-gates``.
* AIC: ``aic,3``, physical ``0x301000000``, size ``0x1cc000``.
* PMGR parent: ``pmgr2,arch``; register-bearing child: ``pmgr2,t8152`` at
  ``/arm-io/pmgr-child``. Its register zero is ``0x300700000``.
* DART: ``dart,gen3``; USB0 DART register zero is ``0x403020000``.
* ANS/GPU ASC: ``iop,ascwrap-v8``. ANS register zero is ``0x321600000``.
* PCIe: ``apcie,t8152``. USB DRD: ``usb-drd,t8152``. GPU: ``gpu,t8152``.

All listed MMIO addresses include ADT bus-range translation. They are static
firmware evidence, not observed live register behavior.

Offline integration and validation
----------------------------------

The Apple platform binding now recognizes the evidenced board identity
``apple,j873g``, ``apple,t8152``, ``apple,arm-platform``. This names the platform;
it does not enable devices or claim that an older SoC fallback is compatible.
The updated binding passes ``dt-doc-validate``.

Run the firmware-to-driver audit from this kernel checkout::

  ../.venv/bin/python tools/testing/selftests/arm64/apple/j873_firmware_audit.py \
    ../AuroraSilicon/targets/j873/topology-26A428.json

It checks the 525-node inventory, including unique indices for same-name
siblings, critical translated register windows, compatible strings, and all
12 CPU descriptors. It reports the current AIC, DART and power-domain driver
match tables. A match-table observation is not proof of hardware support.
First validate the underlying firmware with AuroraSilicon's inspector.

AppleT8152PMGR driver analysis establishes separate PMGR2 device and service
namespaces and a PMS-mediated service-control path. AppleGen3DART delegates
translation changes to SPTM; SPTM analysis identifies different SID/TTBR and
TLBI handling. ASCWrap-v8 uses postoffice mailboxes. Adding T8152 aliases to
the M5 power, DART or ASC drivers would therefore skip necessary port work.

The sibling m1n1 branch provides an opt-in J873g diagnostic which builds and
runs in an instruction harness with synthetic CPU/UART inputs. That result
validates software control flow only. Linux CPU ``reg`` properties must use
measured MPIDR affinity fields, not the static ADT's logical CPU IDs. In
particular, do not reuse J813's measured performance-core affinity-2 bit.

AGX source sync
---------------

The ``j873`` branch first copied the committed ``drivers/gpu/drm/asahi`` changes
from Linux ``j813`` commit ``2596a20e31f810c24c88999d163027cbf9277f20``.
It then merged the newer J813 tip ``78828af3d927``. The AGX driver tree now
matches that tip, while the J873 firmware and identity work remains present.
Mesa's matching ``j873`` branch has merged its J813 tip ``7fbab581d3bc``.
The J813 mailbox CPU-state API, its Rust wrapper, related T8142 mailbox
binding, and the PMGR power-state correction are also carried because the
AGX driver uses that API and power path. They add no T8152 match.

These sources describe tested M5/G17G and related G17P paths. The J873g
firmware advertises ``gpu,t8152`` and an ASCWrap-v8 GPU mailbox, but the
current driver has no T8152 GPU match or J873 boot DT. Do not use an M5
compatible string to enable it. The GPU generation, firmware contract, DMA
translation, power sequencing, and command submission require target evidence
and validation before J873 runtime support can be claimed.

For this merge, 101 of 110 selected standalone AGX scripts passed. Six Rust
test harnesses failed to compile, two Python assertions failed, and one test
requires a local ``gfx-text`` binary fixture absent from this workbench.
All four focused M5 render checks passed. These are host checks of the J813
source; no full J873 kernel build or M6 GPU execution was performed.

Remaining work order
--------------------

1. Determine bindings and support for PMGR2 service control, gen3 DART, and
   ASC v8. Keep J813 behavior unchanged.
2. Build T8152 and J873g DTS files from verified interrupt, register, power,
   clock, and DMA relationships. Validate with dtc and dt-schema.
3. When hardware is available, validate console and interrupts, then power
   control, DMA isolation, storage, USB, networking, and display in stages.

No J873 boot DTS has been enabled and no full kernel or hardware test is claimed.
The event aperture, live CPU affinities, timer routing, memory reservations,
and power ownership are prerequisites to a usable first-boot device tree.
The current macOS host filesystem is case-insensitive and produces 13 upstream
case-collision modifications on a fresh Linux checkout. Those files are not
J873 changes; use a case-sensitive filesystem for a full kernel build.

The sibling AuroraSilicon repository holds the full evidence and extraction
instructions under ``targets/j873``. Raw local firmware is outside Git at
``../firmware/26A428/26A428__MacOS``.
