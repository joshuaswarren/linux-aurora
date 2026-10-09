"""Load the qualified MMIO survey and a bounded AVE firmware send observer.

Arm/disarm via ave-command-control.json during capture-xnu's paused diagnostics.
Uses hardware execution breakpoints, leaving guest instructions and data intact.
Only copies the command argument and a small IPC object header, never MMIO.
"""
import hashlib
import json
import os
from pathlib import Path
import struct
import time

_cmd_source = Path(__file__).resolve().parent
exec(compile((_cmd_source / 'trace-j700-ave.py').read_text(),
             str(_cmd_source / 'trace-j700-ave.py'), 'exec'), globals())
_cmd_dir = _ave_dir
_cmd_log = (_cmd_dir / 'ave-commands.jsonl').open('x', buffering=1)
_cmd_buffers = _cmd_dir / 'ave-command-buffers'
_cmd_buffers.mkdir()
_cmd_control = _cmd_dir / 'ave-command-control.json'
_cmd_active = []
_cmd_state = 'waiting'
_cmd_count = 0
_cmd_limit = 64
# HEVC Start in the verified 25G76 driver is 0x13f28 bytes.
_cmd_max_size = 131072
_cmd_entries = {
    'ipc-send': 0xfffffe0008b17dbc,      # mov x22, x3
    'channel-send': 0xfffffe0008a227f0,  # mov x8, x0
}
_cmd_original_break = hv.handle_break
_cmd_repeat_signature = None
_cmd_repeat_count = 0
_cmd_capture_inputs = os.environ.get('J700_AVE_CAPTURE_INPUTS', '0') == '1'
_cmd_input_seen = {}
_cmd_input_dart = None


def _cmd_capture_input_planes(data, command_sequence):
    """Read at most two Process inputs per phase from existing guest mappings."""
    global _cmd_input_dart
    if not _cmd_capture_inputs or len(data) != 0xb1c0 or struct.unpack_from('<I', data)[0] != 8:
        return
    count = _cmd_input_seen.get(_ave_phase, 0)
    if count >= 2:
        return
    _cmd_input_seen[_ave_phase] = count + 1
    try:
        from m1n1.hw.dart import DART
        if _cmd_input_dart is None:
            _cmd_input_dart = DART.from_adt(hv.u, '/arm-io/dart-ave')
        _cmd_input_dart.dart.invalidate_cache()
        for plane, slot in enumerate((0xa300, 0xa320, 0xa340, 0xa360)):
            address, size, stride = struct.unpack_from('<QII', data, slot)
            if not address and not size and not stride:
                continue
            if not (0x10000000000 <= address and 0 < size <= 1024*1024 and 0 < stride <= 8192):
                raise ValueError('AVE input plane exceeds observed bounded layout')
            ranges = _cmd_input_dart.iotranslate(0, address, size)
            if any(pa is None or not hv.ram_base <= pa < pa+length <= hv.ram_base+hv.ram_size for pa,length in ranges):
                raise ValueError('AVE input plane translation is outside physical RAM')
            raw = b''.join(iface.readmem(pa,length) for pa,length in ranges)
            if len(raw) != size:
                raise ValueError('Short AVE input DMA snapshot')
            filename = f'{command_sequence:04d}-input-{plane}.bin'
            (_cmd_buffers / filename).write_bytes(raw)
            _cmd_emit('input-plane', command_seq=command_sequence, plane=plane, slot=hex(slot),
                iova=hex(address), size=size, stride=stride, stream=0, file=filename,
                sha256=hashlib.sha256(raw).hexdigest())
    except Exception as exc:
        _cmd_emit('input-capture-error', command_seq=command_sequence, error=repr(exc), complete=False)


def _cmd_emit(kind, **data):
    _cmd_log.write(json.dumps({'kind': kind, 'host_monotonic_ns': time.monotonic_ns(),
                               'phase': _ave_phase, **data}) + '\n')


def _cmd_read(va, size):
    # Kernel argument addresses only; hv.readmem translates guest virtual RAM.
    if not 0xfffffe0000000000 <= va < 0xffffffffffffffff - size:
        raise ValueError('Command capture argument is outside kernel virtual memory')
    data = hv.readmem(va, size)
    if len(data) != size:
        raise ValueError(f'Short virtual read: {len(data)}/{size}')
    return data


def _cmd_disarm(reason):
    global _cmd_state
    for addr in list(_cmd_active):
        hv.remove_hw_bp(addr)
        _cmd_active.remove(addr)
    _cmd_state = 'disarmed'
    _cmd_emit('disarmed', reason=reason, events=_cmd_count)


def _cmd_hook(name):
    def hit(ctx):
        global _cmd_count, _cmd_state, _cmd_repeat_signature, _cmd_repeat_count
        if _cmd_state != 'armed':
            return True
        try:
            _ave_update_phase()
            _cmd_count += 1
            regs = [int(x) for x in ctx.regs]
            signature = (name, tuple(regs[:4]))
            _cmd_repeat_count = _cmd_repeat_count + 1 if signature == _cmd_repeat_signature else 1
            _cmd_repeat_signature = signature
            if _cmd_repeat_count >= 3:
                raise RuntimeError('Repeated identical breakpoint entry without channel progress')
            row = {'seq': _cmd_count, 'function': name, 'cpu': int(ctx.cpu_id),
                   'pc': hex(int(ctx.elr)), 'lr': hex(regs[30]),
                   'args': [hex(x) for x in regs[:4]]}
            if name == 'ipc-send':
                size = regs[3] & 0xffffffff
                if not 0 < size <= _cmd_max_size:
                    raise ValueError(f'Unexpected AVE command size: {size}')
                data = _cmd_read(regs[2], size)
                header = _cmd_read(regs[0], 0x40)
                surface_va = struct.unpack_from('<Q', header, 0x28)[0]
                surface = _cmd_read(surface_va, 0xf2)
                kernel_base = struct.unpack_from('<Q', surface, 0x98)[0]
                firmware_base = struct.unpack_from('<Q', header, 0x38)[0]
                surface_size = struct.unpack_from('<I', surface, 0xe0)[0]
                valid_surface = bool(surface[0xf1] & 8) and bool(
                    struct.unpack_from('<Q', surface, 0x68)[0])
                if not (valid_surface and kernel_base <= regs[2] and
                        regs[2] + size <= kernel_base + surface_size):
                    raise ValueError('Command is outside the live IPC surface')
                filename = f'{_cmd_count:04d}-ipc-send.bin'
                (_cmd_buffers / filename).write_bytes(data)
                row.update(channel=regs[1] & 0xffffffff, length=size,
                           buffer=filename, sha256=hashlib.sha256(data).hexdigest(),
                           opcode=struct.unpack_from('<I', data)[0] if size >= 4 else None,
                           ipc_header_hex=header.hex(),
                           surface_header_hex=surface.hex(),
                           surface_kernel_base=hex(kernel_base),
                           surface_firmware_base=hex(firmware_base),
                           surface_size=surface_size,
                           expected_firmware_address=hex(firmware_base + regs[2] - kernel_base))
            else:
                row.update(firmware_address=hex(regs[1]), length=regs[2] & 0xffffffff,
                           flags=regs[3] & 0xffffffff)
            _cmd_emit('command', **row)
            if name == 'ipc-send' and row['channel'] == 1:
                _cmd_capture_input_planes(data, _cmd_count)
            if _cmd_count >= _cmd_limit:
                _cmd_state = 'limit'
                _cmd_emit('limit-reached', complete=False)
        except Exception as exc:
            _cmd_state = 'error'
            _cmd_emit('capture-error', error=repr(exc), complete=False)
        if _cmd_state in ('error', 'limit'):
            _cmd_disarm(_cmd_state)
        return True
    return hit


def _cmd_poll():
    global _cmd_state
    if _cmd_state in ('error', 'limit'):
        _cmd_disarm(_cmd_state)
        return
    try:
        request = json.loads(_cmd_control.read_text())
    except FileNotFoundError:
        return
    if request.get('op') == 'disarm' and _cmd_active:
        _cmd_disarm('requested')
    elif request.get('op') == 'arm' and _cmd_state == 'waiting':
        # Validate exact 25G76 entry instructions before setting breakpoints.
        expected = {'ipc-send': bytes.fromhex('f60303aa'),
                    'channel-send': bytes.fromhex('e80300aa')}
        for name, addr in _cmd_entries.items():
            if _cmd_read(addr, 4) != expected[name]:
                raise RuntimeError(f'AVE entry mismatch at {name}; refusing breakpoint')
        try:
            for name, addr in _cmd_entries.items():
                hv.add_hw_bp(addr, hook=_cmd_hook(name))
                _cmd_active.append(addr)
        except Exception:
            _cmd_disarm('arm-failed')
            raise
        _cmd_state = 'armed'
        _cmd_emit('armed', entries={k: hex(v) for k, v in _cmd_entries.items()},
                  event_limit=_cmd_limit, max_command_bytes=_cmd_max_size)
        print('J700_AVE_COMMAND_CAPTURE_ARMED', flush=True)


def _cmd_handle_break(ctx):
    address = int(ctx.elr)
    if address not in _cmd_active:
        return _cmd_original_break(ctx)
    name = next(k for k, v in _cmd_entries.items() if v == address)
    cpu_id = int(ctx.cpu_id)
    source_value = int(ctx.regs[3] if name == 'ipc-send' else ctx.regs[0])
    _cmd_hook(name)(ctx)
    # Execute the exact validated register move in the saved context, then
    # advance. This avoids the unqualified hardware single-step path, which
    # repeatedly hit the same entry in the first capture. Neither instruction
    # changes flags, memory, or SP. All other breakpoints keep normal behavior.
    # remove_hw_bp may switch CPUs and reload the saved context on a limit.
    # Apply the move to the current context, not the stale pre-switch object.
    if int(hv.ctx.cpu_id) != cpu_id:
        raise RuntimeError('AVE breakpoint removal failed to restore its CPU')
    hv.ctx.regs[22 if name == 'ipc-send' else 8] = source_value
    hv.ctx.elr = address + 4
    return True


hv.handle_break = _cmd_handle_break
hv.j700_ave_command_poll = _cmd_poll
_cmd_emit('waiting', instruction='Set ave-command-control.json op to arm after guest boot')
