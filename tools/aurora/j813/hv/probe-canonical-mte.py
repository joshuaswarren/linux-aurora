"""Compare ordinary-memory LDG with canonical tagging off/on, then restore.

Run through the existing paused M5 controller. No guest data, allocation
tags, or page-table entries are written.
"""
import copy
import struct


def probe(hv):
    if hv.ctx is None or int(hv.adt['/chosen'].chip_id) != 0x8142:
        raise RuntimeError('Requires the paused M5 guest controller')
    if hv.ctx.elr != 0xfffffe000b7bbdd0:
        raise RuntimeError('Requires the verified 25G76 panic stop')
    args = struct.unpack('<48Q', hv.readmem(hv.ctx.regs[21], 48 * 8))
    if args[4:6] != (0xfffffe000b99db88, 0xfffffe000b86952c):
        raise RuntimeError('Requires the verified zone retagging panic')
    address = args[43] | (0xf << 56)
    instruction = 0xfffffe000b8d0964
    if hv.readmem(instruction, 4) != bytes.fromhex('290160d9'):
        raise RuntimeError('LDG instruction signature mismatch')
    table = hv.u.mrs('TTBR1_EL12') & 0x0000ffffffffc000
    for shift in (36, 25, 14):
        slot = table + ((address >> shift) & 0x7ff) * 8
        pte = hv.p.read64(slot)
        if pte & 3 != 3:
            raise RuntimeError('Expected a complete 16-KiB page walk')
        table = pte & 0x0000ffffffffc000
    if (pte >> 2) & 7 != 0 or (pte >> 6) & 3 != 2:
        raise RuntimeError('Expected ordinary read-only memory')
    tcr = hv.u.mrs('TCR_EL12')
    saved = copy.deepcopy(hv.ctx)
    state = {name: getattr(hv, name)
             for name in ('exc_reason', 'exc_code', 'is_fault')}
    debug = hv.u.mrs('MDSCR_EL1')
    before = hv.readmem(address, 16)
    results = []
    try:
        for value in (tcr & ~(3 << 60), tcr | (3 << 60)):
            hv.u.msr('TCR_EL12', value)
            hv.u.exec('isb; tlbi vmalle1is; dsb ish; isb')
            if hv.u.mrs('TCR_EL12') != value:
                raise RuntimeError('TCR canonical-tag bits did not read back')
            hv.ctx.elr = instruction
            hv.ctx.regs[9] = address
            hv.step()
            if hv.ctx.elr != instruction + 4:
                raise RuntimeError('LDG did not complete exactly one instruction')
            results.append(hv.ctx.regs[9])
            print('M5_CANONICAL_LDG', hex(value), hex(address), hex(results[-1]))
    finally:
        try:
            hv.u.msr('TCR_EL12', tcr)
            hv.u.exec('isb; tlbi vmalle1is; dsb ish; isb')
        finally:
            hv.ctx = saved
            hv._commit_context()
            hv.u.msr('MDSCR_EL1', debug)
            for name, value in state.items():
                setattr(hv, name, value)
        if hv.p.read64(slot) != pte or hv.readmem(address, 16) != before:
            raise RuntimeError('Read-only canonical-tag probe changed memory')
        if hv.u.mrs('TCR_EL12') != tcr:
            raise RuntimeError('Original TCR was not restored')
        print('M5_CANONICAL_PROBE_RESTORED')
    if results != [address & ~(0xf << 56), address]:
        raise RuntimeError('Canonical-tag hypothesis not confirmed')
    print('M5_CANONICAL_TAGS_PROVEN')


if 'hv' in globals():
    probe(hv)
