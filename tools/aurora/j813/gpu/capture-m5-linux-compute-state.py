#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Read retained M5 compute RAM after the bounded probe stopped both CPUs.

Never reads MMIO. Physical pages must belong to Linux System RAM or one of
this boot's measured GPU reserved-RAM ranges. Results are physical-memory
observations, not claims about contents held in firmware-private caches.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(os.environ.get('M1N1_DIR', root / 'hv/m1n1')) / 'proxyclient'))
from m1n1.proxy import Serial
from m1n1.agx.g17p_trace import G17PUATReader

inventory_path, output_path = map(Path, sys.argv[1:])
inventory = json.loads(inventory_path.read_text())
if inventory['model'] != 'Mac17,3':
    raise ValueError('Expected this boot M5 inventory')
props = inventory['nodes']['/arm-io/sgx']['properties']
reserved = [(props[name + '-base'], props[name + '-base'] + props[name + '-size'])
            for name in ('gpu-region', 'gfx-shared-region', 'gfx-shared-l2-region',
                         'gfx-data', 'gfx1-data')]
output_path.mkdir(mode=0o700, exist_ok=False)
manifest = dict(phase='Linux mode6 after both firmware CPU stop requests',
                device_memory_read=False, objects=[])
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

    identity = command("grep -q apple,j813 /proc/device-tree/compatible "
        "&& [ $(uname -r) = 7.1.6-aurora-j813-gpu-v9 ] "
        "&& [ -d /sys/module/asahi ] "
        "&& dmesg | grep -q 'requested probe mode=6' "
        "&& dmesg | grep -q 'M5_INIT stop requests=Ok(())' "
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

    try:
        save('context-root-table', props['gpu-region-base'], 0x400)
        for start, size in [(0x124000, 0xc000), (0x177000, 0x2000)]:
            save('gfx-data-%x' % start, props['gfx-data-base'] + start - 0x4c000, size)
        for label, address, size, context in [
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
                ('program-buffers', 0x1000001c000, 0x4000, 3)]:
            save(label, address, size, context)
    finally:
        manifest['physical_pages_read'] = len(cache)
        (output_path / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
errors = [x['label'] for x in manifest['objects'] if 'error' in x]
print(json.dumps(dict(objects=len(manifest['objects']), physical_pages_read=len(cache),
                      failed_objects=errors, device_memory_read=False)))
