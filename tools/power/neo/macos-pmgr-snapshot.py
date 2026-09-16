#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Read J700 PMGR status through the existing DevMem ABI 7 client.

No register writes, bulk MMIO dumps, or virtual-domain accesses. Only the
99 physical state words in our corrected ADT table are admitted. Reads are
sequential, not an atomic snapshot or a sleep residency measurement: running
this program wakes CPUs and may affect fabric/storage state. Compare policy
bits separately from instantaneous ACTUAL and sticky history flags.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time


TABLE_SHA256 = '67267feae9e57dbfaa513b303947f4025d129f162a1f2ce72a0d75be769d0d25'


def run(*argv):
    return subprocess.run(argv, check=True, capture_output=True,
                          text=True, timeout=15).stdout


def decode(value):
    return dict(target=value & 15, actual=(value >> 4) & 15,
                auto_enable=bool(value & (1 << 28)),
                auto_state=(value >> 24) & 15, minimum=(value >> 16) & 15,
                busy=bool(value & (1 << 11)),
                device_disable=bool(value & (1 << 10)),
                was_clock_gated=bool(value & (1 << 9)),
                was_power_gated=bool(value & (1 << 8)))


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--table', type=Path, default=Path(__file__).with_name('j700-pmgr-domains.json'))
    ap.add_argument('--client', default='/usr/local/bin/devmem-v12')
    ap.add_argument('--output', required=True, type=Path)
    ap.add_argument('--names', nargs='+', help='Subset of physical domain names')
    args = ap.parse_args()
    raw = args.table.read_bytes()
    if hashlib.sha256(raw).hexdigest() != TABLE_SHA256:
        ap.error('table identity differs from the reviewed J700 inventory')
    table = [d for d in json.loads(raw) if not d['virtual']]
    assert len(table) == 99
    if args.names:
        unknown = set(args.names) - {d['name'] for d in table}
        if unknown:
            ap.error('unknown or virtual domains: ' + ', '.join(sorted(unknown)))
        table = [d for d in table if d['name'] in args.names]
    if run('sysctl', '-n', 'hw.model').strip() != 'Mac17,5':
        ap.error('requires J700 / Mac17,5')
    info = run(args.client, 'info')
    if 'abi_version=7\n' not in info:
        ap.error('requires DevMem ABI 7')
    args.output.mkdir(parents=True, exist_ok=False)
    boot = run('sysctl', '-n', 'kern.bootsessionuuid').strip()
    metadata = dict(boot_uuid=boot, wall_time=time.time(), table_sha256=TABLE_SHA256,
                    tool_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                    client_sha256=hashlib.sha256(Path(args.client).read_bytes()).hexdigest(),
                    sw_vers=run('sw_vers'), devmem_info=info,
                    pmset=run('pmset', '-g'), count=len(table))
    (args.output / 'metadata.json').write_text(json.dumps(metadata, indent=2) + '\n')
    for label, cmd in [('battery-before', ['ioreg', '-a', '-r', '-c', 'AppleSmartBattery']),
                       ('power-before', ['ioreg', '-a', '-l', '-r', '-n', 'IOPMrootDomain'])]:
        (args.output / (label + '.plist')).write_text(run(*cmd))
    rows = []
    with (args.output / 'progress.jsonl').open('x') as progress:
        for domain in table:
            address = int(domain['base'], 0) + domain['offset']
            assert int(domain['base'], 0) in (0x300700000, 0x308280000)
            assert domain['offset'] % 4 == 0 and domain['offset'] <= 0x10000
            progress.write(json.dumps(dict(about_to_read=domain['name'], address=hex(address))) + '\n')
            progress.flush()
            os.fsync(progress.fileno())
            begin = time.monotonic_ns()
            output = run(args.client, 'mmio32', hex(address))
            match = re.fullmatch(r'([0-9a-fA-F]+): ((?:[0-9a-fA-F]{2} ){3}[0-9a-fA-F]{2})\s*', output)
            if not match or int(match[1], 16) != address:
                raise RuntimeError('unexpected MMIO response: ' + repr(output))
            value = int.from_bytes(bytes.fromhex(match[2]), 'little')
            row = dict(domain, address=hex(address), value=value, raw=output,
                       begin_ns=begin, end_ns=time.monotonic_ns(), decoded=decode(value))
            rows.append(row)
            progress.write(json.dumps(row) + '\n')
            progress.flush()
    if run('sysctl', '-n', 'kern.bootsessionuuid').strip() != boot:
        raise RuntimeError('boot identity changed')
    (args.output / 'snapshot.json').write_text(json.dumps(rows, indent=2) + '\n')
    (args.output / 'battery-after.plist').write_text(run('ioreg', '-a', '-r', '-c', 'AppleSmartBattery'))
    print(json.dumps(dict(output=str(args.output), count=len(rows), boot_uuid=boot,
                         elapsed_s=(rows[-1]['end_ns'] - rows[0]['begin_ns']) / 1e9)))


if __name__ == '__main__':
    main()
