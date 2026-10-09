# SPDX-License-Identifier: MIT
"""Read the existing EL2 translation tables, without accessing target pages."""
import json
import os
from pathlib import Path
import struct
import time

if hv.ctx is None or not hv.started or int(u.adt['/chosen'].chip_id) != 0x8142:
    raise RuntimeError('Pause the existing M5 guest first')
_el2_tcr = u.mrs('TCR_EL2')
_el2_ttbr = u.mrs('TTBR0_EL2')
if ((_el2_tcr >> 14) & 3) != 2 or (_el2_tcr & 63) != 16:
    raise RuntimeError('Expected current carrier 48-bit, 16K EL2 tables')
_el2_mask = ((1 << 50) - 1) & ~0x3fff
_el2_dram = int(u.adt['/chosen'].dram_base)
_el2_end = _el2_dram + int(u.adt['/chosen'].dram_size)

def _el2_walk(va):
    table = _el2_ttbr & _el2_mask
    walk = []
    for shift, bits in ((47, 1), (36, 11), (25, 11), (14, 11)):
        addr = table + ((va >> shift) & ((1 << bits) - 1)) * 8
        if not _el2_dram <= addr < _el2_end - 7:
            raise RuntimeError('EL2 table outside DRAM')
        value, = struct.unpack('<Q', hv.iface.readmem(addr, 8))
        walk.append(dict(address=hex(addr), value=hex(value), shift=shift))
        if not value & 1 or value & 3 != 3 or shift == 14:
            valid = bool(value & 1)
            return dict(walk=walk, valid=valid,
                physical=hex((value & _el2_mask & ~((1 << shift)-1)) |
                             (va & ((1 << shift)-1))) if valid else None,
                mair_index=(value >> 2) & 7 if valid else None)
        table = value & _el2_mask
    raise RuntimeError('Unterminated walk')

_el2_records = []
for _el2_name in ('gpu-region', 'gfx-shared-region', 'gfx-shared-l2-region', 'gfx-handoff'):
    _el2_pa = int(u.adt['/arm-io/sgx'].getprop(_el2_name + '-base'))
    for _el2_alias in (0, 0xa0000000000, 0xe0000000000):
        _el2_records.append(dict(label=_el2_name, alias=hex(_el2_alias),
            target=hex(_el2_pa), **_el2_walk(_el2_pa | _el2_alias)))
_el2_path = Path(os.environ['M5_GPU_TRACE_DIR']) / ('el2-mappings-' + str(time.time_ns()) + '.json')
_el2_path.write_text(json.dumps(dict(ttbr=hex(_el2_ttbr), tcr=hex(_el2_tcr),
    records=_el2_records), indent=2) + '\n')
print('M5_EL2_MAPPING_SNAPSHOT', _el2_path.name)
for _el2_record in _el2_records:
    print(_el2_record['label'], _el2_record['alias'], _el2_record['valid'],
          _el2_record['physical'], _el2_record['mair_index'])
