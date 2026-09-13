#!/usr/bin/env python3
"""Validate the live M5 display session and the RAM loader's exported FDT."""
import hashlib
import json
import os
from pathlib import Path
import runpy
import struct
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def prepare_display(u, p):
    capture = runpy.run_path(str(ROOT / 'display/inspect-m5-display.py'))
    path = capture['capture_display_topology'](u, ROOT / 'logs', p, True)
    state = json.loads(path.read_text())
    status = state['register_snapshot']
    require(status.get('stable') and int(status['cpu_control'], 16) & 0x10,
            'DCP must already be running; cold startup is not qualified')
    for stream in state['dart_streams']:
        require(stream['enabled'] and int(stream['tcr'], 16) & 1 and
                int(stream['ttbr'], 16) != 0 and
                int(stream['reg_protect'], 16) & 1 and
                int(stream['reg_protect_lock'], 16) & 1,
                'Display DART must retain enabled, protected inherited tables')
        for segment in stream.get('segments', []):
            require(all(a is not None for a, _ in segment['translations']),
                    'DCP firmware has an unmapped segment')
    pmgr = u.adt['/arm-io/pmgr']
    require(pmgr.get_reg(47) == (0x382000000, 0x1000000) and
            u.adt['/arm-io/disp0'].get_reg(4) == (0x382700000, 0x114000),
            'M5 PMC register map differs from the native driver profile')
    for key, label in [('bwr-agent', b'DISP_INT'), ('boost-agent', b'DISP_INT_DCP')]:
        agents = [row for row in struct.iter_unpack('<4BI16s', bytes(pmgr.getprop(key)))
                  if row[5].rstrip(b'\0') == label]
        require(len(agents) == 1 and agents[0][2] == 0 and agents[0][4] == 62,
                'M5 display bandwidth/boost agent differs from the native profile')
    print('M5_DCP_LIVE_HANDOFF_PREFLIGHT_PASSED', path, flush=True)
    return state


def verify_display_fdt(u, p, before, integrated_gpu=False,
                       carrier_build=None, carrier_sha256=None):
    """Read the exact prepared FDT, using symbols from the verified RAM image."""
    build = (ROOT / 'hv/publish-j813/build-j813-dcp' if integrated_gpu else
             ROOT / 'm1n1/build-j813-dcp')
    require((carrier_build is None) == (carrier_sha256 is None),
            'Carrier build and SHA256 must be supplied together')
    if carrier_build is not None:
        build = Path(carrier_build)
    symbols = {}
    listing = subprocess.check_output(
        ['/opt/homebrew/opt/llvm/bin/llvm-nm', '--defined-only', str(build / 'm1n1-raw.elf')],
        text=True)
    for line in listing.splitlines():
        fields = line.split()
        if len(fields) == 3 and fields[2] in ('_base', '_end', 'dt', 'kboot_prepare_dt'):
            require(fields[2] not in symbols, 'Ambiguous loader symbol')
            symbols[fields[2]] = int(fields[0], 16)
    require(set(symbols) == {'_base', '_end', 'dt', 'kboot_prepare_dt'} and symbols['_base'] == 0,
            'Unexpected loader symbol layout')
    image = (build / 'm1n1.bin').read_bytes()
    if carrier_build is not None:
        require(hashlib.sha256(image).hexdigest() == carrier_sha256,
                'Selected carrier SHA256 mismatch')
    elif integrated_gpu:
        require(hashlib.sha256(image).hexdigest() ==
                '3eb341d34e923abe578de7e670ed948c84fcb6f50084a23dd132898fd3b4b409',
                'Integrated carrier differs from the pinned combined candidate')
    offset = symbols['kboot_prepare_dt']
    require(u.iface.readmem(u.base + offset, 64) == image[offset:offset + 64],
            'Running loader does not match the DCP candidate code')
    require(0 <= symbols['dt'] <= symbols['_end'] - 8, 'FDT pointer outside loader')
    address = p.read64(u.base + symbols['dt'])
    dram = int(u.adt['/chosen'].getprop('dram-base'))
    end = dram + int(u.adt['/chosen'].getprop('dram-size'))
    require(dram <= address < address + 40 <= end, 'FDT pointer outside RAM')
    header = u.iface.readmem(address, 40)
    magic, size = struct.unpack_from('>II', header)
    require(magic == 0xd00dfeed and 40 <= size <= 1024 * 1024 and address + size <= end,
            'Prepared FDT header invalid')
    data = b''.join(u.iface.readmem(address + pos, min(0x4000, size - pos))
                    for pos in range(0, size, 0x4000))
    path = ROOT / 'logs' / ('m5-dcp-prepared-' + str(time.time_ns()) + '.dtb')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
    helpers = runpy.run_path(str(ROOT / 'video/check-m5-dtb.py'))
    tree = helpers['read_tree'](path)
    handles = helpers['handles'](tree)
    chosen = tree['/chosen']
    require(not any(key in chosen for key in ('apple,dcp-rtkit-quiesced',
                'apple,dcp-rtkit-idle', 'apple,dcp-asc-stopped')),
            'Running DCP must not be represented as quiesced or stopped')
    bw = tree['/soc/power-management@382720000']
    require(struct.unpack('>QQQQ', bw['reg']) == (0x382720000, 8, 0x3827a4000, 4),
            'Display bandwidth resources disagree with the native M5 driver')
    aliases = {key: value.rstrip(b'\0').decode() for key, value in tree['/aliases'].items()}
    streams = {aliases['dcp']: ('/arm-io/dart-dcp', 23),
               aliases['disp0']: ('/arm-io/dart-disp0', 0),
               aliases['disp0_piodma']: ('/arm-io/dart-disp0', 16)}
    from m1n1.hw.dart import DART

    class TableReader:
        def readmem(self, addr, count):
            require(count == 0x4000 and dram <= addr < addr + count <= end,
                    'DART table read outside RAM')
            return u.iface.readmem(addr, count)

    darts = {}
    for saved in before['dart_streams']:
        key = (saved['path'], saved['stream'])
        dart = DART.from_adt(u, key[0])
        dart.dart.iface = TableReader()
        require(int(dart.dart.regs.TTBR[key[1]].val) == int(saved['ttbr'], 16) and
                int(dart.dart.regs.TCR[key[1]].val) == int(saved['tcr'], 16) and
                int(dart.dart.regs.REG_PROTECT.val) == int(saved['reg_protect'], 16) and
                int(dart.dart.regs.REG_PROTECT_LOCK.val) == int(saved['reg_protect_lock'], 16),
                'Loader changed inherited display translation controls')
        darts[key] = dart
    owned_regions = {handles[handle] for node in streams
                     for handle in helpers['words'](tree[node].get('memory-region', b''))}
    regions, mappings, reserved_iovas = [], 0, []
    for name, props in tree.items():
        if name not in owned_regions:
            continue
        require(name.startswith('/reserved-memory/') and
                props.get('compatible') in (b'apple,asc-mem\0', b'framebuffer\0'),
                'Unexpected display reservation type')
        physical = None
        if 'reg' in props:
            require('no-map' in props and len(props['reg']) == 16,
                    'Display physical reservation must be a no-map range')
            physical, length = struct.unpack('>QQ', props['reg'])
            require(dram <= physical < physical + length <= end, 'Display reservation outside RAM')
            regions.append((physical, length))
        entries = props.get('iommu-addresses', b'')
        require(len(entries) % 20 == 0, 'Malformed inherited IOMMU mapping')
        for handle, iova, count in struct.iter_unpack('>IQQ', entries):
            node = handles[handle]
            require(node in streams and (physical is None or count == length),
                    'Unexpected display mapping owner/size')
            require(1 << 40 <= iova < iova + count <= (1 << 40) + (1 << 36),
                    'Display reservation is outside the DMA allocation window')
            key = streams[node]
            cursor = physical
            for actual, amount in darts[key].iotranslate(key[1], iova, count):
                require(actual is not None and dram <= actual < actual + amount <= end,
                        'Reserved display IOVA is unmapped or outside RAM')
                if physical is not None:
                    require(actual == cursor, 'Exported display mapping disagrees with live DART')
                    cursor += amount
            if physical is not None:
                require(cursor == physical + length, 'Incomplete exported display mapping')
            reserved_iovas.append((key, iova, count))
            mappings += 1
    carveouts = u.adt['/chosen/carveout-memory-map']
    expected = [tuple(carveouts.getprop('region-id-' + str(n))) for n in (49, 50, 57, 94, 95)]
    expected.append(tuple(u.adt['/vram'].get_reg(0)))
    require(all(region in regions for region in expected), 'Missing display physical reservation')
    require(mappings >= 3, 'Inherited display IOVA mappings not exported')
    for segment in before['dart_streams'][0]['segments']:
        iova, size = int(segment['remap'], 16), int(segment['size'], 16)
        coverage = sorted((base, base + count) for key, base, count in reserved_iovas
                          if key == ('/arm-io/dart-dcp', 23))
        cursor = iova
        for base, stop in coverage:
            if base <= cursor < stop:
                cursor = stop
        require(cursor >= iova + size, 'Live DCP firmware IOVA is not reserved')
        # The live OS_LOG alias differs from the ADT physical descriptor.
        for physical, length in segment['translations']:
            actual, amount = int(physical, 16), int(length, 16)
            require(any(base <= actual and actual + amount <= base + count for base, count in regions),
                    'Live DCP firmware memory is not reserved')
    require(p.read32(0x286e00044) == int(before['register_snapshot']['cpu_control'], 16),
            'DCP CPU state changed before Linux handoff')
    print('M5_DCP_EXPORTED_FDT_VERIFIED', path, 'regions=' + str(len(regions)),
          'mappings=' + str(mappings), 'sha256=' + hashlib.sha256(data).hexdigest(), flush=True)
    return path
