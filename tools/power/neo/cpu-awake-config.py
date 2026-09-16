#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Read a fixed J700 CPU/AIC comparison set on Linux or macOS.

Uses pmgr-ps read on Linux or the existing DevMem ABI 7 mmio32 client on
macOS. No writes or power-state transitions. Subprocesses perturb CPU idle;
samples are sequential observations, not hardware residency measurements.
"""
import argparse
import collections
import hashlib
import json
import platform
from pathlib import Path
import re
import subprocess
import time


CORES = dict(zip(('E0', 'E1', 'E2', 'E3', 'P0', 'P1'),
                 (0, 8, 16, 24, 48, 56)))
# AIC version/config and IRQ_CFG are read-only here. Never read IACK, which
# consumes interrupts. SMC RX=584 and NVMe=1039 come from the J700 live DT.
CONFIG = {
    'aic_version': 0x301000000,
    'aic_config': 0x301000014,
    'aic_smc_rx_config': 0x301010000 + 584 * 4,
    'aic_nvme_config': 0x301010000 + 1039 * 4,
    # Native T8140 enableCPMPowerDomainGating uses bit31 of these words.
    'cpm_gate_60': 0x300724060,
    'cpm_gate_68': 0x300724068,
}


def run(*args):
    return subprocess.check_output(args, text=True, timeout=15)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--client', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--rounds', type=int, default=64)
    a = p.parse_args()
    if not 1 <= a.rounds <= 2000:
        p.error('rounds must be 1..2000')
    client = str(a.client.resolve(strict=True))
    system = platform.system()
    if system == 'Darwin':
        if run('sysctl', '-n', 'hw.model').strip() != 'Mac17,5':
            p.error('requires Mac17,5')
        info = run(client, 'info')
        if 'abi_version=7\n' not in info:
            p.error('requires DevMem ABI 7')
        identity = run('sw_vers') + run('sysctl', '-n', 'kern.bootsessionuuid')
    elif system == 'Linux':
        if not Path('/sys/firmware/devicetree/base/compatible').read_bytes().startswith(b'apple,j700\0'):
            p.error('requires apple,j700')
        info = 'pmgr-ps read'
        identity = Path('/proc/version').read_text() + Path('/proc/sys/kernel/random/boot_id').read_text()
    else:
        p.error('requires Linux or macOS')
    a.output.mkdir(parents=True, exist_ok=False)
    metadata = dict(system=system, identity=identity, client_info=info,
                    rounds=a.rounds, client_sha256=hashlib.sha256(Path(client).read_bytes()).hexdigest(),
                    tool_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                    wall_time=time.time(), status='running')
    (a.output / 'metadata.json').write_text(json.dumps(metadata, indent=2) + '\n')
    start = time.monotonic_ns()
    counts = {name: collections.Counter() for name in CORES}
    with (a.output / 'reads.jsonl').open('x') as log:
        def read(name, address, round_id):
            log.write(json.dumps(dict(about_to_read=name, address=hex(address), round=round_id)) + '\n')
            log.flush()
            if system == 'Darwin':
                raw = run(client, 'mmio32', hex(address))
                m = re.fullmatch(r'([0-9a-fA-F]+): ((?:[0-9a-fA-F]{2} ){3}[0-9a-fA-F]{2})\s*', raw)
                if not m or int(m[1], 16) != address:
                    raise RuntimeError('unexpected MMIO response: ' + repr(raw))
                value = int.from_bytes(bytes.fromhex(m[2]), 'little')
            else:
                raw = run(client, 'read', hex(address & ~0x3fff), hex(address & 0x3fff))
                if not re.fullmatch(r'[0-9a-fA-F]{8}\s*', raw):
                    raise RuntimeError('unexpected PMGR response: ' + repr(raw))
                value = int(raw, 16)
            log.write(json.dumps(dict(name=name, address=hex(address), round=round_id,
                                      value=value, elapsed_ns=time.monotonic_ns()-start)) + '\n')
            log.flush()
            return value

        before = {k: read(k, v, 'before') for k, v in CONFIG.items()}
        if before['aic_version'] != 0xa:
            raise RuntimeError('unexpected J700 AIC version')
        for i in range(a.rounds):
            for name, offset in CORES.items():
                value = read(name, 0x300700000 + offset, i)
                counts[name][(value >> 4) & 15] += 1
            time.sleep(.017 + (i * 7 % 13) / 1000)
        after = {k: read(k, v, 'after') for k, v in CONFIG.items()}
    result = dict(before=before, after=after, actual_state_counts=counts,
                  elapsed_seconds=(time.monotonic_ns()-start)/1e9)
    (a.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    metadata['status'] = 'complete'
    (a.output / 'metadata.json').write_text(json.dumps(metadata, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
