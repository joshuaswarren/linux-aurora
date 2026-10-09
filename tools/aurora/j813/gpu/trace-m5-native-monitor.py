# SPDX-License-Identifier: MIT
"""Observe two native compute publications while a bounded job keeps M5 busy.

Run only in the existing paused M5 hypervisor, after the one-shot SKSM
capture has finished. The hardware breakpoint is removed after two matching
submissions, on error, or after 128 unmatched submissions. No guest code,
firmware code, selectors, or acknowledgements are modified.
"""
import json
import os
from pathlib import Path
import struct
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from m5_native_monitor import M5NativeMonitor

if (hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3'
        or int(hv.u.adt['/chosen'].chip_id) != 0x8142):
    raise RuntimeError('Pause the existing M5 owner before arming native monitors')
if getattr(hv, 'm5_native_monitor_armed', False):
    raise RuntimeError('Native M5 monitor hook is already armed')
_m5_monitor = M5NativeMonitor(hv) if getattr(hv, 'm5_native_monitor', None) is None else hv.m5_native_monitor
hv.m5_native_monitor = _m5_monitor
_m5_monitor_pc = 0xfffffe0008984fc0
if _m5_monitor_pc in hv._bps:
    raise RuntimeError('Native submit breakpoint already belongs to another capture')
_m5_monitor_dram_start = int(hv.u.adt['/chosen'].dram_base)
_m5_monitor_dram_end = _m5_monitor_dram_start + int(hv.u.adt['/chosen'].dram_size)
_m5_monitor_directory = Path(os.environ['M5_GPU_TRACE_DIR']) / ('native-monitor-' + str(time.time_ns()))
_m5_monitor_directory.mkdir(mode=0o700)
_m5_monitor_records = []
_m5_monitor_hits = 0


def _m5_monitor_read(address, size):
    if size <= 0 or size > 64:
        raise ValueError('Native monitor CPU read exceeds bound')
    output = bytearray()
    while size:
        amount = min(size, 4096 - (address & 4095))
        pa = int(hv.p.hv_translate(address, False, False))
        if not _m5_monitor_dram_start <= pa < pa + amount <= _m5_monitor_dram_end:
            raise ValueError('Native monitor CPU read is outside guest DRAM')
        data = hv.iface.readmem(pa, amount)
        if len(data) != amount:
            raise RuntimeError('Short native monitor CPU read')
        output.extend(data)
        address += amount
        size -= amount
    return bytes(output)


def _m5_monitor_check_code():
    if _m5_monitor_read(_m5_monitor_pc, 8) != bytes.fromhex('811642f9700240f9'):
        raise RuntimeError('Native monitor breakpoint does not match pinned 25G76 code')


def _m5_monitor_submit(ctx):
    global _m5_monitor_hits
    _m5_monitor_hits += 1
    record = dict(hit=_m5_monitor_hits, phase='host-tag14-before-channel-publication')
    try:
        _m5_monitor_check_code()
        tag, qid, count, priority, stamp, dm = struct.unpack('<IHHBQI', _m5_monitor_read(int(ctx.regs[2]), 21))
        record.update(tag=tag, queue_id=qid, count=count, priority=priority, stamp=hex(stamp), data_master=dm)
        if tag == 14 and dm == 2 and count and priority == 2:
            # The second publication may use another native hardware queue.
            # Observe the first publication's queue while its job executes.
            observed_queue = _m5_monitor_records[0]['queue_id'] if _m5_monitor_records else qid
            record['observed_queue_id'] = observed_queue
            record['mmio'] = _m5_monitor.capture(observed_queue)
            _m5_monitor_records.append(record)
            print('M5_NATIVE_MONITOR_OBSERVED', qid, hex(stamp),
                  'powered=' + str(record['mmio']['powered']),
                  'compute_valid=' + str(record['mmio']['compute_monitor_valid']))
        if len(_m5_monitor_records) >= 2 or _m5_monitor_hits >= 128:
            hv.remove_hw_bp(_m5_monitor_pc)
            hv.m5_native_monitor_armed = False
        else:
            # Execute the verified LDR effect once to avoid repeated traps
            # at the same publication on this carrier.
            ctx.regs[1], = struct.unpack('<Q', _m5_monitor_read(int(ctx.regs[20]) + 0x428, 8))
            ctx.elr += 4
    except Exception as error:
        hv.remove_hw_bp(_m5_monitor_pc)
        hv.m5_native_monitor_armed = False
        record['error'] = str(error)
        if not any(item is record for item in _m5_monitor_records):
            _m5_monitor_records.append(record)
        print('M5_NATIVE_MONITOR_FAILED', repr(error))
    (_m5_monitor_directory / 'observations.json').write_text(json.dumps(
        dict(records=_m5_monitor_records, hits=_m5_monitor_hits,
             armed=hv.m5_native_monitor_armed, atomic=False), indent=2) + '\n')
    return True


if (int(hv.ctx.spsr) & 0xf) >> 2:
    _m5_monitor_check_code()
hv.add_hw_bp(_m5_monitor_pc, hook=_m5_monitor_submit)
hv.m5_native_monitor_armed = True
print('M5_NATIVE_MONITOR_ARMED: two bounded native compute observations')
