#!/usr/bin/env python3
"""Read an extracted AVE2 assembly function with logging-only paths omitted.

This is an analysis view, not an emulator or a complete decompiler. It assumes
AVE_Log_CheckLevel returns zero; all other conditional edges remain visible.
"""
import argparse
from pathlib import Path
import re


def functions(text):
    return [part for part in re.split(r'\n(?=0x[0-9a-f]+ )', text) if part.strip()]


def flow(part):
    ins = {int(m[1], 16): (m[2], m[3], line) for line in part.splitlines()
           if (m := re.match(r'([0-9a-f]+):\s+(\S+)\s*(.*)', line))}
    pending, seen = [min(ins)], set()
    while pending:
        address = pending.pop()
        if address in seen or address not in ins:
            continue
        seen.add(address)
        op, args, line = ins[address]
        target = re.search(r'#0x([0-9a-f]+)', args)
        if ('AVE_Log_CheckLevel' in line and address + 4 in ins and
                ins[address + 4][0] == 'cbz'):
            pending.append(int(re.search(r'#0x([0-9a-f]+)', ins[address + 4][1])[1], 16))
            continue
        if op.startswith('ret'):
            continue
        if op == 'b':
            pending.append(int(target[1], 16))
            continue
        if (op.startswith('b.') or op in ('cbz', 'cbnz', 'tbz', 'tbnz')) and target:
            pending.append(int(target[1], 16))
        pending.append(address + 4)
    return [(address, ins[address][2]) for address in sorted(seen)]


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('file', type=Path)
    p.add_argument('pattern')
    p.add_argument('--low', type=lambda v: int(v, 0), default=0)
    p.add_argument('--high', type=lambda v: int(v, 0), default=1 << 64)
    a = p.parse_args()
    for part in functions(a.file.read_text()):
        if re.search(a.pattern, part.splitlines()[0]):
            print(part.splitlines()[0])
            print('\n'.join(line for address, line in flow(part) if a.low <= address < a.high))
