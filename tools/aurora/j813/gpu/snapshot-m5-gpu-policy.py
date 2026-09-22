# SPDX-License-Identifier: MIT
"""Snapshot current M5 policy backing in the existing paused HV session.

This is runtime evidence, not a replacement for pre-handoff captures.
Only verified-extent DRAM objects are read; no GPU registers are accessed.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import time

from m5_gpu_capture import M5GPUCapture

if (hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3' or
        int(hv.u.adt['/chosen'].chip_id) != 0x8142):
    raise RuntimeError('Pause the existing M5 guest before taking a policy snapshot')
_policy_output = Path(os.environ['M5_GPU_TRACE_DIR'])
_policy_roots = re.findall(r'\buat_global_root_pa=(0x[0-9a-f]+)',
                           (_policy_output / 'console.log').read_text())
if len(set(_policy_roots)) != 1:
    raise RuntimeError('Expected one current-boot global-root receipt')
_policy_reader = M5GPUCapture(hv.iface.readmem,
    (int(hv.u.adt['/chosen'].dram_base), int(hv.u.adt['/chosen'].dram_size)),
    int(_policy_roots[0], 16))
_policy_manifest = json.loads((_policy_output / 'preinit/gfx-asc/manifest.json').read_text())
_policy_objects = {entry['label']: entry for entry in _policy_manifest['objects']}
_policy_cluster = _policy_reader.read(int(_policy_objects['shared-cluster']['pointer'], 16), 0xc500)
_policy_aon_va = struct.unpack_from('<Q', _policy_cluster, 0xb6a0)[0]
_policy_snapshot = _policy_output / ('runtime-policy-' + str(time.time_ns()))
_policy_snapshot.mkdir(mode=0o700, exist_ok=False)
_policy_records = []
for _label, _va, _size in [
        ('status-b', int(_policy_objects['status-b']['pointer'], 16), 0xeb00),
        # Native sharedDataDescriptor at 0xfffffe00071398d8 carries 0xe4.
        ('aon-ptd', _policy_aon_va, 0xe4)]:
    _data = _policy_reader.read(_va, _size)
    if _data != _policy_reader.read(_va, _size):
        raise RuntimeError('Runtime policy changed between two reads: ' + _label)
    (_policy_snapshot / (_label + '.bin')).write_bytes(_data)
    _policy_records.append(dict(label=_label, pointer=hex(_va), size=_size,
                                sha256=hashlib.sha256(_data).hexdigest()))
(_policy_snapshot / 'manifest.json').write_text(json.dumps(dict(
    phase='runtime-after-Metal-reference', device_memory_read=False,
    objects=_policy_records), indent=2) + '\n')
print('M5_GPU_RUNTIME_POLICY', _policy_snapshot.name)
