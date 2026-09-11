# SPDX-License-Identifier: MIT
"""Bounded M5 register observations in one host-owned proxy call.

No guest or firmware executable is patched. The helper and result buffer
belong to the existing hypervisor's reserved heap. The only SGX stores are
read tags to AP port9, the path verified by Linux diagnostic mode10.
"""
import hashlib
import struct

from m1n1.asm import ARMAsm
from m1n1.proxy import GUARD


FIELDS = [
    'gpx_before', 'gfx_before', 'counter_before', 'counter_after', 'frequency',
    'gpx_after', 'gfx_after', 'slot_before', 'slot_after',
    'queue_before', 'queue_after', 'requested_queue',
    'slot_state_before', 'slot_progress', 'pipe_1688', 'pipe_a730', 'pipe_a0a8',
    'slot_state_after', 'queue_enable', 'queue_stamp', 'queue_completed',
    'event0', 'event1', 'event2', 'event3',
    'mask_c0b0', 'mask_c0c8', 'mask_c0d8', 'active_c128',
    'dm0_c160', 'dm1_c168', 'dm2_c170', 'command_pointer', 'queue_pointer',
    'slot_timestamp', 'pipe_index', 'flags',
]
SIZE = len(FIELDS) * 8
SGX = 0x580000000


def source():
    """AAPCS leaf: x0=SGX, x1=result, x2=queue. Uses only x0..x17."""
    code = []

    def emit(*lines):
        code.extend(lines)

    def save(reg, field):
        emit(f'str {reg}, [x1, #{FIELDS.index(field) * 8}]')

    def direct(offset, field, width=64):
        emit(f'ldr x10, ={offset:#x}', f'ldr {"w11" if width == 32 else "x11"}, [x0, x10]')
        save('x11', field)

    def port(tag, field, index=None):
        emit(f'ldr x10, ={tag:#x}')
        if index is not None:
            emit(f'orr x10, x10, {index}, lsl #44')
        emit('str x10, [x12]', 'ldr x11, [x12, #8]')
        save('x11', field)

    emit('mov x9, #0', 'mrs x11, cntpct_el0')
    save('x11', 'counter_before')
    emit('mrs x11, cntfrq_el0')
    save('x11', 'frequency')
    save('x2', 'requested_queue')
    direct(0xe80000, 'gpx_before', 32)
    emit('and w11, w11, #0xff', 'cmp w11, #0xff', 'b.ne finish')
    direct(0xe80110, 'gfx_before', 32)
    emit('and w11, w11, #0xff', 'cmp w11, #0xff', 'b.ne finish', 'orr x9, x9, #1')
    direct(0x10398, 'slot_before', 32)
    emit('mov x13, x11')
    direct(0x103a8, 'queue_before')
    emit('cmp x11, x2', 'b.ne no_queue', 'orr x9, x9, #4')
    for offset, name in [(0x21008, 'queue_enable'), (0x21020, 'queue_stamp'),
                         (0x21028, 'queue_completed')]:
        direct(offset, name)
    emit('no_queue:')
    for ordinal in range(4):
        direct(0x21168 + ordinal * 8, f'event{ordinal}')
    for offset, name in [(0xc0b0, 'mask_c0b0'), (0xc0c8, 'mask_c0c8'),
                         (0xc0d8, 'mask_c0d8'), (0xc128, 'active_c128'),
                         (0xc160, 'dm0_c160'), (0xc168, 'dm1_c168'), (0xc170, 'dm2_c170')]:
        direct(offset, name)
    emit('cmp x13, #8', 'b.hs skip_slot', 'ldr x12, =0x4090', 'add x12, x0, x12')
    port((1 << 43) | 0xc020, 'slot_state_before', 'x13')
    emit('mov x14, x11', 'orr x9, x9, #2')
    for offset, name in [(0xc0f0, 'command_pointer'), (0xc0f8, 'queue_pointer'),
                         (0xc028, 'slot_timestamp'), (0xc078, 'slot_progress')]:
        direct(offset, name)
    emit('tbz x9, #2, no_slot', 'ubfx x15, x11, #40, #7', 'cmp x15, x2', 'b.ne no_slot',
         'tbz x14, #0, no_slot', 'and x15, x14, #0x3c', 'cmp x15, #0xc', 'b.ne no_slot',
         'ubfx x15, x14, #16, #3', 'cbz x15, no_slot',
         'sub x16, x15, #1', 'tst x15, x16', 'b.ne no_slot',
         'clz x17, x15', 'mov x16, #63', 'sub x17, x16, x17',
         'orr x9, x9, #8')
    save('x17', 'pipe_index')
    port(0x1688, 'pipe_1688', 'x17')
    port(0xa730, 'pipe_a730', 'x17')
    # Exact M5 DM2 table44990: selector a010, value3, register a0a8.
    # Firmware7c68..7c94 packs the selector into the read tag; it does not
    # write a010 here. Follow that path with the already qualified pipe.
    port((1 << 43) | (3 << 40) | 0xa0a8, 'pipe_a0a8', 'x17')
    emit('no_slot:')
    port((1 << 43) | 0xc020, 'slot_state_after', 'x13')
    emit('cmp x14, x11', 'b.ne state_changed', 'orr x9, x9, #32', 'state_changed:', 'skip_slot:')
    direct(0x10398, 'slot_after', 32)
    emit('mov x16, x11')
    direct(0x103a8, 'queue_after')
    emit('cmp x16, x13', 'b.ne selection_changed',
         f'ldr x15, [x1, #{FIELDS.index("queue_before") * 8}]',
         'cmp x11, x15', 'b.eq selection_stable', 'selection_changed:',
         'and x9, x9, #0xfffffffffffffff1', 'selection_stable:')
    direct(0xe80000, 'gpx_after', 32)
    emit('and w11, w11, #0xff', 'cmp w11, #0xff', 'b.ne finish')
    direct(0xe80110, 'gfx_after', 32)
    emit('and w11, w11, #0xff', 'cmp w11, #0xff', 'b.ne finish', 'orr x9, x9, #16')
    emit('finish:', 'mrs x11, cntpct_el0')
    save('x11', 'counter_after')
    save('x9', 'flags')
    emit('dsb sy', 'mov x0, x9', 'ret')
    return '\n'.join(code)


def decode(raw):
    if len(raw) != SIZE:
        raise ValueError('Short M5 monitor result')
    values = dict(zip(FIELDS, struct.unpack('<' + 'Q' * len(FIELDS), raw)))
    flags = values['flags']
    powered = flags & 0x11 == 0x11
    return dict(format_version=3, values={key: hex(value) for key, value in values.items()},
                powered=powered, slot_view_valid=powered and bool(flags & 2),
                queue_view_valid=powered and bool(flags & 4),
                compute_monitor_valid=powered and flags & 0x2e == 0x2e,
                elapsed_us=((values['counter_after'] - values['counter_before']) * 1e6
                            / values['frequency'] if values['frequency'] else None),
                atomic=False)


class M5NativeMonitor:
    def __init__(self, hv):
        if (hv.u.adt.model != 'Mac17,3' or int(hv.u.adt['/chosen'].chip_id) != 0x8142
                or hv.u.adt['/arm-io/sgx'].get_reg(0)[0] != SGX):
            raise ValueError('Expected M5 native monitor profile')
        self.hv = hv
        self.code = hv.u.memalign(0x4000, 0x4000)
        self.result = hv.u.memalign(0x4000, 0x4000)
        binary = ARMAsm(source(), self.code).data
        if len(binary) > 0x4000:
            raise ValueError('M5 monitor helper exceeds its code page')
        hv.iface.writemem(self.code, binary)
        if hv.iface.readmem(self.code, len(binary)) != binary:
            raise ValueError('M5 monitor helper readback mismatch')
        hv.p.dc_cvau(self.code, len(binary))
        hv.p.ic_ivau(self.code, len(binary))
        self.digest = hashlib.sha256(binary).hexdigest()

    def capture(self, queue):
        if self.hv.ctx is None or not self.hv.started or not 0 <= queue < 128:
            raise ValueError('Expected existing paused M5 owner and valid queue')
        hv = self.hv
        hv.iface.writemem(self.result, bytes(SIZE))
        call, region = hv.u.exec_modes[None]
        hv.p.set_exc_guard(GUARD.SKIP | GUARD.SILENT)
        try:
            returned = call(self.code | region, SGX, self.result, queue, 0)
            if hv.p.get_exc_count():
                raise RuntimeError('M5 monitor helper encountered an exception')
        finally:
            hv.p.set_exc_guard(GUARD.OFF)
        result = decode(hv.iface.readmem(self.result, SIZE))
        if int(result['values']['flags'], 16) != returned:
            raise RuntimeError('M5 monitor result/return disagreement')
        result['helper_sha256'] = self.digest
        return result
