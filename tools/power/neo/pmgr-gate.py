#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Gate or restore unmanaged J700 PMGR power domains from userspace.

Uses the static `pmgr-ps` helper (aligned 32-bit /dev/mem accesses with durable
checkpoints) and `j700-pmgr-domains.json`, decoded from the native ADT device
table. Domains that Linux manages through genpd, the memory/fabric/always-on
infrastructure, CPUs and the DCP-owned internal display pipeline are refused.
A returned write is not proof that a block was idle or that power dropped:
measure separately and verify device function afterwards.

  pmgr-gate.py status [NAME...]
  pmgr-gate.py gate  --log LOG --state STATE NAME...    (children first)
  pmgr-gate.py ungate --log LOG --state STATE [NAME...] (reverse order)
"""
import argparse
import fcntl
import json
import os
import re
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
TABLE = HERE / 'j700-pmgr-domains.json'
HELPER = HERE / 'pmgr-ps'

# Never gated from here: infrastructure, wake path, memory, CPU, Linux-managed.
DENY_PREFIXES = ('ECPU', 'PCPU', 'ECPM', 'PCPM', 'SEP', 'SBR', 'AIC', 'PMS', 'PMC', 'SOC_',
                 'PMGR_', 'AFI', 'AFG', 'AFC', 'AMCC', 'DCS', 'PIOGW', 'AP_TMM', 'PMP',
                 'NUB_', 'SMC_', 'MTP_', 'DEBUG_', 'DISP_', 'DP', 'GPIO', 'MSG',
                 'ANS', 'APCIE', 'ATC0', 'GFX', 'I2C2', 'CPU-BUSY')
ALLOW_DESPITE_PREFIX = {'DPTX_PHY', 'DPA_P', 'DPA0', 'DPA1', 'DPRX', 'DPEXT0', 'DISPEXT0_SYS',
                        'DISPEXT0_FE', 'DISPEXT0_DBE', 'DISPEXT0_GPPP'}


def load_table():
    table = {d['name']: d for d in json.loads(TABLE.read_text())}
    children = {}
    for d in table.values():
        for p in d['parents']:
            children.setdefault(p, []).append(d['name'])
    return table, children


def helper(*args):
    p = subprocess.run([str(HELPER), *args], capture_output=True, text=True)
    if args[0] != 'read':
        sys.stdout.write(p.stdout)
    if p.returncode:
        raise RuntimeError(p.stderr or p.stdout or 'pmgr-ps failed')
    return p.returncode, p.stdout


def read(d):
    _, out = helper('read', d['base'], hex(d['offset']))
    return int(out.strip(), 16)


def decode(v):
    return 'tgt=%x act=%x busy=%d auto=%d flags=%02x' % (v & 0xf, (v >> 4) & 0xf, (v >> 11) & 1,
                                                       (v >> 28) & 1, (v >> 8) & 0xff)


def denied(name):
    if name.endswith('_CPU'):
        return True
    if name in ALLOW_DESPITE_PREFIX:
        return False
    return any(name.startswith(p) for p in DENY_PREFIXES)


def writable_policy(v):
    # ACTUAL, history and BUSY are hardware status, not restorable policy.
    return v & ~((0xf << 4) | (3 << 8) | (1 << 11))


def restore(d, entry, log):
    before = entry['before']
    helper('set', d['base'], hex(d['offset']), '0xf', str(log))
    active = read(d)
    if (active >> 4) & 0xf != 0xf or active & (1 << 11):
        raise RuntimeError('ACTIVE restoration not confirmed')
    if before & (1 << 28):
        helper('write', d['base'], hex(d['offset']),
               hex(before & ~((1 << 11) | (3 << 8))), str(log))
    after = read(d)
    if writable_policy(after) != writable_policy(before):
        raise RuntimeError('restored policy mismatch: %#x != %#x' % (after, before))
    if not before & (1 << 28) and (after >> 4) & 0xf != 0xf:
        raise RuntimeError('restored non-AUTO domain is not ACTIVE')
    return after


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('command', choices=('status', 'gate', 'ungate'))
    ap.add_argument('names', nargs='*')
    ap.add_argument('--log', type=Path)
    ap.add_argument('--state', type=Path)
    ap.add_argument('--target', default='0', help='PS target for gate: 0 (pwrgate) or 4 (clkgate)')
    args = ap.parse_args()
    if os.geteuid():
        ap.error('root is required')
    if b'apple,j700\0' not in Path('/proc/device-tree/compatible').read_bytes():
        ap.error('J700 only')
    lock = open('/run/neo-pmgr-gate.lock', 'w')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    table, children = load_table()
    unknown = [n for n in args.names if n not in table or table[n]['virtual']]
    if unknown:
        ap.error('unknown domains: %s' % ' '.join(unknown))
    if args.command == 'status':
        names = args.names or sorted((n for n in table if not table[n]['virtual']),
                                    key=lambda n: (table[n]['base'], table[n]['offset']))
        for n in names:
            d = table[n]
            v = read(d)
            print('%-18s %s+%#07x %#010x %s%s' % (n, d['base'], d['offset'], v, decode(v),
                                                  '  [denied]' if denied(n) else ''))
        return 0
    if not args.log or not args.state:
        ap.error('--log and --state are required')
    state = json.loads(args.state.read_text()) if args.state.exists() else {}

    def save_state():
        tmp = args.state.with_suffix('.tmp')
        tmp.write_text(json.dumps(state, indent=1) + '\n')
        with tmp.open('rb') as f:
            os.fsync(f.fileno())
        tmp.replace(args.state)
        fd = os.open(args.state.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)

    def powered_children(name):
        powered = []
        for child in children.get(name, []):
            if table[child]['virtual']:
                powered.extend(powered_children(child))
            elif (read(table[child]) >> 4) & 0xf:
                powered.append(child)
        return powered

    if args.command == 'gate':
        summary = Path('/sys/kernel/debug/pm_genpd/pm_genpd_summary').read_text()
        managed = {m.group(1).upper() for m in
                   re.finditer(r'^(\S+)\s+(?:on|off)\b', summary, re.MULTILINE)}
        bad = [n for n in args.names if denied(n) or n.upper() in managed]
        if bad:
            ap.error('refusing to gate infrastructure/managed domains: %s' % ' '.join(bad))
        target = int(args.target, 0)
        if target not in (0, 4):
            ap.error('target must be 0 or 4')
        for n in args.names:
            d = table[n]
            before = read(d)
            if before & ((1 << 31) | (1 << 12) | (1 << 11) | (1 << 10)):
                raise RuntimeError('%s has reset/busy/device-disable bits set; refusing write' % n)
            if ((before >> 4) & 0xf) != 0xf:
                print('%s already not ON (%#010x); skipping' % (n, before))
                continue
            powered = powered_children(n)
            if powered:
                raise RuntimeError('%s has powered children %s' % (n, powered))
            if n in state:
                print('%s already recorded in state (%#010x); skipping' % (n, state[n]['before']))
                continue
            state[n] = {'before': before, 'target': target, 'result': None, 'boot_id': boot}
            save_state()
            rc, _ = helper('set', d['base'], hex(d['offset']), str(target), str(args.log))
            after = read(d)
            if (after >> 4) & 0xf != target or after & (1 << 11):
                raise RuntimeError('%s target not reached: %#x' % (n, after))
            state[n].update(result=rc, after=after)
            save_state()
            print('%-18s %#010x -> %#010x  %s  rc=%d' % (n, before, after, decode(after), rc))
        return 0

    # ungate: restore recorded words, reverse order
    names = args.names or list(reversed(list(state)))
    rc_all = 0
    for n in names:
        if n not in state:
            print('%s not in state; skipping' % n)
            continue
        if state[n].get('restore_complete'):
            continue
        if state[n].get('boot_id') != boot:
            raise RuntimeError('refusing restoration from a different or unknown boot')
        try:
            after = restore(table[n], state[n], args.log)
            state[n].update(restored=after, restore_rc=0, restore_complete=True)
            print('%-18s restored -> %#010x  %s' % (n, after, decode(after)))
        except RuntimeError as error:
            state[n]['restore_error'] = str(error)
            save_state()
            raise  # Do not restore children after a failed parent restoration.
        save_state()
    return rc_all


if __name__ == '__main__':
    raise SystemExit(main())
