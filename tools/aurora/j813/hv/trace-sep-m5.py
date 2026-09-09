"""Passive, bounded M5 SEP mailbox journal; no DART or shared-memory access."""
import json
import os
from pathlib import Path
import time

from m1n1.hw.sep import SEPMessage
from m1n1.trace import TraceMode
from m1n1.trace.asc import BaseASCTracer, DIR


def decode_message(raw, received, names):
    msg = SEPMessage(raw)
    if received and msg.EP == 0xfd and msg.TYPE == 0:
        word = int(msg.DATA).to_bytes(4, 'big')
        if all(32 <= byte < 127 or byte == 0 for byte in word):
            name = word.decode('ascii').strip('\0 ')
            if name:
                names[int(msg.PARAM)] = name
    label = {0xff: 'bootrom', 0xfe: 'shmem', 0xfd: 'discovery'}.get(
        int(msg.EP), names.get(int(msg.EP), 'unknown'))
    entry = dict(direction='rx' if received else 'tx', ep=int(msg.EP),
                 name=label, tag=int(msg.TAG), type=int(msg.TYPE),
                 param=int(msg.PARAM), data=int(msg.DATA), raw=int(raw))
    if not received and msg.EP == 0xfe and msg.TAG == 0:
        # Exact 25G76 AppleSEPBooter::bootSEP packs a page count into
        # bits16..31. Here TYPE/PARAM are raw bytes, not an endpoint opcode.
        entry.update(kind='shared_memory_setup',
                     size=((int(raw) >> 16) & 0xffff) << 12,
                     slave_addr=int(msg.DATA) << 12)
    return entry


class M5SEPJournal(BaseASCTracer):
    # No payload snapshots: the guest can execute mailbox operations directly.
    DEFAULT_MODE = TraceMode.ASYNC
    LIMIT = 8192

    def init_state(self):
        super().init_state()
        self.state.m5_names = {}
        self.state.m5_count = 0

    def handle_msg(self, direction, r0, r1):
        if self.state.m5_count >= self.LIMIT:
            return True
        entry = decode_message(r0.value, direction == DIR.RX, self.state.m5_names)
        self.state.m5_count += 1
        entry.update(seq=self.state.m5_count, host_ns=time.monotonic_ns())
        self.output.write(json.dumps(entry) + '\n')
        return True


if 'hv' in globals():
    if (int(hv.u.adt['/chosen'].chip_id) != 0x8142 or
            not getattr(hv, 'j813_kernelcache_verified', False)):
        raise RuntimeError('SEP journal requires the exact M5 guest')
    node = hv.adt['/arm-io/sep']
    if (list(node.compatible) != ['iop-sep,ascwrap-v6'] or
            tuple(node.get_reg(0)) != (0x30a600000, 0x88000)):
        raise RuntimeError('M5 SEP mailbox geometry changed')
    if hasattr(hv, 'm5_sep_journal'):
        raise RuntimeError('M5 SEP journal already installed')
    directory = Path(__file__).resolve().parent / 'logs/sep-traces'
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(directory, 0o700)
    path = directory / f'sep-{time.time_ns()}.jsonl'
    tracer = M5SEPJournal(hv, '/arm-io/sep', verbose=0)
    tracer.output = open(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                                0o600), 'w', buffering=1)
    tracer.start()
    hv.m5_sep_journal = tracer
    print('M5_SEP_MAILBOX_JOURNAL_ARMED', str(path))
