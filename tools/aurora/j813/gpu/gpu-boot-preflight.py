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
import subprocess
import time

REGIONS = ('gpu-region', 'gfx-shared-region', 'gfx-shared-l2-region',
           'gfx-handoff', 'gfx-data', 'gfx1-data')

FDT_REGIONS = dict(zip(REGIONS, ('uat-ttbs', 'uat-pagetables',
    'uat-l2-pagetables', 'uat-handoff', 'gfx-data', 'gfx1-data')))


def validate_exported_regions(tree, regions):
    """Reject empty, stale, or miswired reservations before Linux takes RAM."""
    gpu = tree['/soc/gpu@582600000']
    if gpu.get('compatible') != b'apple,agx-t8142\0':
        raise ValueError('Prepared FDT is not the M5 GPU profile')
    names = gpu['memory-region-names'].rstrip(b'\0').decode().split('\0')
    refs = struct.unpack('>' + 'I' * (len(gpu['memory-region']) // 4),
                         gpu['memory-region'])
    expected = ('ttbs', 'pagetables', 'handoff', 'shared-l2', 'gfx-data', 'gfx1-data')
    paths = ('uat-ttbs', 'uat-pagetables', 'uat-handoff', 'uat-l2-pagetables',
             'gfx-data', 'gfx1-data')
    if tuple(names) != expected or len(refs) != len(paths):
        raise ValueError('Prepared GPU memory-region bindings changed')
    for name, path in FDT_REGIONS.items():
        props = tree['/reserved-memory/' + path]
        if props.get('reg') != struct.pack('>QQ', *regions[name]):
            raise ValueError('Prepared GPU reservation differs from live ADT: ' + path)
    for ref, path in zip(refs, paths):
        if tree['/reserved-memory/' + path].get('phandle') != struct.pack('>I', ref):
            raise ValueError('Prepared GPU memory-region points to the wrong reservation')


def verify_gpu_fdt(u, p, combined_display=False,
                   carrier_build=None, carrier_sha256=None):
    """Read the prepared FDT from the qualified GPU RAM carrier's dt symbol."""
    root = Path(__file__).resolve().parents[1]
    build = (root / 'hv/publish-j813/build-j813-dcp' if combined_display else
             root.parent / 'j813-m1n1-integrate/build')
    image = (build / 'm1n1.bin').read_bytes()
    expected = ('3eb341d34e923abe578de7e670ed948c84fcb6f50084a23dd132898fd3b4b409'
                if combined_display else
                '768edbffc67e30ac3bb608b8245a468ab1e20ca89eb740294db16cc46a2bc5cf')
    if (carrier_build is None) != (carrier_sha256 is None):
        raise ValueError('Carrier build and SHA256 must be supplied together')
    if carrier_build is not None:
        build = Path(carrier_build)
        image = (build / 'm1n1.bin').read_bytes()
        expected = carrier_sha256
    if hashlib.sha256(image).hexdigest() != expected:
        raise ValueError('GPU carrier differs from the selected pinned image')
    listing = subprocess.check_output(['/opt/homebrew/opt/llvm/bin/llvm-nm',
        '--defined-only', str(build / 'm1n1-raw.elf')], text=True)
    symbols = {}
    for line in listing.splitlines():
        fields = line.split()
        if len(fields) == 3 and fields[2] in ('_base', '_end', 'dt', 'kboot_prepare_dt'):
            if fields[2] in symbols:
                raise ValueError('Ambiguous GPU carrier symbol')
            symbols[fields[2]] = int(fields[0], 16)
    if (set(symbols) != {'_base', '_end', 'dt', 'kboot_prepare_dt'} or
            symbols['_base'] != 0 or not 0 <= symbols['dt'] <= symbols['_end'] - 8):
        raise ValueError('Invalid GPU carrier symbols')
    offset = symbols['kboot_prepare_dt']
    if (not 0 <= offset <= len(image) - 64 or
            u.iface.readmem(u.base + offset, 64) != image[offset:offset + 64]):
        raise ValueError('Running loader does not match the qualified GPU carrier')
    address = p.read64(u.base + symbols['dt'])
    chosen = u.adt['/chosen']
    low = int(chosen.getprop('dram-base'))
    end = low + int(chosen.getprop('dram-size'))
    if not low <= address < address + 40 <= end:
        raise ValueError('Prepared GPU FDT pointer outside RAM')
    magic, size = struct.unpack_from('>II', u.iface.readmem(address, 40))
    if magic != 0xd00dfeed or not 40 <= size <= 1024 * 1024 or address + size > end:
        raise ValueError('Invalid prepared GPU FDT header')
    data = b''.join(u.iface.readmem(address + i, min(0x4000, size - i))
                    for i in range(0, size, 0x4000))
    path = root / 'logs' / ('m5-gpu-prepared-' + str(time.time_ns()) + '.dtb')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
    tree = runpy.run_path(str(root / 'video/check-m5-dtb.py'))['read_tree'](path)
    regions, _ = inspect_adt(u.adt)
    validate_exported_regions(tree, regions)
    print('M5_GPU_EXPORTED_FDT_VERIFIED regions=6', path,
          'sha256=' + hashlib.sha256(data).hexdigest(), flush=True)
    return path


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
