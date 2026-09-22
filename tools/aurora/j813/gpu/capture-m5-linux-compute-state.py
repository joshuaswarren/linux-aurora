#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Read retained M5 compute RAM after the bounded probe requested CPU stops.

Never reads MMIO. Physical pages must belong to Linux System RAM or one of
this boot's measured GPU reserved-RAM ranges. Results are physical-memory
observations, not claims about contents held in firmware-private caches.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import sys
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(os.environ.get('M1N1_DIR', root / 'hv/m1n1')) / 'proxyclient'))
from m1n1.proxy import Serial
from m1n1.agx.g17p_trace import G17PUATReader

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('inventory', type=Path)
parser.add_argument('output', type=Path)
parser.add_argument('--triangle', action='store_true', help='Require and capture the mode11 compute triangle')
phase_args = parser.add_mutually_exclusive_group()
phase_args.add_argument('--before-compute', action='store_true',
                    help='Require successful mode7 setup with its SKSM kick unpublished')
phase_args.add_argument('--before-recovery', action='store_true',
                    help='Require mode8 admission frozen after the kick and before recovery')
phase_args.add_argument('--after-live-snapshot', action='store_true',
                    help='Require mode9/10 live observations; capture the later retained RAM')
phase_args.add_argument('--after-success', action='store_true',
                    help='Require exact compute output, retired queues and no GPU restart')
phase_args.add_argument('--after-graphics-draw', action='store_true',
                    help='Capture mode12 pixels after retired graphics commands and firmware stop requests')
phase_args.add_argument('--after-application-failure', action='store_true',
                    help='Capture the retained first mode14 application graph after failed submission and stop requests')
parser.add_argument('--application-native-va', action='store_true',
                    help='Capture command data from the application test with --native-va')
parser.add_argument('--graphics-shared-list', action='store_true',
                    help='Use the shared completion-list and uncached stamp layout for mode12')
parser.add_argument('--graphics-usc-pool', action='store_true',
                    help='Capture relocated graphics USC addresses and unmapped guard-page walks')
parser.add_argument('--graphics-parameter-buffer', action='store_true',
                    help='Capture the dynamic parameter-buffer directory and firmware aliases')
parser.add_argument('--graphics-render-targets', action='store_true',
                    help='Capture VM-owned tile/viewport/status/auxiliary resources')
parser.add_argument('--context', type=int, choices=range(3, 64), default=3,
                    help='Application root slot used by the mode12 graphics draw')
args = parser.parse_args()
graphics_capture = args.after_graphics_draw or args.after_application_failure
if args.application_native_va and not args.after_application_failure:
    parser.error('--application-native-va requires --after-application-failure')
if args.graphics_shared_list and not graphics_capture:
    parser.error('--graphics-shared-list requires --after-graphics-draw')
if args.graphics_usc_pool and not graphics_capture:
    parser.error('--graphics-usc-pool requires --after-graphics-draw')
if args.graphics_parameter_buffer and not graphics_capture:
    parser.error('--graphics-parameter-buffer requires --after-graphics-draw')
if args.graphics_render_targets and not graphics_capture:
    parser.error('--graphics-render-targets requires --after-graphics-draw')
if args.context != 3 and not graphics_capture:
    parser.error('--context requires --after-graphics-draw')
if args.triangle and not (args.after_success or args.after_live_snapshot):
    parser.error('--triangle requires --after-success or --after-live-snapshot')
inventory_path, output_path = args.inventory, args.output
inventory = json.loads(inventory_path.read_text())
if inventory['model'] != 'Mac17,3':
    raise ValueError('Expected this boot M5 inventory')
props = inventory['nodes']['/arm-io/sgx']['properties']
reserved = [(props[name + '-base'], props[name + '-base'] + props[name + '-size'])
            for name in ('gpu-region', 'gfx-shared-region', 'gfx-shared-l2-region',
                         'gfx-data', 'gfx1-data')]
output_path.mkdir(mode=0o700, exist_ok=False)
manifest = dict(phase=('Linux first application submission failure; rendering unproven' if args.after_application_failure
                      else 'Linux TA/fragment shared completion state' if args.graphics_shared_list
                      else 'Linux TA/fragment pixels with completion bookkeeping unqualified' if args.after_graphics_draw
                      else 'Linux mode8 admission before recovery' if args.before_recovery
                      else 'Linux mode7 setup before compute' if args.before_compute
                      else 'Linux verified compute completion' if args.after_success
                      else 'Linux live-status probe after compute' if args.after_live_snapshot
                      else 'Linux mode6 after compute') + ' after both firmware CPU stop requests',
                device_memory_read=False, compute_triangle=args.triangle,
                graphics_draw=graphics_capture, application_failure=args.after_application_failure,
                application_native_va=args.application_native_va,
                graphics_shared_list=args.graphics_shared_list,
                graphics_context=args.context if graphics_capture else None, objects=[])
cache = {}
sequence = 0
with Serial('/dev/cu.kis-01100000-ch-0', 115200, timeout=0.2) as port:
    def command(script):
        global sequence
        sequence += 1
        token = 'M5_RAM_%d' % sequence
        port.write(('stty -echo; echo ' + token + '_BEGIN; ' + script
                    + '; echo ' + token + '_END; stty echo\n').encode())
        received = bytearray()
        deadline = time.monotonic() + 30
        end = ('\n' + token + '_END\n').encode()
        while time.monotonic() < deadline:
            received.extend(port.read(65536))
            clean = bytes(received).replace(b'\r', b'')
            if end in clean:
                return clean.split(('\n' + token + '_BEGIN\n').encode(), 1)[1].split(end, 1)[0]
        raise RuntimeError('Stopped M5 RAM read timed out; leave target unchanged')

    probe_gate = (
        "dmesg | grep -Eq 'requested probe mode=(6|9|10|11)$' "
        "&& dmesg | grep -q 'M5_COMPUTE verified 1024 exact outputs, inputs unchanged, queues retired;' "
        "&& dmesg | grep -q 'M5_INIT transaction result=Ok(())' "
        "&& ! dmesg | grep -q 'M5_COMPUTE firmware requested GPU restart'"
        if args.after_success else (
        "dmesg | grep -q 'requested probe mode=7' "
        "&& dmesg | grep -q 'M5_INIT transaction result=Ok(())' "
        "&& dmesg | grep -q '1024 untouched outputs, SKSM kick unpublished' "
        "&& ! dmesg | grep -q 'M5_COMPUTE first SKSM kick submitted' "
        "&& ! dmesg | grep -q 'requested probe mode=6'"
        if args.before_compute else (
        "dmesg | grep -q 'requested probe mode=8' "
        "&& dmesg | grep -q 'M5_INIT transaction result=Ok(())' "
        "&& dmesg | grep -q 'M5_KSM quiescent admission snapshot;' "
        "&& dmesg | grep -Fq 'id=0x41 args=[b, 1, 2, 1]' "
        "&& ! dmesg | grep -q 'M5_COMPUTE firmware requested GPU restart'"
        if args.before_recovery else (
        "dmesg | grep -Eq 'requested probe mode=(9|10|11)$' "
        "&& dmesg | grep -q 'M5_LIVE_SNAPSHOT sample=3'"
        if args.after_live_snapshot else "dmesg | grep -q 'requested probe mode=6'"))))
    if args.after_graphics_draw:
        probe_gate = (
            "dmesg | grep -q 'requested probe mode=12$' "
            "&& dmesg | grep -q 'M5_GRAPHICS installed TA/fragment queues;' "
            "&& ( dmesg | grep -Fq 'M5_GRAPHICS final outer/inner/completion=([4, 4, 4], [[4, 4, 4], [4, 4, 4]],' "
            "|| dmesg | grep -q 'M5_GRAPHICS verified native-program TA/fragment pixels=.*both queues retired;' ) "
            "&& dmesg | grep -Fq 'pixels=TriangleResult { foreground: 18432, background: 47104, unexpected: 0, untouched: 0, checked: 63234, wrong: 0 }' "
            "&& ! dmesg | grep -q 'M5_GRAPHICS firmware requested GPU restart'")
    if args.triangle:
        probe_gate += " && dmesg | grep -q 'requested probe mode=11$' && dmesg | grep -q 'pages=7 triangle=true'"
    else:
        probe_gate += " && ! dmesg | grep -q 'requested probe mode=11$'"
    stop_gate = "dmesg | grep -q 'M5_INIT stop requests=Ok(())'"
    if args.after_application_failure:
        probe_gate = (
            "dmesg | grep -q 'M5_APPLICATION session ready; both firmware CPUs retained' "
            "&& dmesg | grep -q 'M5_DRM registered experimental first-job render adapter' "
            "&& dmesg | grep -q 'M5_GRAPHICS installed TA/fragment queues;' "
            "&& dmesg | grep -Fq 'M5_APPLICATION submission failed'")
        stop_gate = "dmesg | grep -Fq 'stop=Ok(()); VM and firmware owners retained'"
    identity = command("grep -q apple,j813 /proc/device-tree/compatible "
        "&& [ $(uname -r) = 7.1.6-aurora-j813-gpu-v9 ] "
        "&& [ -d /sys/module/asahi ] "
        "&& " + probe_gate + " "
        "&& " + stop_gate + " "
        "&& cat /proc/iomem")
    ram = []
    for line in identity.decode().splitlines():
        match = re.fullmatch(r'\s*([0-9a-f]+)-([0-9a-f]+) : System RAM', line)
        if match:
            ram.append((int(match[1], 16), int(match[2], 16) + 1))
    if not ram:
        raise RuntimeError('M5 stopped-probe identity or System RAM inventory failed')
    allowed = ram + reserved

    def read_phys(address, length):
        if not 0 < length <= 0x4000:
            raise ValueError('Unexpected RAM read extent')
        result = bytearray()
        while length:
            page = address & ~0xfff
            count = min(length, 0x1000 - (address - page))
            if not any(low <= page <= high - 0x1000 for low, high in allowed):
                raise ValueError('Read is outside measured RAM')
            if page not in cache:
                if len(cache) >= 256:
                    raise ValueError('Stopped M5 capture exceeds 1MiB read budget')
                raw = command('dd if=/dev/mem bs=4096 skip=%d count=1 2>/dev/null | base64'
                              % (page // 4096))
                data = base64.b64decode(b''.join(raw.split()), validate=True)
                if len(data) != 0x1000:
                    raise RuntimeError('Short physical RAM page')
                cache[page] = data
            result.extend(cache[page][address - page:address - page + count])
            address += count
            length -= count
        return bytes(result)

    walker = G17PUATReader(read_phys, props['gpu-region-base'])

    def save(label, address, length, context=None):
        item = dict(label=label, address=hex(address), size=length, context=context)
        try:
            if context is None:
                raw = b''.join(read_phys(address + off, min(0x4000, length - off))
                               for off in range(0, length, 0x4000))
            else:
                physical, walk = walker.translate(context, address)
                item['first_mapping'] = dict(physical=hex(physical),
                    walk=[[hex(slot), hex(entry)] for slot, entry in walk])
                raw = walker.read(address, length, context=context)
            name = label + '.bin'
            (output_path / name).write_bytes(raw)
            item.update(file=name, sha256=hashlib.sha256(raw).hexdigest())
        except (RuntimeError, ValueError) as error:
            item['error'] = str(error)
        manifest['objects'].append(item)

    def compact_span(base, size):
        if base & 0x7fff or size <= 0 or size & 0x3fff or not (
                0x1000000000 <= base < base + size + 0x8000 <= 0x1100000000):
            raise ValueError('Resource span escaped the compact arena')
        pages = []
        for offset in range(0, size, 0x4000):
            physical, walk = walker.translate(args.context, base + offset)
            pages.append(dict(address=hex(base + offset), physical=hex(physical),
                walk=[[hex(a), hex(e)] for a, e in walk]))
        guards = []
        for address in (base + size, base + size + 0x4000):
            walk = []
            def guard_read(physical, length):
                raw = read_phys(physical, length)
                if length != 8:
                    raise ValueError('Unexpected compact guard read size')
                walk.append([hex(physical), hex(int.from_bytes(raw, 'little'))])
                return raw
            guard_walker = G17PUATReader(read_phys, props['gpu-region-base'],
                read_root=guard_read, read_table=guard_read)
            try:
                guard_walker.translate(args.context, address)
            except RuntimeError as error:
                if not str(error).startswith('unmapped G17P UAT address '):
                    raise
            else:
                raise RuntimeError('Compact resource guard acquired a mapping')
            guards.append(dict(address=hex(address), walk=walk))
        return dict(address=hex(base), size=size, context=args.context, pages=pages, guards=guards)

    try:
        save('context-root-table', props['gpu-region-base'], 0x400)
        save('primary-status-b', 0xfffffc2000020340, 0x5000, 0)
        # The M5 hardware-event reader consumes all four records. Native
        # compute uses ordinal 0, so the old ordinal-2 prefix is insufficient.
        save('hardware-event-state', 0xfffffc20c0788000 + 0x2614, 0x80, 0)
        save('recovery-records', 0xfffffc20001c0000, 0x80, 0)
        # M5 0x5bb4/0x5bb8 reads the unaligned main+0x26c pointer,
        # the fourth owned bundle view (shared+0xbc80), for classification.
        save('recovery-classifier', 0xfffffc20c0793c80, 0x80, 0)
        for start, size in [(0x124000, 0xc000), (0x177000, 0x2000)]:
            save('gfx-data-%x' % start, props['gfx-data-base'] + start - 0x4c000, size)
        # M5 GFX firmware 0x17e18 uses main+0x1e0 for a control block
        # (consumer+0, producer+0x20) and main+0x1e8 for 512 x 0x48
        # records. These are report-channel views of the owned status A.
        for role, status in [('gfx', 0xfffffc200002ee40),
                             ('gfx1', 0xfffffc20000e3100)]:
            save(role + '-report-states', status, 0x300, 0)
            save(role + '-event-records', status + 0x2c0, 0x4800, 0)
            save(role + '-report-records', status + 0xa6ac0, 0x9000, 0)
        views = [
                ('uma-slot0', 0xfffffc20015e8000, 32, 0),
                ('event-state', 0xfffffc20c0405000, 0x70, 0),
                ('event-link', 0xfffffc20c0406000, 64, 0),
                ('descriptor', 0xfffffc20c0400000, 0x1000, 0),
                ('queue', 0xfffffc20c0000840, 0xc0, 0),
                ('queue-pointers', 0xfffffc20c0007000, 0x80, 0),
                ('scheduler', 0xfffffc20c0007200, 0xc0, 0),
                ('sksm-entry1', 0xfffffc2000390200, 512, 0),
                ('completion-prefix', 0xfffffc20c0838000, 0x100, 0),
                ('directory-prefix', 0x7000000000, 0x10000, 3),
                ('program-shader', 0x10000000000, 0x4000, 3),
                ('program-pipeline', 0x10000090000, 0x4000, 3),
                ('program-stream', 0x100000b0000, 0x4000, 3),
                ('program-preempt', 0x100000e0000, 0x4000, 3),
                ('program-robustness', 0x1000018000, 0x4000, 3),
                ('program-buffers', 0x1000001c000, 0x4000, 3)]
        if graphics_capture:
            views = [
                ('graphics-pixels', 0x10000058000, 0x40000, args.context),
                ('graphics-queues', 0xfffffc20c0000000, 0x1c000, 0),
                ('graphics-ta', 0xfffffc20c1000000, 0x3000, 0),
                ('graphics-fragment', 0xfffffc20c1004000, 0x3000, 0),
                ('graphics-scheduler', 0xfffffc20c1008000, 0x400, 0),
                ('graphics-pm-state', 0xfffffc20c100c000, 0x400, 0),
                ('graphics-usc-state', 0xfffffc20c101c000, 0x400, 0),
                ('graphics-stamps-a', 0xfffffc20c1084000 if args.graphics_shared_list else 0xfffffc20c1028000, 0x4000, 0),
                ('graphics-stamps-b', 0xfffffc20c1088000 if args.graphics_shared_list else 0xfffffc20c102c000, 0x4000, 0),
                ('graphics-pb-slot', 0xfffffc20015e0000, 16, 0),
                ('graphics-uma-slot', 0xfffffc20015e8020, 32, 0)]
            if args.graphics_shared_list:
                views += [('graphics-completion-list', 0xfffffc20c0020000, 0x4000, 0),
                          ('graphics-submitted-total', 0xfffffc20c1080000, 0x4000, 0)]
            if args.after_application_failure:
                application_base = 0x1000000000 if args.application_native_va else 0x4000000000
                views += [('application-vdm', application_base + 0x18000, 0x100, args.context),
                          ('application-ppp', application_base + 0x58000, 0x200, args.context)]
        for label, address, size, context in views:
            save(label, address, size, context)
        if args.graphics_parameter_buffer:
            state = walker.read(0xfffffc20c100c000, 0x90, context=0)
            scene = walker.read(0xfffffc20c1030080, 0x80, context=0)
            page_list_high, page_list = struct.unpack_from('<QQ', state, 0x20)
            block_table = struct.unpack_from('<Q', state, 0x44)[0]
            metrics, metrics_high = struct.unpack_from('<QQ', scene, 0)
            # Only the known 48-page/three-block profile is admitted here.
            if struct.unpack_from('<I', state, 0x34)[0] != 48:
                raise ValueError('Unexpected M5 parameter-buffer capacity')
            save('graphics-pb-blocks', block_table, 24, 0)
            save('graphics-pb-pages', page_list, 48 * 4, args.context)
            save('graphics-pb-pages-firmware', page_list_high, 48 * 4, 0)
            save('graphics-pb-scene', 0xfffffc20c1030080, 0x80, 0)
            save('graphics-pb-metrics', metrics & ~0x3fff, 0x100, args.context)
            save('graphics-pb-metrics-firmware', metrics_high & ~0x3fff, 0x100, 0)
            raw_blocks = walker.read(block_table, 24, context=0)
            blocks = [0x1000000000 + (struct.unpack_from('<I', raw_blocks, i * 8)[0] << 15)
                      for i in range(3)]
            scene_base = 0x1000000000 + struct.unpack_from('<Q', scene, 0x28)[0] - 0x40
            discard = 0x1000000000 + struct.unpack_from('<Q', state, 0x84)[0]
            spans = [(base, 0x80000) for base in blocks] + [
                (page_list, 0x1c000), (metrics & ~0x3fff, 0x4000),
                (scene_base, 0x4000), (discard, 0x8000)]
            manifest['graphics_pb_spans'] = [compact_span(base, size) for base, size in spans]
        if args.graphics_render_targets:
            ta = walker.read(0xfffffc20c1000000, 0x3000, context=0)
            fragment = walker.read(0xfffffc20c1004000, 0x3000, context=0)
            ta_regs = dict(struct.unpack_from('<IQ', ta, 0x60 + i * 12) for i in range(82))
            frag_regs = dict(struct.unpack_from('<IQ', fragment, 0xa0 + i * 12) for i in range(90))
            if frag_regs[0x15211] != (256 << 32) | 256:
                raise ValueError('Render-target capture expects the 256x256 reference')
            base = 0x1000000000
            targets = [
                ('tilemap', base + ta_regs[0x1c039], 0x4000, 0x1200),
                ('tpc', base + ta_regs[0x1c0a1], 0x4000, 0x100),
                ('deflake', base + (ta_regs[0x1c950] & 0xffffffff), 0x4000, 0x1000),
                ('ta-status', ta_regs[0x14318] & ~1, 0x4000, 0x100),
                ('fragment-status', frag_regs[0x14080] & ~1, 0x4000, 0x100),
                ('auxiliary', frag_regs[0x16461], 0x8000, 0x800)]
            manifest['graphics_target_spans'] = []
            for name, address, size, prefix in targets:
                span = compact_span(address, size)
                span['label'] = 'graphics-target-' + name
                manifest['graphics_target_spans'].append(span)
                save(span['label'], address, prefix, args.context)
        if args.graphics_usc_pool:
            state = walker.read(0xfffffc20c101c000, 0x70, context=0)
            directory = struct.unpack_from('<Q', state, 0x14)[0]
            freelist = struct.unpack_from('<Q', state, 0x30)[0]
            # Bound the private capture to the pool size actually allocated
            # by this candidate, never an untrusted firmware length.
            units = 19 * 512
            save('graphics-usc-directory', directory, units * 8 + 8, args.context)
            save('graphics-usc-free-blocks', freelist, 0x4000, args.context)
            entries = struct.unpack('<%dQ' % (units + 1),
                walker.read(directory, units * 8 + 8, context=args.context))
            guards = [directory + 0x400000, directory + 0x404000, freelist + 0x4000]
            for block in range(19):
                base = entries[block * 512]
                guards.extend((base + 0x200000, base + 0x204000))
            manifest['graphics_usc_guards'] = []
            for address in guards:
                walk = []
                def read_guard_entry(physical, length):
                    raw = read_phys(physical, length)
                    if length != 8:
                        raise ValueError('Unexpected guard walk read size')
                    walk.append([hex(physical), hex(int.from_bytes(raw, 'little'))])
                    return raw
                guard_walker = G17PUATReader(read_phys, props['gpu-region-base'],
                    read_root=read_guard_entry, read_table=read_guard_entry)
                try:
                    guard_walker.translate(args.context, address)
                except RuntimeError as error:
                    if not str(error).startswith('unmapped G17P UAT address '):
                        raise
                else:
                    raise RuntimeError('Graphics USC guard page acquired a mapping')
                manifest['graphics_usc_guards'].append(dict(
                    address=hex(address), context=args.context, walk=walk))
        if args.triangle:
            save('program-constants', 0x10000018000, 0x4000, 3)
        for ordinal in range(4):
            save('hardware-event-ring-%d' % ordinal,
                 0xfffffc20c07e8000 + ordinal * 0x28000,
                 0x4000 if graphics_capture else 0x20000, 0)
    finally:
        manifest['physical_pages_read'] = len(cache)
        (output_path / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
errors = [x['label'] for x in manifest['objects'] if 'error' in x]
print(json.dumps(dict(objects=len(manifest['objects']), physical_pages_read=len(cache),
                      failed_objects=errors, device_memory_read=False)))
if errors:
    raise SystemExit(1)
