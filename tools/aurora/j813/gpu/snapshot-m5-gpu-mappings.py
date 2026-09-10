# SPDX-License-Identifier: MIT
"""Record firmware I/O page mappings without reading any mapped device."""
import json
import os
from pathlib import Path
import re
import time

from m5_gpu_capture import M5GPUCapture
from m1n1.agx.g17p_trace import G17PUATReader

if (hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3' or
        int(hv.u.adt['/chosen'].chip_id) != 0x8142):
    raise RuntimeError('Pause the existing M5 guest before walking its mappings')
_m5_maps_output = Path(os.environ['M5_GPU_TRACE_DIR'])
_m5_maps_roots = re.findall(r'\buat_global_root_pa=(0x[0-9a-f]+)',
                           (_m5_maps_output / 'console.log').read_text())
if len(set(_m5_maps_roots)) != 1:
    raise RuntimeError('Expected the current boot global-root receipt')
_m5_maps_reader = M5GPUCapture(hv.iface.readmem,
    (int(hv.u.adt['/chosen'].dram_base), int(hv.u.adt['/chosen'].dram_size)),
    int(_m5_maps_roots[0], 16))
_m5_maps_records = []
_m5_maps_context_reader = G17PUATReader(_m5_maps_reader._read_dram,
    int(hv.u.adt['/arm-io/sgx'].gpu_region_base))
for _m5_maps_label, _m5_maps_va in [
        ('hardware-header-28', 0xfffffc2181400000),
        ('accelerator-register-window', 0xfffffc2180080000),
        ('g17g-register-slot39', 0xfffffc2180128000)]:
    for _m5_maps_context, _m5_maps_view, _m5_maps_active in [
            (0, 'sptm-global', _m5_maps_reader.walker),
            (0, 'context0', _m5_maps_context_reader),
            (1, 'context1', _m5_maps_context_reader),
            (2, 'context2', _m5_maps_context_reader)]:
        _m5_maps_record = dict(label=_m5_maps_label, firmware_va=hex(_m5_maps_va),
                               view=_m5_maps_view, device_memory_read=False)
        try:
            _m5_maps_pa, _m5_maps_walk = _m5_maps_active.translate(_m5_maps_context, _m5_maps_va)
            _m5_maps_record.update(physical=hex(_m5_maps_pa), walk=_m5_maps_walk)
        except RuntimeError as _m5_maps_error:
            _m5_maps_record['unmapped'] = str(_m5_maps_error)
        _m5_maps_records.append(_m5_maps_record)
_m5_maps_path = _m5_maps_output / ('gpu-mappings-' + str(time.time_ns()) + '.json')
with _m5_maps_path.open('x') as _m5_maps_file:
    json.dump(_m5_maps_records, _m5_maps_file, indent=2)
print('M5_GPU_MAPPING_SNAPSHOT', _m5_maps_path.name, 'page tables only; no device reads')
