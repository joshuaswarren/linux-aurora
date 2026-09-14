#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Cross-check sleep power with an empirically calibrated raw charge counter.

Retains the capacity-based estimate separately. Calibration against battery
current is an internal consistency check, not an external meter calibration.
Never rewrites the original experiment captures.
"""
import argparse
import hashlib
import json
from pathlib import Path

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--calibration', type=Path, required=True)
p.add_argument('--sleep', type=Path, required=True)
p.add_argument('--reference', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
cal, sleep, reference = [json.loads(x.read_text())
                         for x in (a.calibration, a.sleep, a.reference)]
if cal['status'] != 'measured' or cal['mode'] != 'awake' or cal['elapsed_seconds'] < 30:
    raise ValueError('requires a completed awake current-integration calibration')
scale = cal['raw_charge_counter_drop'] / cal['sampled_current_integral_uAh']
if not 3.4 < scale < 3.8:
    raise ValueError('calibration does not reproduce the observed J700 3.6 scaling')


def estimate(x):
    if x['status'] != 'measured' or x['mode'] != 'sleep' or x['sleep_returncode']:
        raise ValueError('requires a completed, qualified sleep experiment')
    if x['kernel_notes_sha256'] != cal['kernel_notes_sha256']:
        raise ValueError('kernel identities differ; requalify the counter interface')
    raw = x['raw_charge_counter_drop']
    if raw <= 0:
        raise ValueError('counter did not record discharge')
    factor = x['mean_endpoint_voltage_V'] * 3600 / x['elapsed_seconds'] / 1e6
    return dict(elapsed_seconds=x['elapsed_seconds'],
                capacity_estimate_W=x['estimated_average_W'],
                fitted_counter_estimate_W=raw / scale * factor,
                counter_divided_by_3p6_estimate_W=raw / 3.6 * factor,
                capacity_loss_uAh=x['charge_loss_uAh'],
                fitted_counter_loss_uAh=raw / scale,
                voltage_endpoints_uV=[x['samples'][i]['voltage_now'] for i in (0, -1)])


result = dict(method='raw charge counter fitted to awake current integration',
              external_meter_calibrated=False, native_counter_units_confirmed=False,
              raw_units_per_uAh=scale,
              calibration_sampled_W=cal['sampled_average_power_W'],
              sleep=estimate(sleep), reference=estimate(reference),
              sources={str(x): hashlib.sha256(x.read_bytes()).hexdigest()
                       for x in (a.calibration, a.sleep, a.reference)})
before = result['reference']['fitted_counter_estimate_W']
after = result['sleep']['fitted_counter_estimate_W']
result['reduction_W'] = before - after
result['reduction_percent'] = (before - after) / before * 100
with a.output.open('x') as f:
    f.write(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, indent=2))
