"""Read cached NVMe emulator state through the existing paused M5 controller.

This reads resident RAM only. It does not access NVMe MMIO or ring buffers.
The prefix layout is derived from the matching carrier's C declaration.
"""
import ctypes
from pathlib import Path
import re
from tools.sptm.seed import _m1n1_symbol_offset


def nvme_prefix_type():
    source = (Path(__file__).resolve().parent /
              'm1n1/src/sptm/private.h').read_text()
    prefix = source.split('struct nvme_c_state {', 1)[1].split('u64 last[16];', 1)[0]
    types = dict(u8=ctypes.c_uint8, u16=ctypes.c_uint16,
                 u32=ctypes.c_uint32, u64=ctypes.c_uint64)
    fields = []
    for kind, name, length in re.findall(
            r'\b(u8|u16|u32|u64)\s+(\w+)(?:\[(\d+)\])?;', prefix):
        item = types[kind]
        fields.append((name, item * int(length) if length else item))
    result = type('NVMePrefix', (ctypes.LittleEndianStructure,), {'_fields_': fields})
    if ctypes.sizeof(result) != 192 or result.aqa_cached.offset != 136:
        raise RuntimeError('NVMe resident prefix changed; requalify the reader')
    return result


def read_nvme_state(hv):
    if int(hv.u.adt['/chosen'].chip_id) != 0x8142 or hv.ctx is None:
        raise RuntimeError('Requires the paused M5 guest controller')
    state = hv.p.read64(hv.u.base + _m1n1_symbol_offset('g_sptm_state'))
    if not state or state & 0x3fff:
        raise RuntimeError('Invalid resident SPTM state pointer')
    prefix = nvme_prefix_type()
    values = prefix.from_buffer_copy(hv.iface.readmem(state + 0x14570, ctypes.sizeof(prefix)))
    print('M5_NVME_STATE', {name: getattr(values, name) for name in (
        'enabled', 'queue_count', 'max_cid', 'violations', 'total_calls',
        'fast_path_calls', 'fallthrough_calls', 'last_rc')})
    print('M5_NVME_CACHED_QUEUES', {name: hex(getattr(values, name)) for name in (
        'bar_pa', 'aqa_cached', 'ioqa_cached', 'asq_pa_cached', 'acq_pa_cached',
        'iosq_pa_cached', 'iocq_pa_cached')})


if 'hv' in globals():
    read_nvme_state(hv)
