"""Passive, bounded M5 PMP startup journal, without DMA or extra MMIO reads.

Observe only accesses the guest performs. ASYNC lets the target forward MMIO
without waiting for a host callback. Raw messages are retained for decoding
after capture; this does not run the older-chip PMP/DART tracer.
"""
from collections import Counter, deque
import json
import os
from pathlib import Path
import struct
import time

from m1n1.trace import TraceMode
from m1n1.utils import irange


class M5PMPJournal:
    IDENT = 'M5 PMP startup journal'
    LIMIT = 100000

    def __init__(self, hv):
        if int(hv.adt['/chosen'].chip_id) != 0x8142:
            raise RuntimeError('PMP journal requires M5')
        if not getattr(hv, 'j813_kernelcache_verified', False):
            raise RuntimeError('PMP journal requires the verified M5 kernel')
        if os.environ.get('M5_PMGR_TRACE') == '1':
            raise RuntimeError('Disable broad synchronous PMGR tracing for this capture')
        node = hv.adt['/arm-io/pmp']
        asc, asc_size = node.get_reg(0)
        update_index = int(node.ptd_update_reg_index)
        update, update_size = node.get_reg(update_index)
        if (asc, asc_size, update_index, update, update_size) != (
                0x380e00000, 0x88000, 3, 0x3803d0000, 0x4000):
            raise RuntimeError('Unreviewed M5 PMP register topology')
        if sorted(int(n) for n in node.interrupts) != [1000, 1001, 1002, 1003]:
            raise RuntimeError('Unreviewed M5 PMP interrupt topology')
        if list(hv.adt['/arm-io/dart-pmp'].interrupts) != [1004]:
            raise RuntimeError('Unreviewed M5 PMP DART interrupt topology')
        raw = hv.adt['/arm-io/pmp/iop-pmp-nub'].ptd_range
        if len(raw) % 32:
            raise RuntimeError('Malformed PMP PTD map')
        ptd = {name.rstrip(b'\0').decode('ascii'): offset
               for _, offset, _, name in struct.iter_unpack('<II8s16s', raw)}
        if ptd.get('PMP-STATUS') != 1:
            raise RuntimeError('Unreviewed PMP status PTD offset')
        pmgr = hv.adt['/arm-io/pmgr']
        # M5 guest25 confirmed native ApplePTD::_readPTD reads this window.
        # The address formula also matches tools/pmp_assist.py. Do not
        # synthesize READY or poll any hardware ourselves.
        windows = {pmgr.get_reg(i) for i in range(len(pmgr.reg))}
        if (0x3803c0000, 0x20000) not in windows:
            raise RuntimeError('M5 PTD window absent')
        self.count = 0
        self.counts = Counter()
        self.tail = deque(maxlen=64)
        self.names = {asc + off: name for off, name in (
            (0x44, 'CPU_CONTROL'), (0x48, 'CPU_STATUS'),
            (0x8110, 'INBOX_CTRL'), (0x8114, 'OUTBOX_CTRL'),
            (0x8800, 'INBOX0'), (0x8808, 'INBOX1'),
            (0x8830, 'OUTBOX0'), (0x8838, 'OUTBOX1'))}
        self.status = update - 0x10000 + ptd['PMP-STATUS'] * 16
        directory = Path(__file__).resolve().parent / 'logs/pmp-traces'
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.path = directory / f'm5-{time.time_ns()}.jsonl'
        fd = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        self.output = os.fdopen(fd, 'w', buffering=1)
        self.output.write(json.dumps(dict(kind='profile', chip='0x8142',
            mode='ASYNC', asc=hex(asc), update=hex(update),
            status=hex(self.status), max_events=self.LIMIT)) + '\n')
        for off, size in ((0x44, 8), (0x8110, 8), (0x8800, 16), (0x8830, 16)):
            hv.add_tracer(irange(asc + off, size), self.IDENT, TraceMode.ASYNC,
                          read=self.event, write=self.event)
        hv.add_tracer(irange(update, update_size), self.IDENT, TraceMode.ASYNC,
                      read=self.event, write=self.event)
        hv.add_tracer(irange(self.status, 16), self.IDENT, TraceMode.ASYNC,
                      read=self.event, write=self.event)
        # The existing AIC3-aware resident hook forwards each guest access,
        # recording only these IRQ acknowledgements into its bounded RAM ring.
        # No interrupt is consumed by the host or injected into the guest.
        hv.trace_irq('M5 PMP and DART', 1000, 5, hv.IRQTRACE_RESIDENT)
        print('M5_PMP_JOURNAL_ARMED', str(self.path))

    def event(self, evt):
        self.count += 1
        address = int(evt.addr)
        write = bool(evt.flags.WRITE)
        self.counts[(address, write)] += 1
        record = dict(seq=self.count, addr=hex(address), pc=hex(int(evt.pc)),
                      width=1 << int(evt.flags.WIDTH), write=write,
                      value=hex(int(evt.data)), name=self.names.get(address,
                      'PMP_STATUS' if self.status <= address < self.status + 16
                      else 'PTD_UPDATE'))
        self.tail.append(record)
        if self.count <= self.LIMIT:
            self.output.write(json.dumps(record, separators=(',', ':')) + '\n')
        elif self.count == self.LIMIT + 1:
            self.output.write('{"kind":"event-limit-reached"}\n')

    def dump(self):
        print('M5_PMP_JOURNAL_COUNT', self.count, 'addresses', len(self.counts))
        for record in self.tail:
            print('M5_PMP_RECENT', record)


if 'hv' in globals():
    hv.m5_pmp_journal = M5PMPJournal(hv)
