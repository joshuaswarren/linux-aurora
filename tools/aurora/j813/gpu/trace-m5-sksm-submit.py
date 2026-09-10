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
_m5_sksm_extra_views = json.loads(os.environ.get('M5_SKSM_CONTEXT_VIEWS', '[]'))
if (not isinstance(_m5_sksm_extra_views, list) or len(_m5_sksm_extra_views) > 4
        or any(not isinstance(x, list) or len(x) != 2
               or not isinstance(x[0], int) or not 0 <= x[0] < (1 << 43)
               or not isinstance(x[1], int) or not 0 < x[1] <= 0x4000
               for x in _m5_sksm_extra_views)):
    raise ValueError('M5 extra context views require up to four [GPU VA, size] pairs')
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

    def save(label, address, size, gpu=False, context=None):
        if context is not None:
            def read(pointer, length):
                global _m5_sksm_read_bytes
                if length <= 0 or _m5_sksm_read_bytes + length > 2 * 1024 * 1024:
                    raise ValueError('M5 SKSM capture exceeds its DRAM read budget')
                _m5_sksm_read_bytes += length
                return _m5_sksm_gpu.read_context(pointer, length, context,
                    int(hv.u.adt['/arm-io/sgx'].gpu_region_base))
        else:
            read = _m5_sksm_read_gpu if gpu else _m5_sksm_read_cpu
        first = read(address, size)
        second = read(address, size)
        name = label + '.bin'
        (directory / name).write_bytes(first)
        item = dict(label=label, address=hex(address), address_space='GPU' if gpu else 'CPU',
                    size=size, file=name,
                    sha256=hashlib.sha256(first).hexdigest(), stable=first == second)
        if context is not None:
            item['gpu_context'] = context
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
        completion_view = None
        if os.environ.get('M5_SKSM_CAPTURE_COMPLETION') == '1':
            hardware = (_m5_sksm_output / 'preinit/gfx-asc/shared-cluster.bin').read_bytes()
            low, high, capacity, entry_size, ordinal = struct.unpack_from(
                '<QQIIQ', hardware, 0x2614 + priority * 0x20)
            if (capacity, entry_size, ordinal) != (0x800, 0x40, priority):
                raise ValueError('M5 completion descriptor differs from the captured profile')
            completion_view = (high, capacity * entry_size)
            save('completion-queue-before', *completion_view, gpu=True)
        # x20 is the retained AGXCLCommandDescriptor at this pinned site.
        # Capture a bounded view, including the native register generator's
        # inputs at +0x530/+0x538/+0x60a/+0x60b. This is not a sizeof claim.
        try:
            save('host-cl-descriptor-view', int(ctx.regs[20]), 0x800)
        except (RuntimeError, ValueError) as error:
            manifest['host_descriptor_error'] = str(error)
        # The retained accelerator supplies configureHardware's actual FIFO
        # offsets. These are host-DRAM fields, never reads from the MMIO base.
        try:
            accelerator, = struct.unpack_from('<Q', queue, 0x68)
            save('host-configure-geometry', accelerator + 0xee58, 12)
            save('host-sgx-map-pointer', accelerator + 0x368, 8)
            save('host-configure-flags', accelerator + 0x6e0, 16)
        except (RuntimeError, ValueError) as error:
            manifest['host_configure_error'] = str(error)
        entry_offset = (stamp & 255) * stride
        manifest['payload_views'] = []
        channel_views = []
        channel_item_commands = None

        def save_channel_commands(phase, pointers_address, items_address):
            pointers = _m5_sksm_read_gpu(pointers_address, 0x64)
            producer, = struct.unpack_from('<I', pointers, 0x40)
            capacity, = struct.unpack_from('<I', pointers, 0x60)
            if capacity != 0x500 or producer > capacity:
                raise ValueError('Unexpected native channel pointer geometry')
            # Controlled first-submit observation only. Large/reused queues
            # remain opaque rather than walking an unbounded retained graph.
            count = min(producer, 16)
            if count:
                items = _m5_sksm_read_gpu(items_address, count * 8)
                for i in range(count):
                    address, = struct.unpack_from('<Q', items, i * 8)
                    if address:
                        save('channel-command-%s-%d' % (phase, i), address, 0x100, gpu=True)
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
                payload = save('payload-view-' + str(index), pointer, size, gpu=True)
                view['size'] = size
                if index == 1 and len(payload) >= 0xc0:
                    # These are bounded observations of the native C0 cached
                    # channel record. Their labels do not qualify a Linux ABI.
                    channel_views.append(('channel-record', pointer, 0xc0))
                    for label, offset, extent in [('channel-pointers', 0, 0x100),
                                                  ('channel-items', 8, 0x1000),
                                                  ('channel-job-list', 0x10, 0x40),
                                                  ('channel-control', 0x9c, 0xc0)]:
                        address, = struct.unpack_from('<Q', payload, offset)
                        if address:
                            try:
                                save(label + '-before', address, extent, gpu=True)
                                channel_views.append((label, address, extent))
                            except (RuntimeError, ValueError) as error:
                                manifest.setdefault('channel_view_errors', {})[label] = str(error)
                    pointers_address, items_address = struct.unpack_from('<QQ', payload)
                    if pointers_address and items_address:
                        channel_item_commands = (pointers_address, items_address)
                        try:
                            save_channel_commands('before', *channel_item_commands)
                        except (RuntimeError, ValueError) as error:
                            manifest['channel_commands_error'] = str(error)
                if index == 0 and struct.unpack_from('<I', payload)[0] == 3:
                    # Only decode the independently measured 40-register CL
                    # profile. All other payload layouts stay opaque.
                    if struct.unpack_from('<4H', payload, 0x18) == (37, 39, 39, 40):
                        registers = dict(struct.unpack_from('<IQ', payload, 0x40 + i * 12)
                                         for i in range(40))
                        context_word = registers[0x10201]
                        if context_word > 0xffff:
                            raise ValueError('M5 CL context word exceeds the measured profile')
                        gpu_context = context_word >> 8
                        manifest['compute_context'] = gpu_context
                        manifest['context_views'] = []
                        for name, address in [('cdm', registers[0x1a420]),
                                              ('preempt', registers[0x1a510]),
                                              ('robustness', registers[0x14070] & ~0x3fff)]:
                            info = dict(label=name, pointer=hex(address), gpu_context=gpu_context)
                            try:
                                length = 0x4000 - (address & 0x3fff)
                                command_view = save('compute-' + name, address, length, gpu=True, context=gpu_context)
                                info['size'] = length
                                if name == 'cdm' and len(command_view) >= 16:
                                    w0, w1, w2, w3 = struct.unpack_from('<4I', command_view)
                                    if w0 >> 29 == 0:
                                        # AGX3 launch: VA[47:38] in word1[31:22],
                                        # VA[37:6] in word2. Preserve the opaque
                                        # header payload; do not infer its mode.
                                        usc = ((w1 >> 22) << 38) | (w2 << 6)
                                        info['usc_pointer'] = hex(usc)
                                        try:
                                            save('compute-usc', usc,
                                                 min(0x1000, 0x4000 - (usc & 0x3fff)),
                                                 gpu=True, context=gpu_context)
                                        except (RuntimeError, ValueError) as error:
                                            info['usc_error'] = str(error)
                            except (RuntimeError, ValueError) as error:
                                info['error'] = str(error)
                            manifest['context_views'].append(info)
                        for extra_index, (address, length) in enumerate(_m5_sksm_extra_views):
                            info = dict(label='extra-' + str(extra_index), pointer=hex(address),
                                        gpu_context=gpu_context)
                            try:
                                save('compute-' + info['label'], address, length,
                                     gpu=True, context=gpu_context)
                                info['size'] = length
                            except (RuntimeError, ValueError) as error:
                                info['error'] = str(error)
                            manifest['context_views'].append(info)
            except (RuntimeError, ValueError) as error:
                view['error'] = str(error)
            manifest['payload_views'].append(view)
        print('M5_SKSM_SUBMIT_CAPTURED qid=%d entries=%d stride=%#x stamp=%#x' %
              (qid, entries, stride, stamp))
        if os.environ.get('M5_SKSM_CAPTURE_COMPLETION') == '1' and _m5_sksm_extra_views:
            if 'compute_context' not in manifest:
                raise ValueError('Completion capture requires a measured compute context')
            completion_pc = 0xfffffe00089855b0
            descriptor_address = int(ctx.regs[20])
            completion_hits = 0

            def completed(completion_ctx):
                nonlocal completion_hits
                completion_hits += 1
                if int(completion_ctx.regs[0]) != descriptor_address and completion_hits < 64:
                    return True
                hv.remove_hw_bp(completion_pc)
                try:
                    if int(completion_ctx.regs[0]) != descriptor_address:
                        raise ValueError('Matching M5 descriptor did not complete within 64 callbacks')
                    if _m5_sksm_read_cpu(completion_pc, 8) != bytes.fromhex('f30300aa080840f9'):
                        raise ValueError('M5 completion site differs from pinned native code')
                    save('completed-host-cl-descriptor', descriptor_address, 0x800)
                    if completion_view is not None:
                        save('completion-queue-after', *completion_view, gpu=True)
                    for label, address, extent in channel_views:
                        try:
                            save(label + '-after', address, extent, gpu=True)
                        except (RuntimeError, ValueError) as error:
                            manifest.setdefault('completed_channel_errors', {})[label] = str(error)
                    if channel_item_commands is not None:
                        try:
                            save_channel_commands('after', *channel_item_commands)
                        except (RuntimeError, ValueError) as error:
                            manifest['completed_channel_commands_error'] = str(error)
                    for i, (address, length) in enumerate(_m5_sksm_extra_views):
                        save('completed-extra-' + str(i), address, length,
                             gpu=True, context=manifest['compute_context'])
                    manifest['completion'] = dict(phase='AGXCLCommandDescriptor::complete',
                        same_descriptor=True, observed_callbacks=completion_hits,
                        native_instruction_bytes_checked=True)
                    print('M5_SKSM_MATCHING_COMPLETION_CAPTURED qid=%d stamp=%#x' % (qid, stamp))
                except Exception as error:
                    manifest['completion_error'] = str(error)
                    print('M5_SKSM_COMPLETION_CAPTURE_FAILED', repr(error))
                manifest['bytes_read'] = _m5_sksm_read_bytes
                (directory / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
                return True

            hv.add_hw_bp(completion_pc, hook=completed)
    except Exception as error:
        manifest['error'] = str(error)
        print('M5_SKSM_SUBMIT_CAPTURE_FAILED', repr(error))
    finally:
        manifest['bytes_read'] = _m5_sksm_read_bytes
        (directory / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    return True


hv.add_hw_bp(_m5_sksm_pc, hook=_m5_sksm_submit)
print('M5_SKSM_SUBMIT_ARMED: one native compute kick queue, bounded DRAM only')
