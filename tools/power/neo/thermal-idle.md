# Explicit minimum-frequency awake idle policy

Kernel #43, without a desktop or measurement sampler, produced 187.4 SMC
mailbox interrupts/s, 279.3 architectural timer interrupts/s and 275.4
function-call IPIs/s over a 30-second capture. The SMC thermal sampler reads
six selected keys every 25 ms; its zone separately polls the cached result
and a third worker checks freshness every 25 ms. This is a concrete source
of awake activity, not yet a measured allocation of watts. Capture:
parent workspace `artifacts/neo-suspend/wifi-idle/awake-irqs43/`.

`apple_smc_cpu_thermal.idle_poll_ms` is an opt-in minimum-frequency policy:

- 0: original 25 ms sampling/watch cadence.
- 100..1000: selected sampling/watch interval in milliseconds; both CPU
  clusters remain capped at their minimum OPP through the existing independent
  thermal QoS guards, regardless of temperature or freshness.
- Other values fail without changing policy. A failed guard update prevents
  entering slower polling. On exit, three new normal samples are required
  before the SMC guard may be released; other thermal constraints still apply.

The 200 ms cache/host-freshness limit and trip temperatures are unchanged.
At intervals over 200 ms, cached temperature reads may correctly report stale
between samples. Old values never become permission for higher frequencies;
SMC performance readiness/live admission remains revoked in this mode. This
is a low-performance idle experiment, not adaptive full-speed thermal policy.

The SMC zone now relies on the sampler's existing explicit updates rather
than also periodically rereading the same cache. Its governor still receives
every completed sample. The independent watchdog and error-driven minimum
caps remain. No thermal protection is disabled, and the PMP policy is unchanged.

Hardware qualification and battery comparisons are pending. Start with 100 ms,
verify both effective CPU maxima remain at their minimum frequencies under
load and after a user maximum request, then test restoration. Compare battery
power separately while preserving panel-off and associated Wi-Fi conditions.

## Kernel #44 admission checks

Image SHA256 `5a1f8f5f2090a9c73f9f4f57b5e00a197f79793e4ce78fb70805c9b084e1e8e4`,
source `b027f32e2`, boot `c1ac7bb7-6e41-4d97-89bd-7a0fbc4f67bd`.
The 1-second policy passed a 15-second, six-worker CPU load with both governors
set to performance: 61 readback samples kept both requested current and maximum
frequencies at E744/P816 MHz. The driver readback is nominal P-state frequency,
not a physical clock meter. Governors and polling setting returned to their
original values. Five invalid parameter values failed with EINVAL and left
the mode unchanged. Captures: `wifi-thermal-qualification44b/`.

The first harness attempt failed because Python 3.14 uses forkserver and the
script lacked a main guard. Its cleanup also tried to join an unstarted child.
The controller restored schedutil and normal polling explicitly, then the
revised single-threaded harness used an explicit fork context and guarded joins.
Keep `wifi-thermal-qualification44/harness-failure.log` as the failed tool run;
it is not a hardware failure or a passing load test.

## Reproducible paired measurement

`idle-profile-trial.py` wraps the panel-off measurement with explicit station
and CPU policies. It records frequency limits, IRQ counts, sample identities
and a gateway probe; restores both policies independently on normal/error
exit; and verifies restoration readbacks. Run it in a transient service,
with the sleep inhibitor active, then test incoming SSH separately:

```
python3 idle-profile-trial.py --directory /root/dirtyroom-gpu/neo-suspend \
  --output NEW_DIRECTORY --idle-poll-ms 100 --power-save on --seconds 120
```

Use 0 for the normal-polling reference. The child independently bounds panel
and backlight off time. Battery wattage applies to the idle interval, not the
later gateway probe. Do not overlap these trials with other policy changes,
CPU loads, uploads, or suspend experiments.

## Paired awake result on #44

Four 120-second panel-off, associated FASTPS intervals measured 1.159145 W
(normal), 1.158336 W (100 ms), 1.151350 W (1000 ms), and 1.151085 W
(restored normal). The final reference is indistinguishable from the 1-second
setting: no resolved power benefit. SMC IRQs over the surrounding ~130-second
interval fell from 25,890 to 8,904 at 100 ms, so the activity reduction is real
but is not the missing >0.15 W. Gateway probes passed; policies restored to
normal sampling and CAM. Captures: wifi-thermal-series44/.
