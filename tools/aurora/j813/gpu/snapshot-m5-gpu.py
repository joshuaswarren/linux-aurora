# SPDX-License-Identifier: MIT
"""Capture M5 GPU initialization roots only from a deliberately paused HV.

The 43-bit UAT walker is shared geometry. Keep the full firmware virtual
address, including its root selector; do not use the G17P object decoder.
RTKit endpoint 1 describes a crashlog buffer, not the initialization root.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import time

from m1n1.agx.g17p_trace import G17PUATReader

if hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3':
    raise RuntimeError('Pause the active M5 guest before requesting a GPU snapshot')
if int(hv.u.adt['/chosen'].chip_id) != 0x8142:
    raise RuntimeError('M5 GPU snapshot requires T8142')
if int(hv.u.adt['/defaults'].uat_vaddr_size) != 43:
    raise RuntimeError('Unexpected M5 UAT virtual-address width')

_m5_output = Path(os.environ['M5_GPU_TRACE_DIR'])
_m5_snapshot = _m5_output / ('gpu-snapshot-' + str(time.time_ns()))
_m5_snapshot.mkdir(mode=0o700)
_m5_low = int(hv.u.adt['/chosen'].dram_base)
_m5_high = _m5_low + int(hv.u.adt['/chosen'].dram_size)


def _m5_read_dram(address, size):
    if not _m5_low <= address < address + size <= _m5_high or size > 0x4000:
        raise ValueError('GPU walker attempted a non-DRAM or oversized read')
    # The pinned carrier's sptm_emul_init maps the complete RAM RW_EL0
    # aperture Normal-NC. Ordinary aliases retain pre-SPTM table contents.
    # This reads that existing mapping; it creates no alias or device map.
    data = hv.iface.readmem(address | 0xa0000000000, size)
    if len(data) != size:
        raise ValueError('Short GPU DRAM read')
    return data


_m5_reader = G17PUATReader(_m5_read_dram, int(hv.u.adt['/arm-io/sgx'].gpu_region_base))
# Firmware kernel mappings use SPTM's separate global root. That root is
# explicitly allocated and reported by the pinned seed_sptm_bootstrap; it
# does not appear in the per-context TTBR pair page on this working M5.
_m5_global_values = re.findall(r'\buat_global_root_pa=(0x[0-9a-f]+)',
                              (_m5_output / 'console.log').read_text())
if len(set(_m5_global_values)) != 1:
    raise RuntimeError('Expected one recorded SPTM global UAT root')
_m5_global_root = int(_m5_global_values[0], 16)
if _m5_global_root & 0x3fff or not _m5_low <= _m5_global_root < _m5_high:
    raise ValueError('Invalid SPTM global UAT root')


def _m5_global_selector(address, size):
    # The shared root has no user-address half. Only the recorded kernel
    # root is supplied to the common walker, and its children are read.
    if address != _m5_reader.gpu_region + 8 or size != 8:
        raise ValueError('SPTM global view requires the kernel root selector')
    return struct.pack('<Q', _m5_global_root | 1)


_m5_global_reader = G17PUATReader(_m5_read_dram, _m5_reader.gpu_region,
                                read_root=_m5_global_selector)
_m5_roots = {}
with (_m5_output / 'gpu-events.jsonl').open() as _m5_stream:
    for _m5_line in _m5_stream:
        _m5_row = json.loads(_m5_line)
        if (_m5_row.get('kind') == 'MAILBOX' and _m5_row.get('direction') == 'tx'
                and _m5_row.get('endpoint') == 0x20):
            _m5_word = int(_m5_row['word0'], 16)
            if (_m5_word >> 48) & 0xff == 0x81:
                _m5_roots[_m5_row['role'].split('/')[-1]] = _m5_word & ((1 << 44) - 1)
if set(_m5_roots) != {'gfx-asc', 'gfx1-asc'}:
    raise RuntimeError('Both actual M5 initialization doorbells must be captured first')

_m5_records = []
for _m5_role, _m5_pointer in _m5_roots.items():
    for _m5_context, _m5_view, _m5_active_reader in [
            (0, 'context0', _m5_reader), (1, 'context1', _m5_reader),
            (2, 'context2', _m5_reader), (0, 'sptm-global', _m5_global_reader)]:
        _m5_record = dict(role=_m5_role, context=_m5_context, view=_m5_view,
                          pointer=hex(_m5_pointer))
        try:
            _m5_physical, _m5_walk = _m5_active_reader.translate(_m5_context, _m5_pointer)
            _m5_data = _m5_active_reader.read(_m5_pointer, 0xc0, context=_m5_context)
            _m5_file = f'{_m5_role}-{_m5_view}-root.bin'
            (_m5_snapshot / _m5_file).write_bytes(_m5_data)
            _m5_record.update(physical=hex(_m5_physical), walk=_m5_walk, file=_m5_file,
                              sha256=hashlib.sha256(_m5_data).hexdigest(),
                              geometry_word=hex(struct.unpack_from('<I', _m5_data, 0x30)[0]))
            if _m5_view == 'sptm-global' and struct.unpack_from('<I', _m5_data, 0x30)[0] == 0x030e4000:
                # Raw captures at the shared G17 header/config offsets.
                # Preserve full kernel VAs and record every walk. These
                # bytes are evidence, not a Linux-ready initialization image.
                _m5_record['objects'] = []
                try:
                    _m5_main_pointer, = struct.unpack_from('<Q', _m5_data, 0x18)
                    _m5_main = _m5_active_reader.read(_m5_main_pointer, 0x600)
                    _m5_hardware_pointer, = struct.unpack_from('<Q', _m5_main)
                    for _m5_label, _m5_address, _m5_size in [
                            ('main-config', _m5_main_pointer, 0x600),
                            ('hardware-data-prefix', _m5_hardware_pointer, 0x4000)]:
                        _m5_object_pa, _m5_object_walk = _m5_active_reader.translate(0, _m5_address)
                        _m5_payload = _m5_active_reader.read(_m5_address, _m5_size)
                        _m5_object_file = f'{_m5_role}-{_m5_label}.bin'
                        (_m5_snapshot / _m5_object_file).write_bytes(_m5_payload)
                        _m5_record['objects'].append(dict(
                            label=_m5_label, pointer=hex(_m5_address), physical=hex(_m5_object_pa),
                            walk=_m5_object_walk, size=_m5_size, file=_m5_object_file,
                            sha256=hashlib.sha256(_m5_payload).hexdigest()))
                except Exception as _m5_object_error:
                    _m5_record['object_error'] = repr(_m5_object_error)
        except Exception as _m5_error:
            _m5_record['error'] = repr(_m5_error)
        _m5_records.append(_m5_record)
(_m5_snapshot / 'manifest.json').write_text(json.dumps(dict(
    source='Paused M5 hypervisor; existing SPTM full-RAM Normal-NC alias', va_bits=43,
    input_root_bits=42, firmware_pointer_masked_to_40_bits=False,
    records=_m5_records), indent=2) + '\n')
print('M5_GPU_ROOT_SNAPSHOT', _m5_snapshot.name,
      'mapped', sum('file' in row for row in _m5_records), 'of', len(_m5_records))
