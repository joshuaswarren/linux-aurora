"""Read cached M5 DART state at a paused guest; no device MMIO accesses."""
import ctypes
import os
from pathlib import Path
import re
import struct
import time
from tools.sptm.seed import _m1n1_symbol_offset


def primitive_type(source, structure, terminator=None):
    body = source.split('struct ' + structure + ' {', 1)[1].split('};', 1)[0]
    if terminator:
        body = body.split(terminator, 1)[0]
    constants = dict(re.findall(r'^#define (DART_C_MAX_\w+)\s+(\d+)\s*$',
                                source, re.M))
    kinds = dict(u8=ctypes.c_uint8, u32=ctypes.c_uint32, u64=ctypes.c_uint64)
    fields = []
    for kind, name, count in re.findall(
            r'\b(u8|u32|u64)\s+(\w+)(?:\[(\w+)\])?;', body):
        item = kinds[kind]
        fields.append((name, item * int(constants.get(count, count)) if count else item))
    return type(structure, (ctypes.LittleEndianStructure,), {'_fields_': fields})


def read_dart_state(hv, path='/arm-io/dart-usb0'):
    if int(hv.adt['/chosen'].chip_id) != 0x8142 or hv.ctx is None:
        raise RuntimeError('Requires paused M5 guest')
    base = Path(__file__).resolve().parent
    source = (base / 'm1n1/src/sptm/private.h').read_text()
    prefix = primitive_type(source, 'dart_c_instance', 'struct dart_c_dapf_state dapf;')
    sid_type = primitive_type(source, 'dart_c_sid_state')
    if (ctypes.sizeof(prefix) != 0x1e0 or ctypes.sizeof(sid_type) != 32 or
            'sizeof(struct dart_c_instance) == 0x2a18' not in source or
            'sizeof(struct nvme_c_state) == 0x2128' not in source):
        raise RuntimeError('Resident DART layout changed; requalify offsets')
    state = hv.p.read64(hv.u.base + _m1n1_symbol_offset('g_sptm_state'))
    if not state or state & 0x3fff:
        raise RuntimeError('Invalid SPTM state')
    dart = state + 0x14570 + 0x2128
    dart_id = int(hv.adt[path].dart_id)
    raw = hv.iface.readmem(dart, 0xa8 + 32 * 0x2a18)
    directory = base / 'logs/dart-state'
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    dest = directory / f'dart-{dart_id}-{time.time_ns()}.bin'
    with open(os.open(dest, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as out:
        out.write(raw)
    print('M5_DART_GLOBAL', struct.unpack_from('<4I3Q', raw))
    matches = []
    for i in range(32):
        offset = 0xa8 + i * 0x2a18
        obj = prefix.from_buffer_copy(raw, offset)
        if obj.used and obj.dart_id == dart_id:
            matches.append((offset, obj))
    if len(matches) != 1:
        raise RuntimeError(f'Expected one cached DART {dart_id}; found {len(matches)}')
    offset, obj = matches[0]
    print('M5_DART_INSTANCE', path, dart_id, 'bases',
          [hex(v) for v in obj.base_pa[:obj.base_count]],
          'obj', hex(obj.obj_va), 'state', hex(obj.state_va),
          'calls', obj.total_calls, 'violations', obj.violations,
          'init', obj.init_state, 'locked', obj.locked)
    print('M5_DART_ENDPOINTS', list(enumerate(obj.ep_count)))
    print('M5_DART_LAST', [hex(v) for v in obj.last])
    for sid in range(256):
        item = sid_type.from_buffer_copy(raw, offset + 0x9e0 + sid * 32)
        if item.valid:
            print('M5_DART_SID', sid, {name: hex(getattr(item, name))
                                     for name, _ in sid_type._fields_})
    print('M5_DART_STATE_SAVED_PRIVATE', str(dest))


if 'hv' in globals():
    read_dart_state(hv)
