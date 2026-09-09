"""Journal guest-requested M5 PMGR MMIO to diagnose the guest15 SError.

Uses the native tracer; it neither synthesizes register values nor adds
device accesses. Existing reserved UART/CPU hooks retain priority.
"""
from collections import deque
import json
import os
from pathlib import Path
import time

from m1n1.trace import TraceMode
from m1n1.utils import irange


class M5PMGRJournal:
    IDENT = 'M5 PMGR journal'

    def __init__(self, hv):
        if int(hv.adt['/chosen'].chip_id) != 0x8142:
            raise RuntimeError('PMGR journal requires M5')
        self.hv = hv
        self.break_acc = os.environ.get('M5_PMGR_BREAK_ACC') == '1'
        self.tail = deque(maxlen=64)
        self.count = 0
        directory = Path(__file__).resolve().parent / 'logs/pmgr-traces'
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.path = directory / f'm5-{time.time_ns()}.jsonl'
        fd = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        self.output = os.fdopen(fd, 'w', buffering=1)
        node = hv.adt['/arm-io/pmgr']
        intervals = sorted((addr, addr + size) for addr, size in (
            node.get_reg(i) for i in range(len(node.reg))))
        merged = []
        for start, end in intervals:
            if end <= start or start < 0x200000000 or end > 0x600000000:
                raise RuntimeError('Unexpected M5 PMGR register range')
            if merged and start <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(merged[-1][1], end))
            else:
                merged.append((start, end))
        for start, end in merged:
            hv.add_tracer(irange(start, end - start), self.IDENT,
                          TraceMode.SYNC, read=self.event, write=self.event)
        # PMGR's register inventory includes MCC sampling registers. The
        # native MCC driver repeatedly reads these counters; synchronizing
        # every sample over KIS materially delays its initialization loop.
        # Keep MCC writes, while ACC and the other PMGR ranges stay synchronous.
        hv.add_tracer(irange(0x3803c0000, 0x20000), self.IDENT,
                      TraceMode.WSYNC, read=None, write=self.event)
        print('M5_PMGR_JOURNAL_ARMED', len(merged), str(self.path))

    def event(self, evt):
        self.count += 1
        record = dict(seq=self.count, addr=hex(int(evt.addr)),
                      pc=hex(int(evt.pc)), width=1 << int(evt.flags.WIDTH),
                      write=bool(evt.flags.WRITE), value=hex(int(evt.data)))
        self.tail.append(record)
        self.output.write(json.dumps(record, separators=(',', ':')) + '\n')
        # SYNC write callbacks run before the proxy forwards the store.
        # Stop in shellwrap's retrying debug shell while the write is still
        # pending. Neither a fabricated result nor the known-failing MMIO
        # access has occurred. Do not use skip/cont to bypass this breakpoint.
        if self.break_acc and record['write'] and int(evt.addr) == 0x211e48000:
            self.output.write(json.dumps(dict(
                kind='pending-acc-write', seq=self.count,
                registers=[hex(int(v)) for v in self.hv.ctx.regs])) + '\n')
            raise RuntimeError('M5 ACC write pending at 0x211e48000; inspect before recovery')

    def dump(self):
        print('M5_PMGR_JOURNAL_COUNT', self.count)
        for record in self.tail:
            print('M5_PMGR_RECENT', record)


if 'hv' in globals():
    hv.m5_pmgr_journal = M5PMGRJournal(hv)
