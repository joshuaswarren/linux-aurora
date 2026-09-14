#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Retain raw J700 SMC power floats and battery readings; no rail labels inferred.

Samples are taken only while userspace runs. A gap across suspend does not make
the first post-resume reading an average over sleep. Never sum overlapping keys.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import struct
import time


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--seconds', type=int, default=120)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    if not 1 <= a.seconds <= 3600:
        p.error('seconds must be 1..3600 (elapsed boottime, including suspend)')
    raw_path = Path('/sys/kernel/debug/neo-smc-power/raw')
    if not raw_path.exists():
        p.error('load neo-smc-power.ko first')
    a.output.mkdir(parents=True, exist_ok=False)
    metadata = dict(boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                    kernel=Path('/proc/version').read_text().strip(),
                    kernel_notes_sha256=hashlib.sha256(Path('/sys/kernel/notes').read_bytes()).hexdigest(),
                    source_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                    requested_seconds=a.seconds, status='running')
    (a.output / 'metadata.json').write_text(json.dumps(metadata, indent=2) + '\n')
    start = time.clock_gettime(time.CLOCK_BOOTTIME)
    battery = Path('/sys/class/power_supply/macsmc-battery')
    with (a.output / 'samples.jsonl').open('x', buffering=1) as out:
        while time.clock_gettime(time.CLOCK_BOOTTIME) - start < a.seconds:
            sample = dict(boottime=time.clock_gettime(time.CLOCK_BOOTTIME),
                          monotonic=time.monotonic(), unix_time=time.time())
            raw = raw_path.read_text()
            values = {}
            errors = {}
            for line in raw.splitlines():
                key, bits, ret = line.split()
                value = struct.unpack('<f', struct.pack('<I', int(bits, 16)))[0]
                if int(ret) or not math.isfinite(value):
                    errors[key] = dict(errno=int(ret), bits=bits)
                else:
                    values[key] = value
            sample.update(raw=raw, values=values, errors=errors,
                          battery={key: (battery / key).read_text().strip()
                                   for key in ('status', 'power_now', 'current_now',
                                               'voltage_now', 'charge_counter', 'charge_now')},
                          completed_boottime=time.clock_gettime(time.CLOCK_BOOTTIME))
            out.write(json.dumps(sample, allow_nan=False) + '\n')
            time.sleep(1)
    metadata['status'] = 'completed'
    (a.output / 'metadata.json').write_text(json.dumps(metadata, indent=2) + '\n')


if __name__ == '__main__':
    main()
