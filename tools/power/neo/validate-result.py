#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Re-check saved pm-test/sleep-test results offline.

For each result.json, find the innermost record with before/after snapshots
(descending through sleep_result), then apply the firmware health check to the
after snapshot and compare /sys/power/suspend_stats failure counters across the
transition. Prints PASS or FAIL per file; exits 1 if any fail.
"""
import argparse
import importlib.util
import json
from pathlib import Path


def load_pm():
    spec = importlib.util.spec_from_file_location('neo_pm', Path(__file__).with_name('pm-test.py'))
    pm = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(pm)
    return pm


def innermost_record(record):
    while isinstance(record, dict) and not ('before' in record and 'after' in record):
        if 'sleep_result' not in record:
            raise ValueError('no record with before/after snapshots')
        record = record['sleep_result']
    if not isinstance(record, dict):
        raise ValueError('no record with before/after snapshots')
    return record


def validate(pm, path):
    record = innermost_record(json.loads(Path(path).read_text()))
    if record.get('status') != 'returned':
        raise RuntimeError(f'status={record.get("status")!r}')
    pm.check_firmware_health(record['after'])
    return pm.check_suspend_stats(record['before'], record['after'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('results', nargs='+', type=Path, metavar='RESULT_JSON')
    args = parser.parse_args()
    failed = 0
    for path in args.results:
        try:
            summary = validate(load_pm(), path)
        except (OSError, ValueError, KeyError, RuntimeError) as error:
            failed += 1
            print(f'FAIL {path}: {error}')
            continue
        print(f'PASS {path}: no suspend_stats failure counters increased; '
              f'deltas={summary["deltas"]}')
    return int(failed > 0)


if __name__ == '__main__':
    raise SystemExit(main())
