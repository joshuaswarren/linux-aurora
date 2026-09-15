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
