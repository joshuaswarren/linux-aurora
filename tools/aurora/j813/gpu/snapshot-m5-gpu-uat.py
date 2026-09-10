# SPDX-License-Identifier: MIT
"""Capture M5 UAT reservations and root tables in the existing paused guest.

Only bounded DRAM reads. This does not access GPU registers, change page
tables, start firmware, or infer absent-handoff support from zero bytes.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import time

from m1n1.proxy import UartRemoteError

if (hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3' or
        int(hv.u.adt['/chosen'].chip_id) != 0x8142):
    raise RuntimeError('Pause the existing M5 guest before reading UAT state')
_uat_out = Path(os.environ['M5_GPU_TRACE_DIR'])
_uat_roots = re.findall(r'\buat_global_root_pa=(0x[0-9a-f]+)',
                       (_uat_out / 'console.log').read_text())
if len(set(_uat_roots)) != 1:
    raise RuntimeError('Expected one current-boot global root receipt')
if '_el2_walk' not in globals():
    raise RuntimeError('Inspect the current EL2 mappings first')

def _uat_read_existing(pa, size):
    # Reservation pages outside usable RAM need not have the generic NC
    # alias. Use only an existing, identity-matched Normal-memory mapping;
    # never create a conflicting cache alias or read Device memory.
    if size != 0x4000 or pa & 0x3fff or not _el2_dram <= pa <= _el2_end - size:
        raise RuntimeError('Expected one aligned DRAM page')
    for alias in (0xe0000000000, 0xa0000000000, 0):
        mapping = _el2_walk(pa | alias)
        if (mapping['valid'] and int(mapping['physical'], 16) == pa and
                mapping['mair_index'] in (0, 1)):
            return hv.iface.readmem(pa | alias, size), dict(
                alias=hex(alias), mair_index=mapping['mair_index'])
    raise RuntimeError('No existing Normal-memory alias for reservation')
_uat_sgx = hv.u.adt['/arm-io/sgx']
_uat_regions = {name: [int(_uat_sgx.getprop(name + suffix)) for suffix in ('-base', '-size')]
    for name in ('gpu-region', 'gfx-shared-region', 'gfx-shared-l2-region',
                 'gfx-handoff', 'gfx-data', 'gfx1-data')}
_uat_snapshot = _uat_out / ('runtime-uat-' + str(time.time_ns()))
_uat_snapshot.mkdir(mode=0o700, exist_ok=False)
_uat_records = []
_uat_reads = [('ttbat', _uat_regions['gpu-region'][0], 0x4000),
              ('primary-private-root', _uat_regions['gfx-shared-region'][0], 0x4000),
              ('secondary-private-root', _uat_regions['gfx-shared-region'][0] + 0x40000, 0x4000),
              ('shared-l2-root', _uat_regions['gfx-shared-l2-region'][0], 0x4000),
              ('handoff-page', _uat_regions['gfx-handoff'][0], 0x4000),
              ('sptm-global-root', int(_uat_roots[0], 16), 0x4000)]
for _name, _pa, _size in _uat_reads:
    try:
        _data, _mapping = _uat_read_existing(_pa, _size)
        _repeat, _repeat_mapping = _uat_read_existing(_pa, _size)
        _stable = _data == _repeat and _mapping == _repeat_mapping
    except UartRemoteError as _error:
        # Preserve a failed observation, never a zero stand-in. An alias
        # translation fault alone does not prove physical access protection.
        _uat_records.append(dict(label=_name, physical=hex(_pa), size=_size,
                                  read_error=str(_error)))
        continue
    (_uat_snapshot / (_name + '.bin')).write_bytes(_data)
    _uat_records.append(dict(label=_name, physical=hex(_pa), size=_size,
        stable=_stable, host_mapping=_mapping, sha256=hashlib.sha256(_data).hexdigest(),
        first_entries=[hex(value) for value in struct.unpack_from('<16Q', _data)]))
(_uat_snapshot / 'manifest.json').write_text(json.dumps(dict(
    phase='runtime-paused-guest', regions=_uat_regions, objects=_uat_records,
    device_memory_read=False, uat_input_bits_per_root=42), indent=2) + '\n')
print('M5_GPU_UAT_SNAPSHOT', _uat_snapshot.name)
