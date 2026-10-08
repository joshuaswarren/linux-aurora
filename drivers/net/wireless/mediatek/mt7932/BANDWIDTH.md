# Private J700 bandwidth experiment

This code uses **tainted native 25G83 research**, is not clean-room work, and
has not been RF tested. Nothing in this experiment establishes publication
suitability. The default remains the previously qualified 20 MHz path.

For the built-in driver, boot with `mt7932_fullmac.max_bw_mhz=40` or
`mt7932_fullmac.max_bw_mhz=80`. The underscore prefix is the Kbuild module name,
not the PCI driver's hyphenated name. The parameter is read-only at runtime;
changing it requires a reboot. Invalid values select 20. The sysfs parameter
reports the request; `ethtool -S` reports the effective limit and fallback latch.

The three bandwidth enums must not be interchanged:

| Field | 20 MHz | 40 MHz | 80 MHz |
| --- | --- | --- | --- |
| Config `StaBw` and `Sta5gBw` (MAX_BW) | 0 | 1 | 2 |
| CID0f 5 GHz selector (CONFIG_BW) | 1 | 0 | 0 |
| EE4d association-start CW | 0 | 0 | 1 |
| EE4d secondary channel / SCO | 0 | 1 above / 3 below | 0 or consistent HT secondary |
| mt7921 CMD_CBW / compact RXV width | 0 | 1 | 2 |

Public definitions: MediaTek gen4 `include/wlan_lib.h` (MAX_BW),
`include/mgmt/rlm.h` (CONFIG_BW), `include/nic/wlan_def.h` (CW/SCO);
in-tree `mt76_connac.h` (CMD_CBW), `mt7921/mcu.c` (center-channel/SCO).
Public MediaTek `wlan_lib.c` also reads the separate station-wide `StaBw`
and 5 GHz `Sta5gBw` maxima. The first 80 MHz candidate only raised
`Sta5gBw`; this revision raises both under the same boot opt-in and lowers
both in the safe fallback. `BW_CONFIG` reports the original and selected
values. `BW_BSS` reports the AP VHT operation width and segments from the
Linux scan entry before submitting a wide join. An 80 MHz AP on primary 149
should advertise VHT width 1 and segment 0 channel 155. Neither log is a
firmware policy readback or proof that the firmware selected 80 MHz.

Native `AppleSunriseWLAN` SHA-256:
`ff462b980c4073b462a77f9e817c95ad52aeebfa0655ab00d639ca224cb5ed0c`.
The EE4d consumer at 0x100160a9c calls `AisPreloadCal` (0x100348ff8).
The latter derives center with `nicGetVhtS1` (0x10033ded0), or, for CW=0,
`rlmDomainGetCenterChannel` (0x10035f2e0). `rlmOneTimeCalSet` at
0x10020beac..0x10020bed4 packs that center into bits 24:17 and selects its
calibration group. `GROUP_FREQ_DEF_ARR` at 0x100471fc2 maps the admitted
wide lower block to group 1 and upper block to group 7. Only the wide path
uses this new group selection; the existing 20 MHz group choice is retained.
The connect builder at 0x1003493f8 supplies band/primary at 0x338/0x339;
there is no evidence for inserting width/center into the following padding.

The opt-in policy opens only regulatory-usable 5 GHz channel definitions.
Every constituent channel must exist and permit the width; NO_IR, disabled,
DFS and power-policy restrictions remain. Channel 165 and all 2.4 GHz
channels remain 20 MHz. HT/VHT/HE host capabilities are bounded by the PHY
capability reply. Firmware still builds association IEs; HE capability
advertisement alone does not prove HE negotiation.

After a submitted 5 GHz join fails, the width latch stays at 20 until reboot.
Following clean firmware disconnect and DMA/token retirement, the next
userspace connect narrows host capabilities and submits StaBw=0, Sta5gBw=0 plus the
20-only CID0f policy before joining. A terminal negative response to the
single wide association calibration can be retired only at that same fence.
Firmware SET submissions are not acknowledged policy readbacks.

**Limitation: no same-boot recovery from a hard firmware assert.** EIDf0 is
confirmed by native `arEventTable` entry 31 as `nicEventAssertDump`. The
experiment logs it, latches 20, wakes failed waiters, stops RF admission and
retains DMA storage. It does not clear ownership/error flags or invent a
firmware reset. An assert, malformed calibration, timeout with uncertain
ownership, or failed retirement can still require rebooting with 20 MHz.
Thus this does not satisfy a guarantee that a bad wide setting can never
strand Wi-Fi during that boot. A reusable crash-reset/DMA epoch recovery
implementation needs separate work and hardware validation.

Optional built-in `CONFIG_MT7932_BW_KUNIT_TEST=y` (requires `CONFIG_KUNIT=y`)
adds suite `mt7932-bandwidth`: enum separation, centers, 2.4 GHz/cap rejection,
secondary-channel regulatory restrictions, host fallback and synthetic
calibration parameters. Object compilation is not KUnit execution.
