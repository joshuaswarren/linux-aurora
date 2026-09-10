#!/usr/bin/env python3
"""Capture M5 boot-local GPU resources and loaded firmware without GPU MMIO.

This records identity for porting. It grants no permission to start either
ASC and does not substitute a Neo firmware ABI for the M5's firmware.
"""
import hashlib
import json
import os
from pathlib import Path
import runpy
import struct

REGIONS = ('gpu-region', 'gfx-shared-region', 'gfx-shared-l2-region',
           'gfx-handoff', 'gfx-data', 'gfx1-data')


def validate_regions(regions, dram):
    low, size = dram
    high = low + size
    if low <= 0 or size <= 0 or high > 1 << 64:
        raise ValueError('Invalid DRAM bounds')
    intervals = []
    for name in REGIONS:
        base, size = regions[name]
        if (not base or not size or (base | size) & 0x3fff or
                not low <= base < base + size <= high):
            raise ValueError('Invalid boot-local GPU region: ' + name)
        if name == 'gfx-shared-region' and size < 0x44000:
            raise ValueError('GPU page-table reservation is too small')
        if any(base < end and start < base + size for start, end in intervals):
            raise ValueError('Overlapping GPU reservation: ' + name)
        intervals.append((base, base + size))


def inspect_adt(adt):
    if adt.model != 'Mac17,3' or int(adt['/chosen'].chip_id) != 0x8142:
        raise ValueError('GPU preflight requires M5 J813')
    expected = {'sgx': [(0x580000000, 0x4000000)],
                'gfx-asc': [(0x582600000, 0x88000), (0x582050000, 8)],
                'gfx1-asc': [(0x582e00000, 0x88000), (0x582850000, 8)]}
    for name, registers in expected.items():
        node = adt['/arm-io/' + name]
        if [node.get_reg(i) for i in range(len(registers))] != registers:
            raise ValueError('M5 GPU register layout changed: ' + name)
    adt.pmgr_init()
    devices = {adt.pmgr_dev_get_id(d): d for d in adt['/arm-io/pmgr'].devices}
    for device, address in [(512, 0x580e80000), (514, 0x580e80110)]:
        if devices[device].flags.no_ps or adt.pmgr_dev_get_addr(devices[device]) != address:
            raise ValueError('M5 GPU power mapping changed')
    # Follow every parent: omitting GPX would leave GFX's physical parent
    # uncontrolled even though the intervening AFGGFXNI0 node is virtual.
    physical, seen, pending = set(), set(), [514]
    while pending:
        identity = pending.pop()
        if not identity or identity in seen:
            continue
        seen.add(identity)
        device = devices[identity]
        if not device.flags.no_ps:
            physical.add(identity)
        pending.extend(adt.pmgr_dev_get_parents(device))
    if physical != {512, 514}:
        raise ValueError('M5 GPU physical power-parent chain changed')
    node = adt['/arm-io/sgx']
    regions = {name: tuple(int(node.getprop(name + suffix)) for suffix in ('-base', '-size'))
               for name in REGIONS}
    chosen = adt['/chosen']
    if int(adt['/defaults'].getprop('uat-vaddr-size')) != 43:
        raise ValueError('M5 total UAT virtual-address width changed')
    if int(node.getprop('agx-address-space-mgmt-mode')) != 1:
        raise ValueError('M5 UAT address-space management mode changed')
    dram = (int(chosen.getprop('dram-base')), int(chosen.getprop('dram-size')))
    validate_regions(regions, dram)
    segments = []
    for role, data in [('gfx-asc', 'gfx-data'), ('gfx1-asc', 'gfx1-data')]:
        node = adt['/arm-io/' + role]
        raw = bytes(node.getprop('segment-ranges'))
        if str(node.getprop('segment-names')) != '__TEXT;__DATA' or len(raw) != 64:
            raise ValueError('Unexpected M5 GPU firmware segment layout')
        for i, name in enumerate(('text', 'data')):
            base, virtual, physical, size, flags = struct.unpack_from('<QQQII', raw, i * 32)
            if (base != physical or (base | size) & 0x3fff or not size or
                    not dram[0] <= base < base + size <= sum(dram)):
                raise ValueError('Invalid M5 GPU firmware segment range')
            if name == 'data' and (base, size) != regions[data]:
                raise ValueError('GPU firmware DATA disagrees with its reservation')
            segments.append(dict(role=role, name=name, base=base, virtual=virtual,
                                 size=size, flags=flags))
    return regions, segments


def prepare_gpu(u, iface, output, require_known_firmware=False):
    regions, segments = inspect_adt(u.adt)
    # The four loaded firmware segments sit below usable Linux RAM on M5.
    # Reserve DATA explicitly as well, and refuse a changed boot layout.
    if any(s['base'] + s['size'] > int(u.ba.phys_base) for s in segments):
        raise ValueError('M5 GPU firmware is not wholly in iBoot reserved memory')
    output = Path(output)
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    for segment in segments:
        payload = iface.readmem(segment['base'], segment['size'])
        if len(payload) != segment['size'] or payload != iface.readmem(segment['base'], segment['size']):
            raise ValueError('Short or unstable GPU firmware capture')
        segment['sha256'] = hashlib.sha256(payload).hexdigest()
        if require_known_firmware and segment['name'] == 'text':
            qualify = runpy.run_path(str(Path(__file__).with_name(
                'm5_firmware_identity.py')))['qualify_text']
            segment['identity'] = qualify(segment['role'], payload)
        path = output / (segment['role'] + '-' + segment['name'] + '.bin')
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'wb') as stream:
            stream.write(payload)
    manifest = dict(source='Two matching pre-Linux KIS DRAM reads; no GPU MMIO',
                    firmware_runtime_admitted=False, uat_virtual_address_bits=43,
                    uat_root_select_bit=42, regions=regions, segments=segments)
    fd = os.open(output / 'manifest.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as stream:
        json.dump(manifest, stream, indent=2)
        stream.write('\n')
    print('M5_GPU_BOOT_PREFLIGHT_PASSED regions=6 firmware_segments=4 runtime_admitted=no', flush=True)
