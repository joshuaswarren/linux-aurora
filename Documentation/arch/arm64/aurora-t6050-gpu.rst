.. SPDX-License-Identifier: GPL-2.0-only

T6050 GPU integration
====================

The t6050-gpu branch extends t6050-base with the M5 Pro hardware and
firmware layouts and an SPTM-owned GPU translation backend. The direct
Neo backend, public driver statistics and the common DRM/Rust APIs remain
available. The existing Neo heap shrink policy stays confined to that
qualified geometry; this series does not enable it for M5.

Transport and ownership
-----------------------

The GPU backend uses a separate compatibility batch provider,
AURORA_APIF_GPU. Its operations have a 64-byte wire layout and its
completion prefix determines which mutations took effect. It is not an
APIF v2 DART/SART provider. Client references survive provider removal,
but removal revokes access and drains any synchronous submission before
releasing its mappings.

The mailbox profile describes the coprocessor view of the GFX/GFX1 ASC
windows. The legacy resource and compatible names remain accepted so the
existing loader can describe this provider. GPU table ownership, context
binding and frame retyping remain with the monitor; Linux retains its
mapping and command lifetime rules.

Loader contract
---------------

Before GPU startup, the loader must supply the GPU aperture, GFX/GFX1
mailboxes, firmware-owned shared regions, GPU region and handoff metadata,
and the compatibility UAT batch page. All retained firmware regions must
remain reserved in the guest. Leakage calibration must come from the
machine's factory values with a successful calibration status; no board
values or firmware binaries are included here.

The cleaned non-GPU loader ownership requirements in
``aurora-apif-platform.rst`` still apply. A DT converted from a live guest
cannot establish ownership of queues, clocks, reset lines or streams.

Qualification
-------------

This integration candidate is for explicit hardware qualification.
M5 firmware and DRM startup retain their existing opt-in controls;
loading the module does not silently enable an unqualified startup path.
Transport tracing and the discarded startup register-dump loop are
removed. Optional platform configuration happens once at initialization,
not in the command submission path.

The current production queue fixes are retained: retirement failures are
returned to the caller, prepared context ownership is bounded, and only
idle physical compute queues may be evicted under ASID pressure. Active
work, replay and growth keep their existing ownership.

A successful native build does not establish that this combination boots,
that an Aura loader has established every required resource claim, or
that GPU workloads pass on hardware. Those checks precede deployment or
promotion.

Licensing
---------

New Aurora sources are GPL-2.0-only. Modified shared files retain the MIT
terms for inherited content alongside GPL-2.0-only Aurora additions; their
combined SPDX expression is GPL-2.0-only AND MIT. The module is distributed
under GPL v2. Existing inherited notices and the kernel's MIT license text
are preserved.
