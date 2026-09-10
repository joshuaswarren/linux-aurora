"""AVE ring publication survey plus iBoot-preloaded firmware extraction.

Extends the qualified command observer. Reads only declared firmware RAM,
validated IPC surfaces, and driver channel objects; never polls MMIO. Exact
register moves are emulated at four execution breakpoints. No code patching.
"""
from pathlib import Path
import hashlib
import json
import struct
import time

_ring_source = Path(__file__).resolve().parent
exec(compile((_ring_source / 'trace-j813-ave-commands.py').read_text(),
             str(_ring_source / 'trace-j813-ave-commands.py'), 'exec'), globals())
_cmd_limit = 256
_ring_log = (_cmd_dir / 'ave-rings.jsonl').open('x', buffering=1)
_ring_entries = {'ring32': 0xfffffe0008cc3294, 'ring64': 0xfffffe0008cc5a24}
_ring_base_emit = _cmd_emit
_ring_base_poll = _cmd_poll
_ring_base_break = hv.handle_break
_ring_surfaces = {}
_ring_pending = {}
_ring_count = 0


def _ring_emit(kind, **data):
    _ring_log.write(json.dumps({'kind': kind, 'host_monotonic_ns': time.monotonic_ns(),
                                'phase': _ave_phase, **data}) + '\n')


def _ring_surface_read(address, size):
    if not 0 < size <= 65536:
        raise ValueError('Invalid ring read length')
    for base, metadata in _ring_surfaces.items():
        if base <= address and address + size <= base + metadata['size']:
            return _cmd_read(address, size)
    raise ValueError('Ring address is outside every observed IPC surface')


def _ring_channel(wrapper):
    head = _cmd_read(wrapper, 16)
    channel = _cmd_va(struct.unpack_from('<Q', head)[0])
    raw = _cmd_read(channel, 0x30)
    mode, count = struct.unpack_from('<II', raw, 0x10)
    ring = _cmd_va(struct.unpack_from('<Q', raw, 0x18)[0])
    consumer, producer = struct.unpack_from('<ii', raw, 0x20)
    if not 1 <= count <= 1024 or mode not in (0, 1):
        raise ValueError(f'Unexpected channel mode/count: {mode}/{count}')
    if not -1 <= consumer < count or not -1 <= producer < count:
        raise ValueError('Ring indices are out of range')
    if not any(base <= ring and ring + count * 64 <= base + info['size']
               for base, info in _ring_surfaces.items()):
        raise ValueError('Ring storage is outside the observed IPC surfaces')
    return {'wrapper': hex(wrapper), 'wrapper_hex': head.hex(),
            'wide': bool(head[12] & 1), 'channel_object': hex(channel),
            'channel_hex': raw.hex(), 'mode': mode, 'slot_count': count,
            'ring_base': hex(ring), 'consumer_index': consumer,
            'producer_index': producer,
            'notify_callback_signed': hex(struct.unpack_from('<Q', raw)[0]),
            'notify_argument': hex(struct.unpack_from('<Q', raw, 8)[0])}


def _cmd_emit(kind, **data):
    if kind == 'command' and data['function'] == 'ipc-send':
        base = int(data['surface_kernel_base'], 16)
        if base not in _ring_surfaces:
            _ring_surfaces[base] = {'size': data['surface_size'],
                                    'firmware_base': data['surface_firmware_base']}
            # Snapshot startup descriptors before the first command. This is
            # not the whole 20 MiB surface or an arbitrary guest-memory dump.
            size = min(16384, data['surface_size'])
            filename = f'ipc-prefix-{len(_ring_surfaces):02d}.bin'
            payload = _ring_surface_read(base, size)
            (_cmd_dir / filename).write_bytes(payload)
            _ring_emit('ipc-surface', kernel_base=hex(base), **_ring_surfaces[base],
                       prefix_file=filename, prefix_sha256=hashlib.sha256(payload).hexdigest())
    elif kind == 'command' and data['function'] == 'channel-send':
        metadata = _ring_channel(int(data['args'][0], 16))
        slot = metadata['producer_index']
        if slot < 0:
            raise ValueError('Send entered while the observed ring was full')
        entry = int(metadata['ring_base'], 16) + slot * 64
        data['ring'] = metadata
        data['ring_before_hex'] = _ring_surface_read(entry, 64).hex()
        key = (int(metadata['channel_object'], 16), slot)
        if key in _ring_pending:
            raise ValueError('An earlier send did not reach ring publication')
        _ring_pending[key] = {'send_seq': data['seq'], 'firmware_address': data['firmware_address'],
                              'length': data['length'], 'flags': data['flags'], **metadata}
    _ring_base_emit(kind, **data)


def _ring_poll():
    old_state = _cmd_state
    _ring_base_poll()
    if old_state == 'waiting' and _cmd_state == 'armed':
        try:
            for name, address in _ring_entries.items():
                if _cmd_read(address, 4) != bytes.fromhex('d15785d2'):
                    raise RuntimeError(f'Ring instruction mismatch: {name}')
                hv.add_hw_bp(address)
                _cmd_active.append(address)
            _ring_emit('armed', entries={k: hex(v) for k, v in _ring_entries.items()}, limit=128)
        except Exception:
            _cmd_disarm('ring-arm-failed')
            raise
    if old_state == 'armed' and _cmd_state == 'disarmed':
        _ring_emit('disarmed', publications=_ring_count, pending=len(_ring_pending))


def _ring_break(ctx):
    global _ring_count
    address = int(ctx.elr)
    if address not in _ring_entries.values() or address not in _cmd_active:
        return _ring_base_break(ctx)
    cpu_id = int(ctx.cpu_id)
    try:
        channel, slot = _cmd_va(int(ctx.regs[19])), int(ctx.regs[24]) & 0xffffffff
        pending = _ring_pending.pop((channel, slot))
        wide = address == _ring_entries['ring64']
        if wide != pending['wide']:
            raise ValueError('Wrapper selected an unexpected ring format')
        entry = int(pending['ring_base'], 16) + slot * 64
        payload = _ring_surface_read(entry, 64)
        _ring_count += 1
        _ring_emit('published', seq=_ring_count, cpu=cpu_id, pc=hex(address),
                   slot_index=slot, entry_address=hex(entry), entry_hex=payload.hex(),
                   current_channel_hex=_cmd_read(channel, 0x30).hex(), **pending)
        if _ring_count >= 128:
            _ring_emit('limit-reached', complete=False)
            _cmd_disarm('ring-limit')
    except Exception as exc:
        _ring_emit('capture-error', error=repr(exc), complete=False)
        _cmd_disarm('ring-error')
    if int(hv.ctx.cpu_id) != cpu_id:
        raise RuntimeError('Ring breakpoint failed to restore its CPU')
    hv.ctx.regs[17] = 0x2abe
    hv.ctx.elr = address + 4
    return True


def _ring_extract_firmware():
    node = hv.adt['/arm-io/ave']
    names = str(node.getprop('segment-names')).split(';')
    raw = bytes(node.getprop('segment-ranges'))
    if names != ['__TEXT', '__DATA'] or len(raw) != 64 or int(node.getprop('pre-loaded')) != 1:
        raise RuntimeError('Unexpected preloaded AVE firmware description')
    # BootArgs.phys_base starts the usable boot allocation, above firmware
    # reserved by iBoot. Use the physical DRAM extent for preloaded segments.
    chosen = u.adt['/chosen']
    memory_base = int(chosen.getprop('dram-base'))
    memory_size = int(chosen.getprop('dram-size'))
    if not (memory_base == hv.ram_base and
            memory_size == int(u.ba.mem_size_actual) == hv.ram_size and
            0 < memory_size <= 64 * 1024**3):
        raise RuntimeError('Physical DRAM description disagrees with hypervisor bounds')
    segments = [struct.unpack_from('<QQQII', raw, index * 32) for index in range(2)]
    for physical, offset, alias, size, flags in segments:
        if not (0 < size <= 8 * 1024**2 and memory_base <= physical and
                physical + size <= memory_base + memory_size and physical % 16384 == 0):
            raise RuntimeError(f'Firmware segment {physical:#x}+{size:#x} is outside '
                               f'physical DRAM {memory_base:#x}+{memory_size:#x}')
    if max(segments[0][0], segments[1][0]) < min(
            segments[0][0] + segments[0][3], segments[1][0] + segments[1][3]):
        raise RuntimeError('Preloaded firmware segments overlap')
    records = []
    for index, name in enumerate(names):
        physical, offset, alias, size, flags = segments[index]
        payload = iface.readmem(physical, size)
        check = iface.readmem(physical, size)
        if len(payload) != size or payload != check:
            raise RuntimeError('Preloaded firmware segment was short or unstable')
        filename = 'ave-preloaded-' + name.removeprefix('__').lower() + '.bin'
        (_cmd_dir / filename).write_bytes(payload)
        records.append({'name': name, 'physical': hex(physical), 'offset': hex(offset),
                        'alias_raw': hex(alias), 'size': size, 'flags_raw': flags,
                        'file': filename, 'sha256': hashlib.sha256(payload).hexdigest()})
    (_cmd_dir / 'ave-preloaded-firmware.json').write_text(json.dumps({
        'source': 'J813 iBoot-preloaded AVE segment-ranges; two matching reads before guest start',
        'dram_base': hex(memory_base), 'dram_size': memory_size,
        'bootargs_phys_base': hex(int(u.ba.phys_base)),
        'segments': records}, indent=2) + '\n')
    print('AVE_PRELOADED_FIRMWARE_SAVED segments=2', flush=True)


_ring_extract_firmware()
hv.j813_ave_command_poll = _ring_poll
hv.handle_break = _ring_break
_ring_emit('waiting')


def _video_poll_status():
    # run_script copies its namespace. Return live values from the observer's
    # own globals instead of a later script's copied phase/state strings.
    _ave_update_phase()
    _ring_poll()
    return dict(phase=_ave_phase, command_state=_cmd_state,
                pending_publications=len(_ring_pending))


hv.j813_video_control = _video_poll_status
