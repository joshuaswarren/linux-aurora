# SPDX-License-Identifier: MIT
"""Observe one native M5 SKSM CL add-kicks record, without patching code.

Pinned 25G76 AGXCLChannelSKSM::submitBuffer, 0xfffffe0008984fc0:
the preceding stores completed the tag-14 record at x2; x24 still holds
the AGXKSMKickQueue. M5 encodeKickEntry uses queue+0x78 as its CPU ring,
queue+0x14 as stride, and the low byte of queue+0x48 as entry index.
All reads below translate guest CPU addresses and reject non-DRAM pages.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from m5_gpu_capture import M5GPUCapture

if (hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3'
        or int(hv.u.adt['/chosen'].chip_id) != 0x8142):
    raise RuntimeError('Pause the existing M5 guest before arming SKSM capture')

_m5_sksm_pc = 0xfffffe0008984fc0
_m5_sksm_dram_base = int(hv.u.adt['/chosen'].dram_base)
_m5_sksm_dram_end = _m5_sksm_dram_base + int(hv.u.adt['/chosen'].dram_size)
_m5_sksm_output = Path(os.environ['M5_GPU_TRACE_DIR'])
_m5_sksm_read_bytes = 0
_m5_sksm_hits = 0
_m5_sksm_priority_text = os.environ.get('M5_SKSM_CAPTURE_PRIORITY')
_m5_sksm_priority = int(_m5_sksm_priority_text) if _m5_sksm_priority_text is not None else None
_m5_sksm_exclude = {int(x, 0) for x in os.environ.get('M5_SKSM_EXCLUDE_QIDS', '').split(',') if x}
_m5_sksm_observations = []
if _m5_sksm_priority is not None and _m5_sksm_priority not in range(4):
    raise ValueError('M5 SKSM priority must be in 0..3')
_m5_sksm_roots = re.findall(r'\buat_global_root_pa=(0x[0-9a-f]+)',
                           (_m5_sksm_output / 'console.log').read_text())
if len(set(_m5_sksm_roots)) != 1:
    raise RuntimeError('M5 SKSM capture requires this boot\'s SPTM root receipt')
_m5_sksm_gpu = M5GPUCapture(hv.iface.readmem,
    (_m5_sksm_dram_base, _m5_sksm_dram_end - _m5_sksm_dram_base),
    int(_m5_sksm_roots[0], 16))


def _m5_sksm_read_gpu(address, size):
    global _m5_sksm_read_bytes
    if size <= 0 or _m5_sksm_read_bytes + size > 2 * 1024 * 1024:
        raise ValueError('M5 SKSM capture exceeds its DRAM read budget')
    _m5_sksm_read_bytes += size
    return _m5_sksm_gpu.read(address, size)


def _m5_sksm_read_cpu(address, size):
    global _m5_sksm_read_bytes
    if size < 0 or _m5_sksm_read_bytes + size > 2 * 1024 * 1024:
        raise ValueError('M5 SKSM capture exceeds its DRAM read budget')
    result = bytearray()
    while size:
        amount = min(size, 4096 - (address & 4095))
        pa = int(hv.p.hv_translate(address, False, False))
        if not _m5_sksm_dram_base <= pa < pa + amount <= _m5_sksm_dram_end:
            raise ValueError('M5 SKSM CPU view is unmapped or outside DRAM')
        data = hv.iface.readmem(pa, amount)
        if len(data) != amount:
            raise RuntimeError('Short M5 SKSM DRAM read')
        result.extend(data)
        _m5_sksm_read_bytes += amount
        address += amount
        size -= amount
    return bytes(result)


def _m5_sksm_check_code():
    # ldr x1,[x20,#0x428]; ldr x16,[x19], after the CL tag-14 stores.
    if _m5_sksm_read_cpu(_m5_sksm_pc, 8) != bytes.fromhex('811642f9700240f9'):
        raise RuntimeError('M5 SKSM capture site does not match pinned 25G76 code')


# hv_translate uses the interrupted EL's permissions. An EL0 pause cannot
# translate kernel text: validate on the EL1 breakpoint before any capture.
# When already paused in EL1, also reject mismatched code before arming.
if (int(hv.ctx.spsr) & 0xf) >> 2:
    _m5_sksm_check_code()


def _m5_sksm_submit(ctx):
    global _m5_sksm_hits
    _m5_sksm_hits += 1
    filter_error = None
    try:
        _m5_sksm_check_code()
        command_probe = _m5_sksm_read_cpu(int(ctx.regs[2]), 0x15)
        probe_tag, probe_qid, probe_count, probe_priority, probe_stamp, probe_dm = struct.unpack('<IHHBQI', command_probe)
        _m5_sksm_observations.append(dict(queue_id=probe_qid, priority=probe_priority,
                                         count=probe_count, stamp=hex(probe_stamp), data_master=probe_dm))
        if (probe_qid in _m5_sksm_exclude or
                (_m5_sksm_priority is not None and probe_priority != _m5_sksm_priority)):
            if _m5_sksm_hits < 1024:
                return True
            raise ValueError('M5 SKSM selected queue not seen within 1024 submissions')
    except Exception as error:
        filter_error = error
    # Remove first so even output-directory or read failures remain one-shot.
    hv.remove_hw_bp(_m5_sksm_pc)
    directory = _m5_sksm_output / ('sksm-submit-' + str(time.time_ns()))
    directory.mkdir(mode=0o700)
    manifest = dict(phase='host-tag14-before-channel-publication',
                    pc=hex(int(ctx.elr)), objects=[], device_memory_read=False,
                    requested_priority=_m5_sksm_priority, observed_submissions=_m5_sksm_hits,
                    excluded_queue_ids=sorted(_m5_sksm_exclude), observations=_m5_sksm_observations)

    def save(label, address, size, gpu=False):
        read = _m5_sksm_read_gpu if gpu else _m5_sksm_read_cpu
        first = read(address, size)
        second = read(address, size)
        name = label + '.bin'
        (directory / name).write_bytes(first)
        item = dict(label=label, address=hex(address), address_space='GPU' if gpu else 'CPU',
                    size=size, file=name,
                    sha256=hashlib.sha256(first).hexdigest(), stable=first == second)
        if first != second:
            second_name = label + '-second.bin'
            (directory / second_name).write_bytes(second)
            item.update(second_file=second_name,
                        second_sha256=hashlib.sha256(second).hexdigest())
        manifest['objects'].append(item)
        return first

    try:
        if filter_error:
            raise filter_error
        _m5_sksm_check_code()
        command = save('add-kicks-command', int(ctx.regs[2]), 0x15)
        tag, qid, count, priority, stamp, dm = struct.unpack('<IHHBQI', command)
        if tag != 14 or dm != 2 or not count:
            raise ValueError('M5 SKSM site did not contain a CL add-kicks command')
        queue = save('host-kick-queue', int(ctx.regs[24]), 0xb0)
        queue_id, entries, stride = struct.unpack_from('<III', queue, 0xc)
        queue_priority, = struct.unpack_from('<I', queue, 0x1c)
        if queue_id != qid or queue_priority != priority:
            raise ValueError('M5 SKSM command and retained queue disagree')
        if not (0 < entries <= 256 and 0x180 <= stride <= 0x1000
                and entries * stride <= 512 * 1024 and (stamp & 255) < entries):
            raise ValueError('Unexpected M5 SKSM ring geometry')
        cpu_ring, gpu_ring, firmware_ring = struct.unpack_from('<QQQ', queue, 0x78)
        # Native CPU views can be unmapped after the encoder finishes. Follow
        # the queue's retained firmware alias through the bounded GPU walker.
        ring = save('kick-ring', firmware_ring, entries * stride, gpu=True)
        manifest.update(queue_id=qid, add_count=count, priority=priority,
                        entry_stamp=hex(stamp), data_master=dm,
                        entry_count=entries, entry_stride=stride,
                        gpu_ring=hex(gpu_ring), cpu_ring=hex(cpu_ring),
                        firmware_ring=hex(firmware_ring))
        entry_offset = (stamp & 255) * stride
        manifest['payload_views'] = []
        # encodeKickEntry copies the payload's two GPU pointers to +0x10/+0x18
        # at 25G76 0xfffffe00089e7654..0x89e7668. These are bounded mapped-page
        # observations, not allocation extents or a claimed command decoder.
        for index, offset in enumerate((0x10, 0x18)):
            pointer, = struct.unpack_from('<Q', ring, entry_offset + offset)
            if not pointer:
                continue
            view = dict(index=index, pointer=hex(pointer))
            try:
                size = 0x4000 - (pointer & 0x3fff)
                save('payload-view-' + str(index), pointer, size, gpu=True)
                view['size'] = size
            except (RuntimeError, ValueError) as error:
                view['error'] = str(error)
            manifest['payload_views'].append(view)
        print('M5_SKSM_SUBMIT_CAPTURED qid=%d entries=%d stride=%#x stamp=%#x' %
              (qid, entries, stride, stamp))
    except Exception as error:
        manifest['error'] = str(error)
        print('M5_SKSM_SUBMIT_CAPTURE_FAILED', repr(error))
    finally:
        manifest['bytes_read'] = _m5_sksm_read_bytes
        (directory / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    return True


hv.add_hw_bp(_m5_sksm_pc, hook=_m5_sksm_submit)
print('M5_SKSM_SUBMIT_ARMED: one native compute kick queue, bounded DRAM only')
