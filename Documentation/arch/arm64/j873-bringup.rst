J873g M6 Mac mini firmware preparation
====================================

The local ``j873`` branch starts from M5/J813 commit
``a4faee845ed1890a0d4bafa9f155ebbbe5f0715a``. Its publication remote is
``aurora`` (``aurora-silicon/linux-aurora``). No physical M6 is available.

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

Work order
----------

1. Determine bindings and support for PMGR2 service control, gen3 DART, and
   ASC v8. Keep J813 behavior unchanged.
2. Build T8152 and J873g DTS files from verified interrupt, register, power,
   clock, and DMA relationships. Validate with dtc and dt-schema.
3. When hardware is available, validate console and interrupts, then power
   control, DMA isolation, storage, USB, networking, and display in stages.

No J873 DTS has been enabled yet and no kernel or hardware test is claimed.
The current macOS host filesystem is case-insensitive and produces 13 upstream
case-collision modifications on a fresh Linux checkout. Those files are not
J873 changes; use a case-sensitive filesystem for a full kernel build.

The sibling AuroraSilicon repository holds the full evidence and extraction
instructions under ``targets/j873``. Raw local firmware is outside Git at
``../firmware/26A428/26A428__MacOS``.
