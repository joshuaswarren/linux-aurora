# SPDX-License-Identifier: MIT
"""Bounded M5 GPU mailbox/power observation, loaded before macOS starts.

No Neo init-data decoder, no speculative device accesses and no nested proxy
requests from asynchronous callbacks. Raw observations establish the M5 ABI.
"""
import json
import os
from pathlib import Path
import time

from m1n1.hv.types import TraceMode
from m1n1.trace.asc import BaseASCTracer
from m1n1.utils import irange

if int(hv.u.adt['/chosen'].chip_id) != 0x8142 or hv.u.adt.model != 'Mac17,3':
    raise RuntimeError('M5 GPU boot tracer requires J813/T8142')
if hv.started:
    raise RuntimeError('GPU startup trace must be installed before guest entry')

output = Path(os.environ['M5_GPU_TRACE_DIR'])
stream = (output / 'gpu-events.jsonl').open('x', buffering=1)
limit = 100000
sequence = 0
failed = False


def emit(kind, **fields):
    global sequence, failed
    if failed or sequence >= limit:
        return
    try:
        sequence += 1
        stream.write(json.dumps(dict(sequence=sequence, monotonic_ns=time.monotonic_ns(),
                                     kind=kind, **fields), sort_keys=True) + '\n')
        if sequence == limit:
            stream.write(json.dumps(dict(kind='TRACE_LIMIT', limit=limit)) + '\n')
    except Exception:
        failed = True


class M5GpuRoleTracer(BaseASCTracer):
    DEFAULT_MODE = TraceMode.ASYNC

    def log(self, message, show_cpu=True):
        emit('ASC_LOG', role=self.ident, message=message)

    def handle_msg(self, direction, low, high):
        emit('MAILBOX', role=self.ident, direction='tx' if int(direction) else 'rx',
             endpoint=int(high.EP), word0=hex(int(low.value)), word1=hex(int(high.value)))
        return True

    def evt_rw(self, event, regmap=None, prefix=None):
        try:
            emit('ASC_MMIO', role=self.ident, cpu=int(event.flags.CPU), pc=hex(event.pc),
                 write=bool(event.flags.WRITE), width=1 << int(event.flags.WIDTH),
                 offset=hex(int(event.addr) - int(self.dev.get_reg(0)[0])),
                 value=hex(event.data))
            return super().evt_rw(event, regmap=regmap, prefix=prefix)
        except Exception as error:
            emit('TRACE_ERROR', role=self.ident, error=repr(error))


roles = []
for name, base in [('gfx-asc', 0x582600000), ('gfx1-asc', 0x582e00000)]:
    path = '/arm-io/' + name
    if hv.adt[path].get_reg(0) != (base, 0x88000):
        raise RuntimeError('M5 ASC wrapper geometry changed')
    role = M5GpuRoleTracer(hv, path, verbose=0)
    role.start()
    roles.append(role)
    emit('TRACE_ARMED', role=name, base=hex(base))


def power_access(event):
    emit('GPU_POWER', address=hex(event.addr), write=bool(event.flags.WRITE),
         width=1 << int(event.flags.WIDTH), value=hex(event.data),
         cpu=int(event.flags.CPU), pc=hex(event.pc))


for name, base in [('GPX', 0x580e80000), ('GFX', 0x580e80110)]:
    hv.add_tracer(irange(base, 4), 'M5_GPU_' + name, TraceMode.ASYNC,
                  read=power_access, write=power_access)
    emit('POWER_TRACE_ARMED', domain=name, base=hex(base))

hv.m5_gpu_roles = roles
print('M5_GPU_TRACE_ARMED: both ASC roles and GPX/GFX; raw events, no firmware ABI override')
