"""Pause before the first native alpha TX doorbell write in this controller.

WSYNC invokes the observer before handle_vm_hook_mapped forwards the original
write. This observer must never forward the write itself. Use cont() from the
capture shell to return to that existing forwarding path exactly once.
"""
import json
import os
from pathlib import Path
import time

from m1n1.hv import EXC_RET
from m1n1.hv.types import TraceMode


class N1TxStop:
    def __init__(self, hv, read_context, stream=2):
        if (hv.ctx is None or int(hv.u.adt['/chosen'].chip_id) != 0x8142
                or not getattr(hv, 'j813_kernelcache_verified', False)):
            raise RuntimeError('Requires paused exact M5 guest')
        # Exact T8142 port configuration layout, qualified against 25G76's
        # AppleT8142PCIe::start and _updateRIDToSIDMappings. Read existing
        # hardware association; never create a DART mapping or change SID.
        port, span = hv.u.adt['/arm-io/apcie'].get_reg(8)
        if span != 0x8000 or not 0 < stream < 19:
            raise RuntimeError('Unexpected T8142 port/SID geometry')
        association = hv.p.read32(port + 0x3000 + 4 * stream)
        if association != 0x80000000 | stream << 16 | 0x0101:
            raise RuntimeError('Native alpha RID 0101 is not mapped to this SID')
        bar, size = hv.n1_journal.attach_enumerated(1, 0x1902, 0x10000)
        if size != 0x10000 or hv.p.read32(bar + 0xc) != 2:
            raise RuntimeError('Native alpha IPC is not ready')
        self.hv, self.read_context, self.stream = hv, read_context, stream
        self.address, self.hit, self.path = bar + 0x1030, False, None
        self.zone = range(self.address, self.address + 4)
        self.ident = 'M5-N1-first-native-TX'
        self.association = association
        hv.add_tracer(self.zone, self.ident, TraceMode.WSYNC, write=self.write)
        print('M5_N1_TX_STOP_ARMED', hex(self.address), 'SID', stream)

    def write(self, evt):
        if self.hit:
            return
        if (int(evt.addr) != self.address or int(evt.flags.WIDTH) != 2
                or not evt.flags.WRITE or evt.flags.MULTI):
            raise RuntimeError('Unexpected native alpha TX doorbell access')
        self.hit = True
        hv = self.hv
        record = dict(time_ns=time.time_ns(), address=int(evt.addr),
                      value=int(evt.data), pc=int(evt.pc),
                      flags=int(evt.flags), stream=self.stream,
                      rid_to_sid=self.association, original_write_pending=True)
        record['registers'] = [int(value) for value in hv.ctx.regs]
        for field in ('elr', 'spsr', 'esr', 'far', 'cpu_id'):
            record[field] = int(getattr(hv.ctx, field))
        record['sp'] = [int(value) for value in hv.ctx.sp]
        try:
            record['context_capture'] = str(self.read_context(
                hv, 1, self.stream, message_payloads=True))
        except Exception as error:
            record['context_error'] = f'{type(error).__name__}: {error}'
        directory = Path(__file__).resolve().parent / 'logs/n1-dma'
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(directory, 0o700)
        self.path = directory / f'tx-stop-{record["time_ns"]}.json'
        with open(os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                          0o600), 'w') as output:
            json.dump(record, output, indent=2)
            output.write('\n')
        print('M5_N1_TX_STOP_SAVED_PRIVATE', self.path)
        result = hv.run_shell('Entering N1 TX capture shell',
                              'Returning to native TX doorbell')
        if result != EXC_RET.HANDLED:
            raise RuntimeError('TX observer requires cont() to finish the pending write')

    def remove(self):
        self.hv.del_tracer(self.zone, self.ident)
        print('M5_N1_TX_STOP_REMOVED')
