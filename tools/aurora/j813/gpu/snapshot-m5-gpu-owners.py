# SPDX-License-Identifier: MIT
"""Inspect external memory and I/O PTEs through the existing paused M5 owner."""
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import time

from m5_gpu_capture import M5GPUCapture
from m1n1.agx.g17p_trace import G17PUATReader

if (hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3' or
        int(hv.u.adt['/chosen'].chip_id) != 0x8142):
    raise RuntimeError('Pause the existing M5 guest before inspecting its owners')
_owners_base = Path(os.environ['M5_GPU_TRACE_DIR'])
_owners_roots = re.findall(r'\buat_global_root_pa=(0x[0-9a-f]+)',
                          (_owners_base / 'console.log').read_text())
if len(set(_owners_roots)) != 1:
    raise RuntimeError('Expected one current-boot global-root receipt')
_owners_reader = M5GPUCapture(hv.iface.readmem,
    (int(hv.u.adt['/chosen'].dram_base), int(hv.u.adt['/chosen'].dram_size)),
    int(_owners_roots[0], 16))
_owners_snapshot = _owners_base / ('runtime-owners-' + str(time.time_ns()))
_owners_snapshot.mkdir(mode=0o700, exist_ok=False)
_owners_main = (_owners_base / 'preinit/gfx-asc/main-config.bin').read_bytes()
_owners_hw = (_owners_base / 'preinit/gfx-asc/shared-cluster.bin').read_bytes()
_owners_status = (_owners_base / 'preinit/gfx-asc/status-b.bin').read_bytes()
_owners_word = lambda data, at: struct.unpack_from('<Q', data, at)[0]
_owners_records = []
for _label, _va in [
        ('sentinel-page', _owners_word(_owners_main, 0x2d0)),
        ('pb-descriptor-page', _owners_word(_owners_main, 0x2e0)),
        ('uma-descriptor-page', _owners_word(_owners_main, 0x2f0)),
        ('fwctl-page', _owners_word(_owners_status, 0x48e0))]:
    _record = dict(label=_label, pointer=hex(_va), phase='runtime-paused-guest')
    try:
        _pa, _walk = _owners_reader.walker.translate(0, _va)
        _record.update(physical=hex(_pa), walk=_walk)
        _data = _owners_reader.read(_va, 0x4000)
        if _data != _owners_reader.read(_va, 0x4000):
            raise ValueError('Owner view changed during capture: ' + _label)
        (_owners_snapshot / (_label + '.bin')).write_bytes(_data)
        _record.update(size=len(_data), sha256=hashlib.sha256(_data).hexdigest())
    except RuntimeError as _error:
        _record['unmapped'] = str(_error)
    _owners_records.append(_record)

_owners_context = G17PUATReader(_owners_reader._read_dram,
    int(hv.u.adt['/arm-io/sgx'].gpu_region_base))
_owners_aliases = [('pb-descriptor', 0x7001838000, _owners_word(_owners_main, 0x2e0), 0x4000),
                   ('uma-descriptor', 0x7001840000, _owners_word(_owners_main, 0x2f0), 0x4000)]
for _ordinal in range(4):
    _low, _high = struct.unpack_from('<QQ', _owners_hw, 0x2614 + _ordinal * 32)
    _owners_aliases.append(('completion-' + str(_ordinal), _low, _high, 0x20000))
for _label, _low, _high, _size in _owners_aliases:
    for _offset in range(0, _size, 0x4000):
        _record = dict(label='alias-page', owner=_label, low=hex(_low + _offset),
                       high=hex(_high + _offset))
        try:
            _pa, _walk = _owners_context.translate(0, _low + _offset)
            _global_pa, _global_walk = _owners_reader.walker.translate(0, _high + _offset)
            _record.update(physical=hex(_pa), high_physical=hex(_global_pa),
                           walk=_walk, high_walk=_global_walk)
        except RuntimeError as _error:
            _record['unmapped'] = str(_error)
        _owners_records.append(_record)

# Translate every described I/O page plus its following guard. No physical
# device contents are read: translate reads DRAM-resident page tables only.
for _slot in range(53):
    _phys, _va, _size10, _size14, _unk, _flag = struct.unpack_from(
        '<QQIIQI', _owners_hw, 0x640 + _slot * 0x28)
    if not _va:
        continue
    _start = _va & ~0x3fff
    _end = (_va + max(_size10, _size14) + 0x3fff) & ~0x3fff
    for _page in range(_start, _end + 1, 0x4000):
        _record = dict(label='register-page', slot=_slot, pointer=hex(_page),
                       size_10=_size10, size_14=_size14, following_guard=_page == _end)
        try:
            _pa, _walk = _owners_reader.walker.translate(0, _page)
            _record.update(physical=hex(_pa), walk=_walk)
        except RuntimeError as _error:
            _record['unmapped'] = str(_error)
        _owners_records.append(_record)
(_owners_snapshot / 'manifest.json').write_text(json.dumps(dict(
    device_memory_read=False, records=_owners_records), indent=2) + '\n')
print('M5_GPU_OWNER_SNAPSHOT', _owners_snapshot.name)
