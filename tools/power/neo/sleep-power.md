# Battery power measurement on J700

`sleep-power.py` measures one awake interval or one already-qualified timed
s2idle cycle. Run it on battery, with AC reporting offline, and keep the panel
brightness, displayed image and connected peripherals unchanged between runs.
It does not configure sleep support. Use the retained development boot and
RTC setup described in `rtc-alarm.md` first. To measure with the display off,
load the qualified `neo-display-sleep` module described in `display-power.md`
and add `--display-off`; the helper verifies its off/resume callback counters.

```sh
python3 sleep-power.py --mode awake --seconds 60 --output NEW_AWAKE_DIRECTORY
python3 sleep-power.py --mode sleep --seconds 600 --output NEW_SLEEP_DIRECTORY
python3 sleep-power.py --mode sleep --seconds 600 --display-off --output NEW_DISPLAY_OFF_DIRECTORY
```

For sleep, launch the command through a service that survives SSH disconnects.
The helper records the boot ID, kernel identity, sleep-helper hash, battery
endpoints and nested sleep qualification. It samples no battery data during
sleep. Optional `--screen-directory` forwards the existing visual phase test
to the sleep helper; otherwise no visual test window is controlled. The RTC provides the timed
wake; there is no userspace timeout capable of recovering a stuck kernel.
Reject a failed sleep qualification, observed external power, changed boot ID
or a nonpositive remaining-charge delta.

## Estimate and limitations

The remaining-capacity estimate is:

```
energy_Wh = (charge_now_before - charge_now_after) / 1e6
            * (voltage_now_before + voltage_now_after) / 2e6
average_W = energy_Wh * 3600 / elapsed_boottime_seconds
```

This uses the fuel gauge's remaining-charge estimate and mean endpoint voltage,
not an independently calibrated energy meter. The measured interval includes
helper preparation and suspend/resume overhead. Report the nested sleep
duration separately. `charge_now` has 1 mAh resolution: at roughly 4.1 V, one
step corresponds to about 0.025 W over ten minutes, before other gauge errors.
Short awake endpoint measurements have much larger relative quantization.

The driver derives `energy_now` from the same remaining-charge reading and a
nominal 3.8 V. Retain that result separately; it is not a second independent
energy accumulator. An instantaneous power reading after resume, including
UPower's potentially cached rate, does not measure the preceding sleep average.
The AC supply exposes an input power limit, not integrated charger energy.

## Charge-counter calibration issue

The driver currently copies SMC `BAAC` into `charge_counter` without scaling.
An initial 60.011-second awake calibration observed a raw counter drop of
52,854 against a sampled-current integral of 14,640.34 uAh: a ratio of 3.61016.
Treating the raw counter as uAh produced an invalid 13.020 W estimate, while
sampled battery power averaged 3.617 W. Dividing the counter by 3.6 agrees
closely with that rate, suggesting milliamp-seconds, but the native counter
semantics are not yet confirmed. This helper retains raw counter differences
separately from its capacity-based `estimated_average_W` field. It does not
change the driver. The later display-off test below demonstrates why that
capacity estimate must be cross-checked after a large load change.

The original calibration JSON is preserved in the workspace at
`artifacts/neo-suspend/power/awake-01.json`; its original counter-derived
`estimated_average_W` field is invalid. Recalculating from its 15 mAh remaining
charge loss and 4.1065 V mean voltage gives about 3.695 W. Use its sampled
3.617 W as the awake reference, with the different measurement methods stated.

## First ten-minute screen-on result

On 2026-09-14, build #19 completed a requested 600-second retained s2idle
cycle on battery with the same static BEFORE SLEEP frame as the calibration.
The battery endpoints spanned 601.665 seconds. Remaining charge fell from
6,964 to 6,843 mAh; voltage fell from 4.104 to 4.094 V. This gives 0.495979 Wh
and **2.968 W estimated average**, including transition overhead. Compared
with the 3.617 W sampled awake baseline, this suggests about 0.649 W / 18%
lower consumption. Different measurement methods and the single short awake
reference limit the precision of that comparison.

The suspend request spanned 601.342 seconds boottime and 0.725 seconds
monotonic, about 600.617 seconds of suspended timekeeping. The PMU alarm fired,
all six CPUs recorded s2idle entries, the boot ID stayed unchanged and all PM
failure counters stayed zero. CPU 0/4 recorded 1,647/1,628 entries, indicating
intermittent retained interrupt activity rather than uninterrupted CPU sleep.
Wi-Fi reconnected; retained DART/radio checks passed. The alarm was disarmed.

The image remained unchanged throughout the power interval. After collecting
the result, phase 2 was requested manually; an independent screenshot showed
WOKEN UP! :D, with the same renderer PID 2105 and GLES context. Display
power-down remains unimplemented. This is a screen-on retained-s2idle baseline,
not a measurement of complete platform deep sleep.

## Ten-minute display-off result and counter cross-check

On the same #19 kernel, boot `eee6042d-cc10-4c2d-8cf4-66e01a884832`,
the combined LCD/backlight hook completed a 600-second RTC s2idle test.
Battery endpoints spanned 601.542 s. The raw accumulated counter dropped
123,441 units, but remaining capacity dropped only 16 mAh while endpoint
voltage rose from 3.883 to 3.896 V. The uncorrected capacity calculation
therefore reported **0.372 W**. Do not use that as the headline result:
the counters disagree substantially during this low-load interval.

A new 60-second awake calibration on the same boot integrated 15,117.59 uAh
and reproduced **3.600772 raw units/uAh**, with sampled power **3.51843 W**.
Using that fitted scale gives **0.79798 W** for display-off sleep. The earlier
screen-on capture gives **2.98789 W** using the same method: **2.18991 W /
73.29% lower**. The first sensor sample immediately after wake was 0.790 W,
which is a consistency observation, not a substitute for the accumulated
counter measurement. Remaining-capacity gauge compensation is a possible
explanation of the discrepancy; its exact internal algorithm is unverified.

Use `power-counter-compare.py` with `--calibration`, `--sleep`, `--reference`,
and a new `--output` path to reproduce the comparison without changing the
original captures. It requires a completed awake calibration and the same
kernel identity, retains source hashes and both estimates, and explicitly
labels the result as internally calibrated rather than externally metered.
The native unit interpretation remains unconfirmed. Different battery charge,
voltage and temperature between the two sleep runs limit comparison precision;
report approximately **0.80 W**, not the full fitted decimal precision.

Evidence under `artifacts/neo-suspend/display/`:
`display-power-sleep-600-01/`, `power-counter-calibration-01/`, and
`display-power-sleep-600-comparison.json`. No battery sampler ran during sleep.

Evidence: `artifacts/neo-suspend/power/power-sleep-600-01/` in the workspace,
including endpoint data, nested kernel snapshots and the post-test screenshot.
Boot ID: `c96d75a7-fc5d-41d5-8a2e-fd0fd584e962`. Kernel Image SHA256:
`4dfa6dc15084b22481c75cd858b0861a64bc4e45b79a8deb3c37ffa648e620b0`.
