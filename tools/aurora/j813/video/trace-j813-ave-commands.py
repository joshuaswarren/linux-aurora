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
exec(compile((_cmd_source / 'trace-j813-ave.py').read_text(),
             str(_cmd_source / 'trace-j813-ave.py'), 'exec'), globals())
_cmd_dir = _ave_dir
_cmd_log = (_cmd_dir / 'ave-commands.jsonl').open('x', buffering=1)
_cmd_buffers = _cmd_dir / 'ave-command-buffers'
_cmd_buffers.mkdir()
_cmd_control = _cmd_dir / 'ave-command-control.json'
_cmd_active = []
_cmd_state = 'waiting'
_cmd_count = 0
_cmd_limit = 64
# Bounded first-capture envelope; M5 command sizes are evidence, not assumed.
_cmd_max_size = 131072
_cmd_entries = {
    'ipc-send': 0xfffffe0008c5638c,      # mov x22, x3
    'channel-send': 0xfffffe0008b60dc0,  # mov x8, x0
}
_cmd_original_break = hv.handle_break
_cmd_repeat_signature = None
_cmd_repeat_count = 0
# Input-plane offsets require their own M5 command-layout qualification.
if os.environ.get('M5_AVE_CAPTURE_INPUTS', '0') != '0':
    raise RuntimeError('M5 input-plane capture is not yet qualified')


def _cmd_emit(kind, **data):
    _cmd_log.write(json.dumps({'kind': kind, 'host_monotonic_ns': time.monotonic_ns(),
                               'phase': _ave_phase, **data}) + '\n')


def _cmd_va(va):
    # M5 allocation-tagged kernel pointers can have a non-FF top byte. Strip
    # only that TBI byte; PAC-contaminated or userspace addresses still fail.
    if not 0 <= va < 1 << 64:
        raise ValueError('Invalid 64-bit guest pointer')
    va |= 0xff00000000000000
    if not 0xfffffe0000000000 <= va < 0xffffffffffffffff:
        raise ValueError('Command capture argument is outside kernel virtual memory')
    return va


def _cmd_read(va, size):
    va = _cmd_va(va)
    if not 0 < size <= _cmd_max_size:
        raise ValueError('Invalid bounded guest read length')
    if va + size > 0xffffffffffffffff:
        raise ValueError('Guest read wraps the virtual address space')
    parts = []
    while size:
        length = min(size, 0x4000 - (va & 0x3fff))
        pa = int(hv.p.hv_translate(va, False, False))
        if not hv.ram_base <= pa < pa + length <= hv.ram_base + hv.ram_size:
            raise ValueError('Guest command pointer does not translate to physical DRAM')
        data = iface.readmem(pa, length)
        if len(data) != length:
            raise ValueError('Short physical DRAM read')
        parts.append(data)
        va += length
        size -= length
    return b''.join(parts)


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
                kernel_base = _cmd_va(struct.unpack_from('<Q', surface, 0x98)[0])
                buffer_va = _cmd_va(regs[2])
                firmware_base = struct.unpack_from('<Q', header, 0x38)[0]
                surface_size = struct.unpack_from('<I', surface, 0xe0)[0]
                valid_surface = bool(surface[0xf1] & 8) and bool(
                    struct.unpack_from('<Q', surface, 0x68)[0])
                if not (valid_surface and 0 < surface_size <= 64 * 1024**2 and
                        kernel_base <= buffer_va and
                        buffer_va + size <= kernel_base + surface_size):
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
                           expected_firmware_address=hex(firmware_base + buffer_va - kernel_base))
            else:
                row.update(firmware_address=hex(regs[1]), length=regs[2] & 0xffffffff,
                           flags=regs[3] & 0xffffffff)
            _cmd_emit('command', **row)
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
        print('M5_AVE_COMMAND_CAPTURE_ARMED', flush=True)


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
hv.j813_ave_command_poll = _cmd_poll
_cmd_emit('waiting', instruction='Set ave-command-control.json op to arm after guest boot')
