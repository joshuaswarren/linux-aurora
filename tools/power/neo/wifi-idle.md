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
