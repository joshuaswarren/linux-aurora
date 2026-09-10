# SPDX-License-Identifier: MIT
"""Observe the verified M5 addKicks FIFO during the Metal reference workload.

Load once from the deliberately paused, existing M5 hypervisor shell.
This installs write observers only; it publishes no GPU commands.
"""
import json
import os
from pathlib import Path
import time

from m1n1.hv.types import TraceMode
from m1n1.utils import irange

if (hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3' or
        int(hv.u.adt['/chosen'].chip_id) != 0x8142):
    raise RuntimeError('Pause the existing J813 guest before adding FIFO observers')
if hv.u.adt['/arm-io/sgx'].get_reg(0) != (0x580000000, 0x4000000):
    raise RuntimeError('M5 accelerator register geometry changed')

# AGXG17G 353.14 configureDevice copies the u32 pair at native VA
# 0xfffffe0007139948 into accelerator +0xee58/+0xee5c, and writes stride 0x10
# to +0xee60. addKicks uses fixed FIFO slot 9, then stores its tag before count.
_m5_sksm_base = 0x580000000
_m5_sksm_fifo = _m5_sksm_base + 0x4000 + 9 * 0x10
_m5_sksm_stream = (Path(os.environ['M5_GPU_TRACE_DIR']) / 'sksm-events.jsonl').open('x', buffering=1)
_m5_sksm_sequence = 0


def _m5_sksm_write(event):
    global _m5_sksm_sequence
    if _m5_sksm_sequence >= 20000:
        return
    _m5_sksm_sequence += 1
    _m5_sksm_stream.write(json.dumps(dict(
        sequence=_m5_sksm_sequence, monotonic_ns=time.monotonic_ns(),
        address=hex(event.addr), value=hex(event.data),
        width=1 << int(event.flags.WIDTH), cpu=int(event.flags.CPU), pc=hex(event.pc)
    ), sort_keys=True) + '\n')


hv.add_tracer(irange(_m5_sksm_fifo, 16), 'M5_SKSM_ADDKICKS_25G76',
              TraceMode.ASYNC, write=_m5_sksm_write)
print('M5_SKSM_ADDKICKS_ARMED: verified FIFO slot 9, ordered tag/count observers')
