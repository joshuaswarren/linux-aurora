# SPDX-License-Identifier: MIT
"""Inspect or temporarily enable the native M5 host-owned trace input.

Run through the existing paused HV owner. M5_NATIVE_FW_TRACE_ACTION is
snapshot (default), enable, or restore. Enable accepts only the previously
qualified trace classes via M5_NATIVE_FW_TRACE_MASK (default 0x201).
Only status-A's host input word is writable; no firmware DATA, ring cursors,
recovery acknowledgments, MMIO, or executable bytes are modified.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from m5_gpu_capture import M5GPUCapture

if (hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3'
        or int(hv.u.adt['/chosen'].chip_id) != 0x8142):
    raise RuntimeError('Use the existing paused M5 hypervisor owner')
trace_action = globals().get('m5_native_trace_action',
                            os.environ.get('M5_NATIVE_FW_TRACE_ACTION', 'snapshot'))
trace_mask = int(str(globals().get('m5_native_trace_mask',
                    os.environ.get('M5_NATIVE_FW_TRACE_MASK', '0x201'))), 0)
if trace_action not in ('snapshot', 'enable', 'restore') or not 0 < trace_mask <= 0x203 or trace_mask & ~0x203:
    raise ValueError('Expected snapshot/enable/restore and mask within 0x203')
trace_base = Path(os.environ['M5_GPU_TRACE_DIR'])
trace_roots = re.findall(r'\buat_global_root_pa=(0x[0-9a-f]+)',
                        (trace_base / 'console.log').read_text())
if len(set(trace_roots)) != 1:
    raise RuntimeError('Expected this boot global-root receipt')
trace_reader = M5GPUCapture(hv.iface.readmem,
    (int(hv.u.adt['/chosen'].dram_base), int(hv.u.adt['/chosen'].dram_size)),
    int(trace_roots[0], 16))
trace_preinit = trace_base / 'preinit/gfx-asc'
trace_manifest = json.loads((trace_preinit / 'manifest.json').read_text())
trace_root_object = next(o for o in trace_manifest['objects'] if o['label'] == 'root')
trace_root = trace_reader.read(int(trace_root_object['pointer'], 16), 0xb8)
trace_qword = lambda data, offset: struct.unpack_from('<Q', data, offset)[0]
if (trace_qword(trace_root, 0) != M5GPUCapture.VERSION
        or trace_root != (trace_preinit / 'root.bin').read_bytes()):
    raise RuntimeError('Native root differs from this boot verified profile')
trace_main = trace_reader.read(trace_qword(trace_root, 0x18), 0x600)
trace_status = trace_qword(trace_root, 0xa8)
if (trace_status != 0xfffffc200002ee40
        or trace_qword(trace_main, 0x1e0) != trace_status + 0x240
        or trace_qword(trace_main, 0x1e8) != trace_status + 0xa6ac0):
    raise RuntimeError('Native primary trace profile differs from the measured M5 layout')
trace_physical, trace_walk = trace_reader.walker.translate(0, trace_status)
if (trace_physical & 3 or not trace_reader.low <= trace_physical <= trace_reader.high - 4):
    raise RuntimeError('Trace input is outside measured DRAM')
trace_word = trace_reader.read(trace_status, 4)
if trace_word != trace_reader.read(trace_status, 4):
    raise RuntimeError('Trace input is changing')
trace_before = int.from_bytes(trace_word, 'little')
trace_state = getattr(hv, 'm5_native_trace_input', None)
if trace_action == 'enable':
    if trace_state is not None or trace_before != 0:
        raise RuntimeError('Expected the unmodified zero native trace input')
    # Retain restoration state before the only permitted target write.
    trace_state = dict(physical=trace_physical, status=trace_status,
                       original=trace_before, enabled=trace_mask)
    hv.m5_native_trace_input = trace_state
    hv.p.write32(trace_physical | 0xa0000000000, trace_mask)
elif trace_action == 'restore':
    if (trace_state is None or trace_state['physical'] != trace_physical
            or trace_state['status'] != trace_status
            or trace_before not in (trace_state['original'], trace_state['enabled'])):
        raise RuntimeError('Trace input does not match this owner restoration receipt')
    hv.p.write32(trace_physical | 0xa0000000000, trace_state['original'])
trace_after = int.from_bytes(trace_reader.read(trace_status, 4), 'little')
trace_expected = (trace_mask if trace_action == 'enable' else
                 trace_state['original'] if trace_action == 'restore' else trace_before)
if trace_after != trace_expected:
    raise RuntimeError('Trace input readback differs; retain this owner restoration receipt')
if trace_action == 'restore':
    trace_state['input_restored'] = True

trace_output = trace_base / ('native-fw-trace-' + trace_action + '-' + str(time.time_ns()))
trace_output.mkdir(mode=0o700, exist_ok=False)
trace_result = dict(action=trace_action, input_before=hex(trace_before),
    input_after=hex(trace_after), input_physical=hex(trace_physical),
    firmware_data_written=False, device_memory_read=False, objects=[])
trace_sgx = hv.u.adt['/arm-io/sgx']
trace_flag_pa = int(trace_sgx.gfx_data_base) + 0x1779fc - 0x4c000
trace_flags = trace_reader._read_dram(trace_flag_pa, 4)
trace_result['firmware_flags_observed'] = hex(int.from_bytes(trace_flags, 'little'))
if trace_state is not None and trace_state.get('input_restored'):
    trace_result['firmware_restore_verified'] = (
        trace_after == trace_state['original']
        and int.from_bytes(trace_flags, 'little') == trace_state['original'])
    # Firmware refreshes its cached flags on configuration commands, not on
    # every workload. Keep the receipt until both views show the original.
    if trace_result['firmware_restore_verified']:
        hv.m5_native_trace_input = None
for trace_label, trace_address, trace_size in [
        # Both callers of firmware 0xe2e8 pass HW-data+0x2614: four
        # 32-byte hardware-event reader records in order 1, 3, 0, 2.
        ('hardware-event-state', trace_qword(trace_main, 0) + 0x2614, 0x80),
        ('states-before', trace_status, 0x300),
        ('records', trace_status + 0xa6ac0, 512 * 72),
        ('states-after', trace_status, 0x300)]:
    trace_data = trace_reader.read(trace_address, trace_size)
    trace_file = trace_label + '.bin'
    (trace_output / trace_file).write_bytes(trace_data)
    trace_result['objects'].append(dict(label=trace_label, file=trace_file,
        address=hex(trace_address), size=trace_size,
        sha256=hashlib.sha256(trace_data).hexdigest()))
(trace_output / 'manifest.json').write_text(json.dumps(trace_result, indent=2) + '\n')
print('M5_NATIVE_FW_TRACE_RESULT', trace_output.name, json.dumps({k: trace_result[k]
    for k in ('action', 'input_before', 'input_after', 'firmware_flags_observed')}))
