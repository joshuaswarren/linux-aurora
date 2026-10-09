#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Read J700 CPU/CPM energy counters twice without enabling or resetting them.

Native 25F84 AppleT8140PMGR getCPUCore{,Sram}EnergyAccumulator reads
cpu-impl + 0x9000/0x9008; getCPM{,Sram}EnergyAccumulator reads ACC +
0x48000/0x48008. Native energyAccumulatorToNanojoules uses 25 pJ/count.
These are hardware energy-model estimates, not rail/battery measurements.
Linux counters may be unconfigured: zero/stalled/decreasing counters do
not establish zero power. No configuration or coefficient writes occur.
"""
import argparse
import hashlib
import json
import platform
from pathlib import Path
import re
import subprocess
import time

BASES = dict(E0=0x210059000, E1=0x210159000, E2=0x210259000,
             E3=0x210359000, P0=0x211059000, P1=0x211159000,
             E_CPM=0x210e48000, P_CPM=0x211e48000)
REGS = {name + suffix: base + offset for name, base in BASES.items()
        for suffix, offset in [('', 0), ('_SRAM', 8)]}


def run(*args):
    return subprocess.check_output(args, text=True, timeout=30).strip()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--client', type=Path, required=True)
    ap.add_argument('--output', type=Path, required=True)
    ap.add_argument('--seconds', type=int, default=120)
    a = ap.parse_args()
    if not 10 <= a.seconds <= 1200:
        ap.error('seconds must be 10..1200')
    client = str(a.client.resolve(strict=True))
    system = platform.system()
    if system == 'Darwin':
        if run('sysctl', '-n', 'hw.model') != 'Mac17,5' or run('sw_vers', '-buildVersion') != '25F84':
            ap.error('requires reviewed J700 / 25F84')
        if 'abi_version=7\n' not in run(client, 'info') + '\n':
            ap.error('requires DevMem ABI 7')
        identity = run('sw_vers') + '\n' + run('sysctl', '-n', 'kern.bootsessionuuid')
    elif system == 'Linux':
        if not Path('/sys/firmware/devicetree/base/compatible').read_bytes().startswith(b'apple,j700\0'):
            ap.error('requires apple,j700')
        identity = Path('/proc/version').read_text() + Path('/proc/sys/kernel/random/boot_id').read_text()
    else:
        ap.error('requires Linux or macOS')
    a.output.mkdir(parents=True, exist_ok=False)
    result = dict(status='running', identity=identity, system=system,
                  started=time.time(), requested_seconds=a.seconds,
                  joules_per_count=2.5e-11,
                  tool_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  client_sha256=hashlib.sha256(Path(client).read_bytes()).hexdigest())

    def save():
        (a.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')

    def snapshot(label):
        data = {}
        with (a.output / (label + '.log')).open('x') as log:
            if system == 'Linux':
                p = subprocess.run([client], stdout=subprocess.PIPE, stderr=log, text=True, timeout=30)
                (a.output / (label + '.raw.json')).write_text(p.stdout)
                p.check_returncode()
                rows = {int(r['address'], 0): r for r in json.loads(p.stdout)}
                if set(rows) != set(REGS.values()):
                    raise RuntimeError('unexpected address set')
                return {name: rows[addr] for name, addr in REGS.items()}
            for name, addr in REGS.items():
                log.write('read64 ' + hex(addr) + '\n')
                log.flush()
                raw = run(client, 'mmio64', hex(addr))
                log.write(raw + '\n')
                m = re.fullmatch(r'([0-9a-fA-F]+): ((?:[0-9a-fA-F]{2} ){7}[0-9a-fA-F]{2})', raw)
                if not m or int(m[1], 16) != addr:
                    raise RuntimeError('unexpected MMIO response: ' + repr(raw))
                data[name] = dict(address=hex(addr), value=int.from_bytes(bytes.fromhex(m[2]), 'little'),
                                  time_ns=time.monotonic_ns())
        return data

    save()
    result['before'] = snapshot('before')
    save()
    time.sleep(a.seconds)
    result['after'] = snapshot('after')
    estimates = {}
    for name in REGS:
        b, e = result['before'][name], result['after'][name]
        delta, seconds = e['value'] - b['value'], (e['time_ns'] - b['time_ns']) / 1e9
        estimates[name] = dict(delta_counts=delta, seconds=seconds,
                               watts=delta * 2.5e-11 / seconds if delta > 0 else None,
                               status='increasing' if delta > 0 else 'stalled_or_reset_unqualified')
    result.update(status='complete', estimates=estimates)
    save()
    print(json.dumps(estimates, indent=2))


if __name__ == '__main__':
    main()
