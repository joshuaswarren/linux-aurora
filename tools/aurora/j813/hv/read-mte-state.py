"""Read MTE emulator counters through the existing paused M5 controller."""
import struct
from tools.sptm.seed import _m1n1_symbol_offset


def read_mte_state(hv):
    if int(hv.u.adt['/chosen'].chip_id) != 0x8142 or hv.ctx is None:
        raise RuntimeError('Requires the paused M5 guest controller')
    address = hv.u.base + _m1n1_symbol_offset('g_sptm_mte')
    values = struct.unpack('<7Q', hv.iface.readmem(address, 56))
    names = ('dram_base', 'storage_base', 'storage_end', 'tag_calls',
             'untag_calls', 'tagged_pages', 'rejected')
    print('M5_MTE_STATE', dict(zip(names, values)))
    print('M5_MTE_STOP', hex(hv.ctx.elr), str(hv.ctx.esr), hex(hv.ctx.regs[16]))


if 'hv' in globals():
    read_mte_state(hv)
