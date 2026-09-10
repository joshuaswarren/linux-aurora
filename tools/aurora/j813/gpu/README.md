# M5 / J813 GPU bring-up

The native Linux diagnostic candidate identifies the physical T8142 GPU as
**G17G revision C0, one die, one cluster, ten active cores**. It does not yet
execute GPU work or expose a render node. DCP presentation is a later,
independent qualification; neither a software desktop nor encoder success
is evidence of GPU rendering.

## Hardware evidence, 2026-09-10

- Linux `7.1.6-aurora-j813-gpu-v9`, Asahi diagnostic module loaded explicitly.
- GPU ID words: `0b022000 000b0800 0011010a 00040404 01320311 10030302`.
- M5 identity selects 42-bit UAT input and output addresses; the Neo runtime
  cannot be enabled by substituting its 43-bit G17P configuration.
- Both GFX roles' loaded TEXT and DATA were captured twice, with matching
  reads, before Linux. Both report `RTKit-3255.160.4.release`.
- Six boot-local UAT/firmware DATA reservations are passed through m1n1.
- GPX and GFX physical power domains are represented with their measured
  dependency. GPX refused power-gating after the diagnostic probe; the next
  diagnostic DT pins it on pending a traced shutdown sequence. That DT change
  is not a power-management qualification.
- Native Wi-Fi obtained DHCP and returned 5/5 pings to `8.8.8.8` after the
  complete N1 startup sequence. Bluetooth initialized with zero TX/RX errors;
  pairing/audio were not repeated. MTP keyboard and trackpad initialized.
- Only simpleDRM `card0` exists. There is no `renderD128`, compute result,
  hardware-rendered frame, or DCP acceptance from this candidate.

The firmware capture identifies an observed build; it does not grant runtime
admission. G17G firmware initialization, both ASC lifecycle paths, UAT
ownership, submission and completion still need M5-specific implementation.
The existing source admits the Neo's G17P runtime only.

## Reproducible diagnostic candidate

`build-m5-gpu-kernel.sh` uses the J813 Lima builder and a separate build output.
It enables Rust/Asahi, device coredumps and built-in Apple mailbox support,
rebuilds the peripheral modules, and retains the qualified v8 files.
`build-m5-gpu-payload.sh` packages a separate RAM payload with Asahi automatic
loading blacklisted. `check-m5-gpu-dtb.py` checks every pre-existing peripheral
property and reference against the qualified v8 DTB.

RAM-chainload the matching m1n1 J813 GPU handoff carrier using the existing
guarded J813 path. `boot-linux.py --gpu --gpu-inventory` checks current-boot
resources, captures loaded firmware in private host files and boots the
candidate. Run `modprobe asahi` explicitly from the native shell to collect
the identification result. A successful module load is not a bound GPU driver:
the diagnostic probe intentionally returns `ENODEV` at the firmware boundary.

For radio regression, use the established N1 startup and protected WPA
configuration, then open normal data ring 25 **once** on the fresh boot.
After association, obtain DHCP with `udhcpc -B -q -f -n -i wlan0
-s /bin/n1-udhcpc -t 3 -T 3`, then run the Wi-Fi-bound ping. Omitting ring 25
leaves receive traffic working but no normal transmit submissions.

`gpu-boot-preflight.py` rejects changed register layouts, missing physical
power parents, overlapping/out-of-RAM reservations and invalid firmware
segment ranges. `test-gpu-preflight.py` exercises the reservation failure
cases. Boot addresses must always come from the current ADT.

## M5 tracing

`launch-m5-gpu-hv.py` uses the already qualified M5 headless macOS carrier and
kernel, retaining resident Stage1 and boot policy. `trace-m5-gpu-boot.py`
observes both ASC mailboxes and GPX/GFX registers before guest entry. Its
asynchronous callbacks only record supplied events; they issue no nested
proxy requests. It does not use the Neo init-data decoder or override the M5
firmware version. Raw traces and firmware binaries remain local.

The initial GPU tracer is prepared but not yet hardware-qualified.
