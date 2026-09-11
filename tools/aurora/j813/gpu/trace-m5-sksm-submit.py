# SPDX-License-Identifier: MIT
"""Observe native M5 SKSM submission, with an explicit program-replay mode.

Pinned 25G76 AGXCLChannelSKSM::submitBuffer, 0xfffffe0008984fc0:
the preceding stores completed the tag-14 record at x2; x24 still holds
the AGXKSMKickQueue. M5 encodeKickEntry uses queue+0x78 as its CPU ring,
queue+0x14 as stride, and the low byte of queue+0x48 as entry index.
Guest-memory reads translate CPU addresses and reject non-DRAM pages.
M5_SKSM_CAPTURE_MMIO=1 additionally enables the guarded native sampler;
its qualification flags distinguish valid observations from unread fields.
M5_SKSM_PROGRAM_REPLAY explicitly opts into a reversible, controlled native
program-page experiment. Without that option this observer remains read-only.
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
_m5_pool_control_pc = 0xfffffe00089a766c
_m5_pool_control_armed = False
_m5_pool_control_hits = 0
_m5_pool_control_observations = []
_m5_sksm_priority_text = os.environ.get('M5_SKSM_CAPTURE_PRIORITY')
_m5_sksm_priority = int(_m5_sksm_priority_text) if _m5_sksm_priority_text is not None else None
_m5_sksm_exclude = {int(x, 0) for x in os.environ.get('M5_SKSM_EXCLUDE_QIDS', '').split(',') if x}
_m5_sksm_observations = []
_m5_sksm_monitor = None
if os.environ.get('M5_SKSM_CAPTURE_MMIO') == '1':
    from m5_native_monitor import M5NativeMonitor
    if getattr(hv, 'm5_native_monitor', None) is None:
        hv.m5_native_monitor = M5NativeMonitor(hv)
    _m5_sksm_monitor = hv.m5_native_monitor
_m5_sksm_extra_views = json.loads(os.environ.get('M5_SKSM_CONTEXT_VIEWS', '[]'))
if (not isinstance(_m5_sksm_extra_views, list) or len(_m5_sksm_extra_views) > 4
        or any(not isinstance(x, list) or len(x) != 2
               or not isinstance(x[0], int) or not 0 <= x[0] < (1 << 43)
               or not isinstance(x[1], int) or not 0 < x[1] <= 0x4000
               for x in _m5_sksm_extra_views)):
    raise ValueError('M5 extra context views require up to four [GPU VA, size] pairs')
if _m5_sksm_priority is not None and _m5_sksm_priority not in range(4):
    raise ValueError('M5 SKSM priority must be in 0..3')
_m5_sksm_replay_config = json.loads(os.environ.get('M5_SKSM_PROGRAM_REPLAY', 'null'))
if _m5_sksm_replay_config is not None:
    if (not isinstance(_m5_sksm_replay_config, dict)
            or not {'native', 'linux', 'workload'} <= set(_m5_sksm_replay_config)
            or set(_m5_sksm_replay_config) - {'native', 'linux', 'workload', 'variant', 'stamp'}
            or any(not isinstance(_m5_sksm_replay_config[k], str)
                   or not Path(_m5_sksm_replay_config[k]).is_absolute()
                   for k in ('native', 'linux', 'workload'))
            or _m5_sksm_replay_config.get('variant', 'linux-pages') not in ('linux-pages', 'shader-body-only', 'shader-record', 'shader-prefix', 'shader-prefix-linux-dispatch')
            or not (_m5_sksm_replay_config.get('stamp', 1) == 'captured'
                    or (type(_m5_sksm_replay_config.get('stamp', 1)) is int
                        and 1 <= _m5_sksm_replay_config.get('stamp', 1) < 256))
            or _m5_sksm_priority != 2
            or os.environ.get('M5_SKSM_CAPTURE_COMPLETION') != '1'
            or _m5_sksm_extra_views != [[0x10000000000, 0x4000],
                                       [0x1000001c000, 0x4000],
                                       [0x10000090000, 0x4000]]
            or getattr(getattr(hv, 'm5_program_replay', None), 'active', False)):
        raise ValueError('Replay requires explicit capture paths, priority 2, all program views and completion')
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
    global _m5_sksm_hits, _m5_pool_control_armed
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
                # On this M5 carrier, returning at the filtered breakpoint
                # repeatedly traps the same PC/stamp instead of progressing.
                # Preserve the verified instruction's exact effect: LDR X1,
                # [X20,#0x428]. This read is still restricted to guest DRAM.
                ctx.regs[1], = struct.unpack('<Q', _m5_sksm_read_cpu(int(ctx.regs[20]) + 0x428, 8))
                ctx.elr += 4
                return True
            raise ValueError('M5 SKSM selected queue not seen within 1024 submissions')
    except Exception as error:
        filter_error = error
    # Remove first so even output-directory or read failures remain one-shot.
    hv.remove_hw_bp(_m5_sksm_pc)
    if _m5_pool_control_armed:
        hv.remove_hw_bp(_m5_pool_control_pc)
        _m5_pool_control_armed = False
    directory = _m5_sksm_output / ('sksm-submit-' + str(time.time_ns()))
    directory.mkdir(mode=0o700)
    manifest = dict(phase='host-tag14-before-channel-publication',
                    pc=hex(int(ctx.elr)), objects=[], device_memory_read=_m5_sksm_monitor is not None,
                    requested_priority=_m5_sksm_priority, observed_submissions=_m5_sksm_hits,
                    excluded_queue_ids=sorted(_m5_sksm_exclude), observations=_m5_sksm_observations)
    replay = None
    if _m5_pool_control_observations:
        manifest['prior_pool_controls'] = _m5_pool_control_observations

    def save(label, address, size, gpu=False, context=None, physical=False):
        if physical:
            def read(pointer, length):
                global _m5_sksm_read_bytes
                if length <= 0 or _m5_sksm_read_bytes + length > 2 * 1024 * 1024:
                    raise ValueError('M5 SKSM capture exceeds its DRAM read budget')
                _m5_sksm_read_bytes += length
                return _m5_sksm_gpu._read_dram(pointer, length)
        elif context is not None:
            def read(pointer, length):
                global _m5_sksm_read_bytes
                if length <= 0 or _m5_sksm_read_bytes + length > 2 * 1024 * 1024:
                    raise ValueError('M5 SKSM capture exceeds its DRAM read budget')
                _m5_sksm_read_bytes += length
                return _m5_sksm_gpu.read_context(pointer, length, context,
                    int(hv.u.adt['/arm-io/sgx'].gpu_region_base))
        else:
            read = _m5_sksm_read_gpu if gpu else _m5_sksm_read_cpu
        mapping_before = None
        mapping_error = None
        if context is not None:
            try:
                mapping_before = _m5_sksm_gpu.mapping(address, size, context,
                    int(hv.u.adt['/arm-io/sgx'].gpu_region_base))
            except (RuntimeError, ValueError) as error:
                mapping_error = str(error)
        first = read(address, size)
        second = read(address, size)
        name = label + '.bin'
        (directory / name).write_bytes(first)
        item = dict(label=label, address=hex(address),
                    address_space='physical-DRAM' if physical else ('GPU' if gpu else 'CPU'),
                    size=size, file=name,
                    sha256=hashlib.sha256(first).hexdigest(), stable=first == second)
        if context is not None:
            item['gpu_context'] = context
            # Equal bytes alone do not qualify the GPU translation. Retain
            # the actual page-table walk on both sides of the data reads.
            if mapping_before is not None:
                item['mapping'] = mapping_before
                try:
                    mapping_after = _m5_sksm_gpu.mapping(address, size, context,
                        int(hv.u.adt['/arm-io/sgx'].gpu_region_base))
                    item['mapping_stable'] = mapping_before == mapping_after
                    if not item['mapping_stable']:
                        item['mapping_after'] = mapping_after
                except (RuntimeError, ValueError) as error:
                    mapping_error = str(error)
            if mapping_error is not None:
                item.update(mapping_stable=False, mapping_error=mapping_error)
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
        if _m5_sksm_monitor is not None:
            if probe_tag != 14 or probe_dm != 2 or not probe_count:
                raise ValueError('M5 monitor requires a measured compute submission')
            manifest['mmio_before_publication'] = _m5_sksm_monitor.capture(probe_qid)
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
        outer_views = []
        if os.environ.get('M5_SKSM_CAPTURE_OUTER') == '1':
            main = (_m5_sksm_output / 'preinit/gfx-asc/main-config.bin').read_bytes()
            grid, = struct.unpack_from('<Q', main, 0x20)
            outer_views.append(('outer-state-grid', grid - 0xc0, 0x340))
            for index in (2, 5, 8, 11):
                address, = struct.unpack_from('<Q', main, 0x20 + index * 0x20 + 24)
                outer_views.append(('outer-cl-ring-' + str(index), address, 0x1800))
            for label, address, extent in outer_views:
                save(label + '-before', address, extent, gpu=True)
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
                # A valid register program can cross a 16-KiB boundary.
                # Follow both mapped pages through the same guarded walker.
                minimum = 0x1000 if index == 0 else 0xc0
                size = max(minimum, 0x4000 - (pointer & 0x3fff))
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
                        region = int(hv.u.adt['/arm-io/sgx'].gpu_region_base)
                        roots = save('compute-context-roots', region + gpu_context * 16,
                                     16, physical=True)
                        manifest['compute_context_roots'] = [hex(x) for x in struct.unpack('<QQ', roots)]
                        if os.environ.get('M5_SKSM_CAPTURE_POOL') == '1':
                            # The controlled M5 profile's private-memory pool.
                            # Capture metadata prefixes (not allocation sizes)
                            # and both ends of each retained 2-MiB data block.
                            pool = []
                            for name, address, length in [
                                    ('directory', 0x7000000000, 0x10000),
                                    ('freelist', 0x7000408000, 0x4000),
                                    ('operand-state', registers[0xa5c1] - 5, 0x20000)]:
                                info = dict(label=name, address=hex(address))
                                try:
                                    save('compute-pool-' + name, address, length,
                                         gpu=True, context=gpu_context)
                                    info['size'] = length
                                except (RuntimeError, ValueError) as error:
                                    info['error'] = str(error)
                                pool.append(info)
                            for block in range(14):
                                base = 0x7000448000 + block * 0x208000
                                for side, address in [('head', base), ('tail', base + 0x200000 - 32)]:
                                    info = dict(label='block-%d-%s' % (block, side), address=hex(address))
                                    try:
                                        save('compute-pool-' + info['label'], address, 32,
                                             gpu=True, context=gpu_context)
                                        info['mapping'] = _m5_sksm_gpu.mapping(address, 32, gpu_context, region)
                                    except (RuntimeError, ValueError) as error:
                                        info['error'] = str(error)
                                    pool.append(info)
                            for role in ('gfx-asc', 'gfx1-asc'):
                                main = (_m5_sksm_output / 'preinit' / role / 'main-config.bin').read_bytes()
                                hardware, = struct.unpack_from('<Q', main)
                                main_offset = 0x1e5c0 if role == 'gfx-asc' else 0x22a80
                                save('compute-pool-' + role + '-control-ring',
                                     hardware + main_offset + 0x4c0, 0x4000, gpu=True)
                            shared_control, = struct.unpack_from('<Q', payload, 0xfb2)
                            event_state = save('compute-pool-event-state', shared_control, 0x70, gpu=True)
                            # M5 halLoadUMADesc uses a 32-byte hardware-ID slot.
                            # Preserve the live slot before interpreting its
                            # fields; Neo uses a different packed layout.
                            buffer_id, = struct.unpack_from('<I', event_state, 8)
                            if buffer_id >= 512:
                                raise ValueError('M5 pool hardware ID exceeds the retained UMA page')
                            primary = (_m5_sksm_output / 'preinit/gfx-asc/main-config.bin').read_bytes()
                            uma_low, uma_high = struct.unpack_from('<QQ', primary, 0x2e8)
                            manifest['compute_pool_uma'] = dict(buffer_id=buffer_id,
                                high=hex(uma_high), low=hex(uma_low))
                            save('compute-pool-uma-slot', uma_high + buffer_id * 32, 32, gpu=True)
                            alternate_low, alternate_high = struct.unpack_from('<QQ', primary, 0x2d8)
                            manifest['compute_pool_pb_table'] = dict(buffer_id=buffer_id,
                                high=hex(alternate_high), low=hex(alternate_low))
                            save('compute-pool-pb-slot', alternate_high + buffer_id * 32, 32, gpu=True)
                            directory_address, = struct.unpack_from('<Q', event_state, 0x14)
                            manifest['compute_pool_directory_edges'] = []
                            for offset in (0xffff, 0x10000, 0x3fffff, 0x400000, 0x404000):
                                edge = dict(offset=hex(offset))
                                try:
                                    edge['mapping'] = _m5_sksm_gpu.mapping(
                                        directory_address + offset, 1, gpu_context, region)
                                except (RuntimeError, ValueError) as error:
                                    edge['error'] = str(error)
                                manifest['compute_pool_directory_edges'].append(edge)
                            manifest['compute_pool'] = pool
                        if os.environ.get('M5_SKSM_CAPTURE_OWNERS') == '1':
                            # Firmware compute setup follows +fb2 even when
                            # +f5c is zero. Capture the actual retained owners,
                            # not a guessed "no event" null-pointer variant.
                            if len(payload) < 0x1000:
                                raise ValueError('M5 owner capture needs the full descriptor prefix')
                            save('compute-root-table-prefix', region, 64, physical=True)
                            save('compute-global-root-page', int(_m5_sksm_roots[0], 16),
                                 0x4000, physical=True)
                            owners = []
                            aliases = []

                            def alias_view(name, address, length, selected_context=None):
                                info = dict(label=name, address=hex(address), context=selected_context)
                                try:
                                    first = _m5_sksm_gpu.mapping(address, length, selected_context, region)
                                    data = save('compute-alias-' + name, address, length,
                                                gpu=True, context=selected_context)
                                    second = _m5_sksm_gpu.mapping(address, length, selected_context, region)
                                    info.update(mapping=first, mapping_stable=first == second)
                                    aliases.append(info)
                                    return data
                                except (RuntimeError, ValueError) as error:
                                    info['error'] = str(error)
                                    aliases.append(info)
                                    return None

                            descriptor_low, = struct.unpack_from('<Q', payload, 0x740)
                            for name, address, length in [
                                    ('descriptor', descriptor_low - 0x40, 0x1000),
                                    ('operand', registers[0xa5c1] - 5, 0x4000),
                                    ('queue', gpu_ring, 0x4000)]:
                                for selected_context in (0, gpu_context):
                                    alias_view(name + '-ctx' + str(selected_context),
                                               address, length, selected_context)
                            alias_view('descriptor-global', pointer, 0x1000)
                            alias_view('queue-global', firmware_ring, 0x4000)
                            for name, offset, length in [
                                    ('event-state', 0xfb2, 0x1000),
                                    ('zero-page', 0xfcb, 0x4000),
                                    ('dispatch-a', 0xf40, 0x40),
                                    ('dispatch-b', 0xf48, 0x40),
                                    ('status-a', 0xf7c, 0x40),
                                    ('status-b', 0xf84, 0x40)]:
                                pointer, = struct.unpack_from('<Q', payload, offset)
                                info = dict(label=name, pointer=hex(pointer))
                                if pointer:
                                    address = pointer if name in ('event-state', 'zero-page') else pointer & ~0x3f
                                    try:
                                        data = save('compute-owner-' + name, address, length, gpu=True)
                                        info.update(address=hex(address), size=length)
                                        if name == 'event-state':
                                            info['prefix_qwords'] = [hex(x) for x in struct.unpack_from('<16Q', data)]
                                            event_low, = struct.unpack_from('<Q', data, 0x30)
                                            event_high, = struct.unpack_from('<Q', data, 0x4c)
                                            if event_low:
                                                for selected_context in (0, gpu_context):
                                                    alias_view('event-link-ctx' + str(selected_context),
                                                               event_low, 0x4000, selected_context)
                                            if event_high:
                                                alias_view('event-link-global', event_high, 0x4000)
                                                alias_view('event-ring-global', event_high + 0x4000, 0x4000)
                                                alias_view('event-zero-global', event_high + 0x8000, 0x4000)
                                            if event_low >= 0x4000:
                                                alias_view('event-header-ctx' + str(gpu_context),
                                                           event_low - 0x4000, 0x4000, gpu_context)
                                                alias_view('event-zero-ctx' + str(gpu_context),
                                                           event_low + 0x4000, 0x4000, gpu_context)
                                    except (RuntimeError, ValueError) as error:
                                        info['error'] = str(error)
                                owners.append(info)
                            for root_index, tagged in enumerate(struct.unpack('<QQ', roots)):
                                try:
                                    save('compute-root-page-' + str(root_index),
                                         tagged & ((1 << 42) - 1) & ~0x3fff,
                                         0x4000, physical=True)
                                except (RuntimeError, ValueError) as error:
                                    owners.append(dict(label='root-' + str(root_index), error=str(error)))
                            manifest['compute_owners'] = owners
                            manifest['compute_aliases'] = aliases
                        context_state, = struct.unpack_from('<Q', payload, 0x10)
                        if context_state:
                            try:
                                state = save('compute-context-state', context_state,
                                     min(0x4000, 0x4000 - (context_state & 0x3fff)), gpu=True)
                                if os.environ.get('M5_SKSM_CAPTURE_OWNERS') == '1':
                                    count_pointer, = struct.unpack_from('<Q', state)
                                    if count_pointer:
                                        save('compute-owner-submitted-count', count_pointer & ~0x3f,
                                             0x40, gpu=True)
                            except (RuntimeError, ValueError) as error:
                                manifest['compute_context_state_error'] = str(error)
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
                    # Pinned first instruction is MOV X19,X0. Preserve its
                    # effect while advancing past this filtered callback.
                    completion_ctx.regs[19] = int(completion_ctx.regs[0])
                    completion_ctx.elr += 4
                    return True
                hv.remove_hw_bp(completion_pc)
                matching_completed = False
                try:
                    if int(completion_ctx.regs[0]) != descriptor_address:
                        raise ValueError('Matching M5 descriptor did not complete within 64 callbacks')
                    if _m5_sksm_read_cpu(completion_pc, 8) != bytes.fromhex('f30300aa080840f9'):
                        raise ValueError('M5 completion site differs from pinned native code')
                    matching_completed = True
                    if _m5_sksm_monitor is not None:
                        manifest['mmio_at_completion'] = _m5_sksm_monitor.capture(qid)
                    save('completed-host-cl-descriptor', descriptor_address, 0x800)
                    for label, address, extent in outer_views:
                        save(label + '-after', address, extent, gpu=True)
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
                if replay is not None and matching_completed:
                    try:
                        replay.restore()
                        manifest['program_replay_state'] = replay.receipt['state']
                        print('M5_PROGRAM_REPLAY_RESTORED: all journaled native bytes verified')
                    except Exception as error:
                        manifest['program_replay_restore_error'] = str(error)
                        print('M5_PROGRAM_REPLAY_RESTORE_FAILED', repr(error))
                manifest['bytes_read'] = _m5_sksm_read_bytes
                (directory / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
                # A failed or unmatched restoration must leave the guest
                # paused. The journal and hv.m5_program_replay remain available.
                return replay is None or not replay.active

            hv.add_hw_bp(completion_pc, hook=completed)
            if _m5_sksm_replay_config is not None:
                expected_stamp = _m5_sksm_replay_config.get('stamp', 1)
                # Queue IDs and stamps can be recycled independently of the
                # process. Opt-in 'captured' uses the observed stamp while
                # retaining every exact program, context and buffer guard.
                if expected_stamp == 'captured':
                    expected_stamp = stamp
                if (stamp != expected_stamp or count != 1 or manifest.get('compute_context') != 3
                        or _m5_sksm_read_cpu(completion_pc, 8) != bytes.fromhex('f30300aa080840f9')):
                    raise ValueError('Replay requires a single controlled command at the explicit stamp and pinned completion')
                from m5_program_replay import M5ProgramReplay
                replay = M5ProgramReplay(_m5_sksm_gpu,
                    lambda pa, data: hv.iface.writemem(pa | 0xa0000000000, data),
                    hv.p.dc_civac, manifest['compute_context'],
                    int(hv.u.adt['/arm-io/sgx'].gpu_region_base), directory)
                hv.m5_program_replay = replay
                gpu_descriptor, = struct.unpack_from('<Q', ring, entry_offset + 0x10)
                replay.prepare(descriptor_address=gpu_descriptor,
                    **{k: v for k, v in _m5_sksm_replay_config.items() if k != 'stamp'})
                replay.apply()
                os.environ.pop('M5_SKSM_PROGRAM_REPLAY', None)
                manifest.update(program_memory_written=True,
                                program_replay_state=replay.receipt['state'],
                                program_replay_expected_stamp=hex(expected_stamp),
                                program_replay_receipt='program-replay.json')
                print('M5_PROGRAM_REPLAY_APPLIED: variant=%s, three program-page readbacks verified' %
                      replay.receipt.get('variant', 'linux-pages'))
    except Exception as error:
        manifest['error'] = str(error)
        print('M5_SKSM_SUBMIT_CAPTURE_FAILED', repr(error))
    finally:
        manifest['bytes_read'] = _m5_sksm_read_bytes
        (directory / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    # apply() rolls back any partial writes before publication. If restoration
    # itself failed, stop here instead of publishing a partially modified job.
    return replay is None or not replay.active or replay.receipt['state'] == 'applied'


hv.add_hw_bp(_m5_sksm_pc, hook=_m5_sksm_submit)
if os.environ.get('M5_SKSM_CAPTURE_POOL') == '1':
    def _m5_pool_control(ctx):
        global _m5_pool_control_armed, _m5_pool_control_hits, _m5_sksm_read_bytes
        try:
            if _m5_sksm_read_cpu(_m5_pool_control_pc, 8) != bytes.fromhex('390040b928f77ed3'):
                raise ValueError('M5 pool-control site differs from pinned 25G76 code')
            _m5_pool_control_hits += 1
            address = int(ctx.regs[1])
            opcode, = struct.unpack('<I', _m5_sksm_read_cpu(address, 4))
            if opcode == 0x20:
                directory = _m5_sksm_output / ('pool-control-' + str(time.time_ns()))
                directory.mkdir(mode=0o700)
                first, second = _m5_sksm_read_cpu(address, 64), _m5_sksm_read_cpu(address, 64)
                (directory / 'command.bin').write_bytes(first)
                record = dict(directory=directory.name, command_stable=first == second,
                              command_sha256=hashlib.sha256(first).hexdigest(),
                              native_instruction_bytes_checked=True, device_memory_read=False)
                pointer, = struct.unpack_from('<Q', first, 0x14)
                record['shared_control'] = hex(pointer)
                try:
                    before = _m5_sksm_read_gpu(pointer, 0x70)
                    repeat = _m5_sksm_read_gpu(pointer, 0x70)
                    (directory / 'event-before.bin').write_bytes(before)
                    record['event_stable'] = before == repeat
                    if (os.environ.get('M5_SKSM_CAPTURE_POOL_GROW') == '1'
                            and len(_m5_pool_control_observations) < 2):
                        # A post-submit FList cannot establish its initial
                        # directory or the separately appended grow records.
                        # Capture both candidate address spaces before the
                        # native control record is published, without MMIO.
                        table, grow = struct.unpack_from('<QQ', first, 0x1c)
                        count, = struct.unpack_from('<I', first, 0x2c)
                        page_list, = struct.unpack_from('<Q', before, 0x14)
                        if not 0 < count <= 0x200:
                            raise ValueError('M5 grow capture count outside bounded profile')
                        views = []
                        context_word, = struct.unpack_from('<I', first, 4)
                        if context_word > 63:
                            raise ValueError('M5 control context outside bounded profile')
                        record['control_word_04'] = context_word
                        for gpu_context in dict.fromkeys((0, context_word)):
                            for label, target, length in (
                                    ('freelist-prefix', table, 0x1000),
                                    ('grow-records', grow, count * 8),
                                    ('directory-prefix', page_list, 0x100)):
                                view = dict(label=label, address=hex(target),
                                            size=length, gpu_context=gpu_context)
                                try:
                                    samples = []
                                    for _ in range(2):
                                        if _m5_sksm_read_bytes + length > 2 * 1024 * 1024:
                                            raise ValueError('M5 SKSM capture exceeds its DRAM read budget')
                                        _m5_sksm_read_bytes += length
                                        samples.append(_m5_sksm_gpu.read_context(target, length,
                                            gpu_context, int(hv.u.adt['/arm-io/sgx'].gpu_region_base)))
                                    name = '%s-context%d.bin' % (label, gpu_context)
                                    (directory / name).write_bytes(samples[0])
                                    view.update(file=name, stable=samples[0] == samples[1],
                                                sha256=hashlib.sha256(samples[0]).hexdigest())
                                except (RuntimeError, ValueError) as error:
                                    view['error'] = str(error)
                                views.append(view)
                        record['before_pool_views'] = views
                except (RuntimeError, ValueError) as error:
                    record['event_error'] = str(error)
                (directory / 'manifest.json').write_text(json.dumps(record, indent=2) + '\n')
                _m5_pool_control_observations.append(record)
                print('M5_POOL_CONTROL_CAPTURED', directory.name)
            if _m5_pool_control_hits >= 512 or len(_m5_pool_control_observations) >= 8:
                hv.remove_hw_bp(_m5_pool_control_pc)
                _m5_pool_control_armed = False
            # The pinned instruction is LDR W25,[X1]. Advance its exact effect
            # to avoid the carrier's repeated-trap path while this hook stays.
            ctx.regs[25] = opcode
            ctx.elr += 4
        except Exception as error:
            hv.remove_hw_bp(_m5_pool_control_pc)
            _m5_pool_control_armed = False
            print('M5_POOL_CONTROL_CAPTURE_FAILED', repr(error))
        return True

    hv.add_hw_bp(_m5_pool_control_pc, hook=_m5_pool_control)
    _m5_pool_control_armed = True
print('M5_SKSM_SUBMIT_ARMED: one native compute kick queue, bounded DRAM'
      + (' and guarded MMIO observations' if _m5_sksm_monitor is not None else '')
      + ('; CONTROLLED PROGRAM REPLAY ENABLED' if _m5_sksm_replay_config is not None else '; read-only'))
