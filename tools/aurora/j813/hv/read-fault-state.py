"""Read the pinned M5 kernel's data-abort panic from its paused controller.

No registers, PTEs, allocation tags, or guest data are changed. This decoder
matches the 25G76 panic format verified in guest13, not arbitrary kernels.
"""
import struct

from tools.sptm.seed import _m1n1_symbol_offset


def inspect(hv):
    if hv.ctx is None or int(hv.adt['/chosen'].chip_id) != 0x8142:
        raise RuntimeError('Requires the paused M5 guest controller')
    if hv.ctx.elr != 0xfffffe000b7bbdd0:
        raise RuntimeError(f'Not at the verified XNU panic stop: {hv.ctx.elr:#x}')
    fmt = hv.readmem(hv.ctx.regs[19], 64)
    if not fmt.startswith(b'%s at pc 0x%016llx, lr 0x%016llx'):
        raise RuntimeError('Not the verified 25G76 saved-state panic format')
    args = struct.unpack('<48Q', hv.readmem(hv.ctx.regs[21], 48 * 8))
    reason = hv.readmem(args[3], 64).split(b'\0')[0].decode('ascii')
    print('M5_FAULT', dict(reason=reason, pc=hex(args[4]), lr=hex(args[5]),
                           esr=hex(args[42]), far=hex(args[43]),
                           spsr=hex(args[41])))
    regs = args[8:39]
    print('M5_FAULT_REGS', [(i, hex(value)) for i, value in enumerate(regs)])
    state_ptr = hv.u.base + _m1n1_symbol_offset('g_sptm_state')
    state = hv.p.read64(state_ptr)
    frame_base, first_pa, frame_size = struct.unpack(
        '<3Q', hv.iface.readmem(state + 0x238, 24))
    if not frame_base or not first_pa or frame_size % 16:
        raise RuntimeError('Invalid resident frame-table geometry')
    end_pa = first_pa + frame_size // 16 * 0x4000

    def frame(pa):
        if not first_pa <= pa < end_pa:
            raise RuntimeError('Frame outside resident managed RAM')
        address = frame_base + ((pa - first_pa) >> 14) * 16
        data = hv.iface.readmem(address, 16)
        print('M5_FAULT_FRAME', hex(pa), data.hex(), 'type', data[2])

    table = hv.u.mrs('TTBR1_EL12') & 0x0000ffffffffc000
    for shift in (36, 25, 14):
        frame(table)
        slot = table + ((args[43] >> shift) & 0x7ff) * 8
        value = hv.p.read64(slot)
        print('M5_FAULT_PTE', shift, hex(slot), hex(value),
              'attr', (value >> 2) & 7, 'AP', (value >> 6) & 3)
        if value & 3 != 3:
            raise RuntimeError('Expected a complete 16-KiB page walk')
        table = value & 0x0000ffffffffc000
    frame(table)
    # The verified zalloc retagging call retains the zone pointer in x20.
    if args[5] == 0xfffffe000b86952c and 0xfffffe000c558000 <= regs[20] < 0xfffffe000c9f8000:
        zone = struct.unpack('<16Q', hv.readmem(regs[20], 128))
        print('M5_FAULT_ZONE_WORDS', [(i * 8, hex(v)) for i, v in enumerate(zone)])
        for offset, value in enumerate(zone):
            if 0xfffffe0007000000 <= value < 0xfffffe0008950000:
                text = hv.readmem(value, 96).split(b'\0')[0]
                if text and all(32 <= c < 127 for c in text):
                    print('M5_FAULT_ZONE_STRING', offset * 8, text.decode('ascii'))


if 'hv' in globals():
    inspect(hv)
