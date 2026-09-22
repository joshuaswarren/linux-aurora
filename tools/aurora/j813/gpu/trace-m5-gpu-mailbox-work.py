# SPDX-License-Identifier: MIT
"""Record workload mailbox messages on the existing, paused M5 trace owner."""
import json
import os
from pathlib import Path
import time
import types

if (hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3' or
        int(hv.u.adt['/chosen'].chip_id) != 0x8142):
    raise RuntimeError('Pause the existing M5 guest first')
_m5_work_stream = (Path(os.environ['M5_GPU_TRACE_DIR']) / 'work-mailbox-events.jsonl').open('x', buffering=1)
_m5_work_count = 0


def _m5_work_message(self, direction, low, high):
    global _m5_work_count
    if int(high.EP) > 0x25 or _m5_work_count >= 20000:
        return True
    _m5_work_count += 1
    _m5_work_stream.write(json.dumps(dict(sequence=_m5_work_count,
        monotonic_ns=time.monotonic_ns(), role=self.ident,
        direction='tx' if int(direction) else 'rx', endpoint=int(high.EP),
        word0=hex(int(low.value)), word1=hex(int(high.value))), sort_keys=True) + '\n')
    return True


for _m5_work_role in hv.m5_gpu_roles:
    _m5_work_role.handle_msg = types.MethodType(_m5_work_message, _m5_work_role)
print('M5_WORK_MAILBOX_TRACE_ARMED: existing ASC observers, independent bounded output')
