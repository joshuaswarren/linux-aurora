# Native macOS sleep baseline on Neo

2026-09-16, macOS 26.5.2 build 25F84, battery 95%, no external power.
This is a native macOS reference, not a Linux power improvement.

The 600 s battery-counter interval started at 00:06:22 CEST and ended at 00:16:22.
The starting snapshot was only 0.238s older than the actual read. macOS logged
321 s sleep, 5 s DHCP maintenance DarkWake, then 269 s sleep and scheduled RTC wake.
Thus 590 s logged asleep; the600 s measured interval includes entry overhead and
maintenance work. The kernel boot identity was unchanged.

- PackCurrentAccumulator drop 6649 over 601 samples; mean voltage 4.3155V:
  estimated mean 0.047743W, energy 0.007957Wh.
- AccumulatedSystemLoad increase 30364 over 588 samples:
  estimated mean 0.051639W, energy 0.008607Wh.
- First freshly published wake-time SystemLoad sample: 0.030W.

Report this as approximately 0.05W (50 mW) averaged across the test, including
transitions and maintenance. Both counter methods are internal estimates,
not independently calibrated external-meter measurements. The accumulated
current is treated as a sum of mA samples and SystemLoad as mW samples; count
normalization gives their means. Observed sample rates were 1.002Hz and0.98Hz.
The awake cross-check gave 682.15mA from 60 accumulator samples, consistent with
endpoint reported currents 652/677mA, supporting this scale interpretation.
A mean voltage approximation is used for the current-based energy estimate.

The earlier 120 s trial slept 115 s and showed 0.025W in first refreshed telemetry,
but its starting counters were 36.6 s stale. Its mixed awake/sleep counter delta
is NOT a valid sleep average. Remaining charge also rose on relaxation; do
not use that reported-capacity delta as an energy integral.

The capture helper now waits for an observed fresh battery update, rejects
charging, records intermediate maintenance wakes, and waits until the scheduled
end rather than treating the first dark wake as completion. The report retains
both counter estimates, sampling rates and the matching pmset events.

Artifacts in the parent workspace:
- artifacts/neo-suspend/macos-sleep-600/{result.json,summary.json,pmset.log,tools.json}
- artifacts/neo-suspend/macos-sleep-120/ (earlier diagnostic)
Remote original capture: /tmp/neo-sleep-600-01/result.json on root@mac-neo.
Capture: `python3 macos-sleep-measure.py --seconds 600 --output NEWDIR`
Report: `python3 macos-sleep-report.py NEWDIR/result.json --output NEWDIR/summary.json`


## Awake on Wi-Fi, lid closed, display off

A 120-second interval on 2026-09-16, 01:19:45–01:21:45 CEST, measured
0.133561 W from the current accumulator and 0.132345 W from the power
accumulator: approximately **0.13 W**, or 0.0044 Wh over two minutes.
This is the whole Mac, not the Wi-Fi radio in isolation. Battery-only,
4.2875 V mean, 119 current samples and 113 power samples. Both endpoint
network checks reached the gateway on en0, independently identified as Wi-Fi.
Lid closed and SleepDisabled=true at both endpoints; no sleep/wake events
occurred in the interval. The display-off notification at 01:18:23 remained
the latest display notification throughout the capture.

The first attempt was stopped because pmset displaysleepnow printed error 1004
while exiting 0, and the display log still said ON. Current Apple pmset source
calls SLSDisplayManagerRequestDisplaysIdle for this operation:
https://github.com/apple-oss-distributions/PowerManagement/blob/main/pmset/pmset.m
The error does not prove global SleepDisabled caused the refusal. A one-minute
display-idle timer worked while system sleep remained disabled. The legacy
IORequestIdle fallback was compiled for investigation but never executed.

The completed capture hit a false boot guard after collecting all data:
kern.boottime microseconds changed 757380→669651, while boot seconds were
identical and the same uninterrupted Python process completed both endpoints.
Raw error status is retained; summary.json records the manual review. All
helpers now use kern.bootsessionuuid for new captures, keeping boottime only
as metadata. The previous 600-second report still reproduces unchanged.

Persistent macOS settings left for the user's awake/lid-closed experiment:
SleepDisabled=1, sleep=0 on battery/AC, displaysleep=1 minute. Original
settings are retained on Neo in /var/tmp/neo-wifi-idle/pmset-before-*.txt.
The temporary caffeinate assertion was released after measurement.

Local evidence: artifacts/neo-suspend/macos-wifi-idle-closed/.
Remote original: /var/tmp/neo-wifi-idle/closed-02/result.json.
