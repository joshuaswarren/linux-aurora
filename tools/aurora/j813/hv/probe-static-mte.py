"""Compare LDG through ordinary and tagged mappings on a paused M5 guest.

The one PTE, allocation tag and debugger context are restored before returning.
Only architectural tag instructions are used; guest data and tag-storage RAM
are never written directly. Load through the existing controller only.
"""
import copy


def probe(hv):
    address = 0xfffffe000c71e500
    instruction = 0xfffffe000b8d0964
    store_instruction = 0xfffffe000b8685f8
    if hv.ctx is None or int(hv.adt['/chosen'].chip_id) != 0x8142:
        raise RuntimeError('Requires the paused M5 guest controller')
    if hv.readmem(instruction, 4) != bytes.fromhex('290160d9'):
        raise RuntimeError('M5 LDG instruction signature mismatch')
    if hv.readmem(store_instruction, 4) != bytes.fromhex('291520d9'):
        raise RuntimeError('M5 STG instruction signature mismatch')
    table = hv.u.mrs('TTBR1_EL12') & 0x0000ffffffffc000
    for shift in (36, 25, 14):
        slot = table + ((address >> shift) & 0x7ff) * 8
        original = hv.p.read64(slot)
        if original & 3 != 3:
            raise RuntimeError('Expected a complete 16-KiB page-table walk')
        table = original & 0x0000ffffffffc000
    if (original >> 2) & 7:
        raise RuntimeError('Expected the ordinary kernel mapping')
    saved = copy.deepcopy(hv.ctx)
    saved_exception = {name: getattr(hv, name)
                       for name in ('exc_reason', 'exc_code', 'is_fault')}
    debug = hv.u.mrs('MDSCR_EL1')
    before = hv.readmem(address, 16)
    old_tagged = None

    def replace(value):
        for entry in (0, value):
            hv.p.write64(slot, entry)
            hv.p.dc_civac(slot, 8)
            hv.u.exec('dsb ish; tlbi vmalle1is; dsb ish; isb')

    def step(pc, value):
        hv.ctx.elr = pc
        hv.ctx.regs[9] = value
        hv.step()
        if hv.ctx.elr != pc + 4:
            raise RuntimeError('Tag operation did not complete one instruction')
        return hv.ctx.regs[9]

    try:
        replace(original | (4 << 2))
        old_tagged = step(instruction, address)
        print('M5_STATIC_TAGGED_LDG', hex(old_tagged))
        step(store_instruction, address)
        result = step(instruction, address)
        print('M5_STATIC_TAG_INITIALIZED_LDG', hex(result))
        if result != address:
            raise RuntimeError('Static default-tag pointer did not round trip')
    finally:
        try:
            if old_tagged is not None:
                step(store_instruction, old_tagged)
                if step(instruction, address) != old_tagged:
                    raise RuntimeError('Original allocation tag was not restored')
        finally:
            try:
                replace(original)
            finally:
                hv.ctx = saved
                hv._commit_context()
                hv.u.msr('MDSCR_EL1', debug)
                for name, value in saved_exception.items():
                    setattr(hv, name, value)
        if hv.p.read64(slot) != original or hv.readmem(address, 16) != before:
            raise RuntimeError('M5 static-tag probe restoration check failed')
        print('M5_STATIC_TAG_PROBE_RESTORED')


if 'hv' in globals():
    probe(hv)
