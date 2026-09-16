# J700 awake Wi-Fi power qualification

The target is whole-machine battery power with the LCD/backlight off and
Wi-Fi/SSH still available. This is separate from retained s2idle, which
currently disconnects the station. No 0.2 W Linux result is established yet.

`wifi-idle-trial.py` combines the qualified `neo-backlight-trial.ko` pmBL
command and `panel-gpio` LCD supply helper, then runs `sleep-power.py` in
awake mode after ten seconds of settling. Both display controls have bounded
automatic restoration, with LCD supply restored before the backlight. Run
under a transient systemd service and inhibit automatic sleep separately.
Keep external SSH health probes sparse and retain their timestamps; endpoint
`iw dev` output alone does not prove network reachability throughout the run.

```
python3 wifi-idle-trial.py --directory /root/dirtyroom-gpu/neo-suspend \
  --output NEW_DIRECTORY --seconds 120
```

Use the accumulated battery counter and the sampled current/power integral.
The remaining-charge gauge can move in the wrong direction when the load
changes. Do not call the raw accumulated counter microamp-hours without
calibrating its units (previous measurements found about 3.6 raw units/uAh).

## Station power-saving command

Native evidence: AppleSunriseWLAN.arm64e SHA256
`7914176d1062f9c3c956c6f507b012a0b49f6a12710e50acad8acad8cda8da23`.
Disassembly retained in `artifacts/neo-suspend/wifi-idle/` in the parent
workspace. `_nicConfigPowerSaveProfile` at 0x10033f3c4 sends legacy CID 0x05,
set=true, response-required=false, payload length 4. Bytes 0/1 are BSS index
and profile. `_wlanoidSet802dot11PowerSaveProfile` at 0x1001135bc accepts
profiles 0..2; its ordered string table is CAM / MAX PS / FAST PS at
0x1003d1e5b / 0x1003d1e5f / 0x1003d1e66. Only BSS 0 is supported by this
Linux driver's association path.

The two reserved bytes and cfg80211 timeout mapping are also documented in
MediaTek's published gen3 sources:
[command layout](https://android.googlesource.com/kernel/mediatek/+/android-mtk-3.18/drivers/misc/mediatek/connectivity/wlan/gen3/include/nic_cmd_event.h),
[cfg80211 mapping](https://android.googlesource.com/kernel/mediatek/+/android-mtk-3.18/drivers/misc/mediatek/connectivity/wlan/gen3/os/linux/gl_cfg80211.c).
These corroborate the native J700 command, not J700 firmware execution.

The experimental `mt7932_fullmac.station_ps=1` boot parameter exposes
cfg80211 power management. Disabled maps to CAM (0); enabled with the
default timeout -1 maps to FAST PS (2), otherwise MAX PS (1), matching the
published vendor mapping. Pre-association requests are cached and restored
after each successful association. Commands serialize with existing control
traffic and reject connection/scan/HIF transitions. The default boot path
does not enable this experiment. A submitted command is not an acknowledged
firmware state; measure power, incoming SSH after idle, reconnection, and
return to CAM before claiming support.

## Kernel #43 battery results (2026-09-16)

Boot `2254ffc7-7aae-4409-9fa4-ecc6339d97bc`, Image SHA256
`98348457c1a2ee3635a35346740ee014f587b65bb2a3aa44011588276c32bec5`,
source `f6ac7ac3a`. Multi-user, backlight and LCD supply off, awake and
associated, battery discharging, 120 seconds per sample after ten seconds
settling. Apple experimental cpuidle remained unbound. Whole-machine watts
below use sampled current times voltage integrated over the interval:

| Sequence | Station policy | PCIe policy | Watts |
| --- | --- | --- | ---: |
| 1 | CAM | default | 1.3354 |
| 2 | FAST PS | default | 1.1585 |
| 3 | FAST PS | powersupersave | 1.1637 |
| 4 | FAST PS | default | 1.1629 |
| 5 | CAM | default | 1.3345 |

Captures: `wifi-idle/wifi-cam-01/`, `wifi-ps-01/`, and
`wifi-policy-series43/` in the parent workspace's sleep artifacts. CAM
returned to within 1 mW of its initial measurement. Station PS therefore
resolves about 0.17 W reduction under these conditions; forcing PCIe L1.2
adds no resolved saving. These are marginal whole-system comparisons, not
a direct radio rail meter. Incoming SSH worked during the standalone pair;
the final series retained successful gateway probes after each stage. A
single FAST PS gateway probe took 621 ms, so latency still needs qualification.
Reconnection and application throughput are not established by these runs.
Raw accumulated counter/current calibration remained around 3.6 units/uAh;
remaining-charge estimates sometimes increased and are not used.

The earlier #42 `wifi-idle-cpupm-01` run overlapped upload/initramfs work;
exclude it from CPU power comparisons. Its 1.3676 W cannot resolve the
benefit of CPU PM. The uncontaminated #42 awake reference was 1.3394 W.

## GPU fabric isolation on the same boot

`neo-auto-idle.ko` changes only the unmanaged physical AFGGFXNI0 word at
Main PMGR+0x1b8. It rejects an existing DT provider, active GPU or unsafe
initial policy; it retains the manual ACTIVE target and lets hardware
arbitrate idleness. First 30-second trials with minimum state 4 (clock only)
and 0 (power allowed) both restored ACTIVE and original AUTO/floor policy.
No DCP CPU register or firmware mapping was changed.

Subsequent 120-second awake panel-off runs with station FAST PS:

| GPU fabric | End-of-measurement word | Whole-machine W |
| --- | --- | ---: |
| AUTO, minimum 4 | 0x1404434f | 1.1622 |
| AUTO, minimum 0 | 0x1000430f | 1.1691 |
| Original manual ACTIVE restored | 0x0f0043ff | 1.1664 |

Thus automatic power-off reached the native observed state, but no useful
battery saving was resolved. The initial immediate readback with minimum 0
was still CLKGATE; the later read reached OFF. Restore returned original
policy with `pending=N`, `failed=N`; sticky history bits need not match the
original word. Captures: `wifi-afg43/`, `wifi-afg-series43/`,
`wifi-afg-series43b/`. The module is not left active, and GPU rendering with
this policy has not been qualified. Do not install it as a production fix.

The first series used an inadequate fixed wait for the delayed restoration
work. Its assertion ran while `pending=Y`; a later read and kernel log
confirmed successful automatic restoration. The controller then stopped that
already-measured series and unloaded the restored module. The second series
waited for actual `pending=N` with a bounded deadline and completed. Preserve
the first series's incomplete JSON alongside the final kernel log rather than
rewriting it as a fully passed run.

## CPU PM and reconnect checks

The uncontaminated #43 120-second CPU-PM comparison measured **1.1802 W**
with station FAST PS and all six `WFI-test` states enabled. All CPUs recorded
entries and about 128–130 seconds residency over the surrounding test span;
the driver was then unbound and all previous parameters restored. The nearby
unbound reference was 1.1664 W, so no reduction is demonstrated. Capture:
`wifi-cpupm43/`. This does not introduce a new CPU power-down state.

A reconnect with the original NetworkManager profile succeeded, but NM
selected CAM during activation; it therefore did not test FAST PS retention.
The retained `wifi-reconnect43/result.json` has `ps=off`; its misleadingly
named `ping_ps` field must not be interpreted as a PS-on test.

A second reconnect temporarily set the existing NM profile's powersave
setting to enabled (3). It succeeded, `iw` read back PS on, and the driver
resubmitted profile 2 before `WPA2_AUTHORIZED`. Ten gateway pings immediately
after reconnect had no loss, mean 3.507 ms, max 7.140 ms. After selecting CAM,
ten more had no loss, mean 3.165 ms. The profile was restored to its previous
(default=0) setting without changing persistent credentials, and PS was left
off. Capture: `wifi-reconnect43b/`. External SSH also returned after both
reconnections. This qualifies one requested-policy reconnect, not sustained
throughput or long-idle latency: earlier PS-on probes reached 519–821 ms.

The CPU and AFG experiments are restored, LCD/backlight restored, original
PCIe policy restored. Kernel #43 stays in the one-shot experimental slot;
normal #6 remains the GRUB default. No upstream synchronization occurred.
The achieved awake display-off result is about **1.16 W**, not 0.2 W. See
[dcp-iboot-power.md](dcp-iboot-power.md) for the firmware display-power path
and the unresolved transport/mapping work needed before testing it.
