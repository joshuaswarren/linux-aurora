"""Read the resident PMP IRQ journal from RAM through a paused M5 controller."""
from collections import Counter
import os
from pathlib import Path
import re
import struct
import time
from tools.sptm.seed import _m1n1_symbol_offset


def read_pmp_irqs(hv):
    if hv.ctx is None or int(hv.adt['/chosen'].chip_id) != 0x8142:
        raise RuntimeError('PMP IRQ reader requires paused M5 guest')
    base = Path(__file__).resolve().parent
    source = (base / 'm1n1/src/hv_aic.c').read_text()
    body = source.split('struct hv_irqtrace_resident_event {', 1)[1].split('};', 1)[0]
    fields = re.findall(r'\bu64\s+(\w+);', body)
    if fields != ['sequence', 'arch_counter', 'apple_counter', 'cpu', 'elr',
                  'raw_event', 'global_num', 'reserved']:
        raise RuntimeError('Resident IRQ event layout changed')
    if '#define IRQTRACE_RESIDENT_COUNT 2048U' not in source:
        raise RuntimeError('Resident IRQ ring capacity changed')
    count = hv.p.read64(hv.u.base + _m1n1_symbol_offset('hv_irqtrace_resident_idx'))
    ring = hv.u.base + _m1n1_symbol_offset('hv_irqtrace_resident')
    raw = hv.iface.readmem(ring, 2048 * 64)
    events = []
    for seq in range(max(0, count - 2048), count):
        values = struct.unpack_from('<8Q', raw, (seq % 2048) * 64)
        if values[0] != seq:
            raise RuntimeError('Resident IRQ ring sequence mismatch')
        events.append(dict(zip(fields, values)))
    directory = base / 'logs/pmp-traces'
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    dest = directory / f'irq-m5-{time.time_ns()}.bin'
    with open(os.open(dest, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as out:
        out.write(struct.pack('<Q', count) + raw)
    print('M5_PMP_IRQ_TOTAL', count, 'retained', len(events),
          'by_irq', dict(Counter(e['global_num'] for e in events)))
    for event in events[-12:]:
        print('M5_PMP_IRQ', event)
    print('M5_PMP_IRQ_SAVED_PRIVATE', str(dest))


if 'hv' in globals():
    read_pmp_irqs(hv)
