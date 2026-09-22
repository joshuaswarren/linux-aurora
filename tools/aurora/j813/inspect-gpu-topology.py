#!/usr/bin/env python3
"""Read only selected GPU topology properties from m1n1's already-loaded ADT."""
import json
import os
from pathlib import Path
import time


def capture_gpu_topology(u, output):
    assert u.adt.model == 'Mac17,3'
    selected = ('compatible', 'interrupts', 'interrupts-valid', 'clock-gates',
                'power-gates', 'clock-id', 'gpu-count', 'gpu-core-count',
                'gpu-core-mask', 'gpu-gen', 'gfx-handoff', 'gfx-shared-region',
                'gpu-region', 'ps-regs', 'ps-groups', 'uat-vaddr-size',
                'agx-address-space-mgmt-mode')
    selected += tuple(prefix + suffix for prefix in (
        'gpu-region', 'gfx-shared-region', 'gfx-shared-l2-region',
        'gfx-handoff', 'gfx-data', 'gfx1-data') for suffix in ('-base', '-size'))

    def plain(value):
        if isinstance(value, (str, int, float, bool)) or value is None:
            return value
        if isinstance(value, bytes):
            if len(value) > 4096:
                return {'bytes': len(value), 'content_omitted': True}
            return {'hex': value.hex()}
        if isinstance(value, dict):
            return {str(k): plain(v) for k, v in value.items() if not str(k).startswith('_')}
        if isinstance(value, (list, tuple)):
            return [plain(v) for v in value]
        return {'type': type(value).__name__}

    result = {'model': u.adt.model, 'source': 'live m1n1 ADT; no MMIO accesses', 'nodes': {}}
    for path in ('/defaults', '/arm-io/sgx', '/arm-io/gfx-asc', '/arm-io/gfx1-asc',
                 '/arm-io/pmgr', '/arm-io/dart-gfx', '/arm-io/dart-gfx1',
                 '/arm-io/aic', '/arm-io/uart0', '/arm-io/mtp',
                 '/arm-io/dart-mtp', '/arm-io/dockchannel-mtp'):
        try:
            node = u.adt[path]
        except KeyError:
            result['nodes'][path] = {'present': False}
            continue
        entry = {'present': True, 'property_names': sorted(node._properties)}
        if 'reg' in node._properties:
            entry['raw_regs'] = plain(node.reg)
            entry['translated_regs'] = [{'address': hex(a), 'size': hex(s)}
                                      for a, s in (node.get_reg(i) for i in range(len(node.reg)))]
        entry['properties'] = {key: plain(node._properties[key]) for key in selected
                               if key in node._properties}
        if path.endswith('/pmgr') and 'devices' in node._properties:
            entry['gpu_devices'] = [plain(d) for d in node._properties['devices']
                                    if str(d.get('name', '')).startswith(('GFX', 'SGX', 'GPX'))]
            u.adt.pmgr_init()
            entry['gpu_power_registers'] = [
                {'name': d.name, 'id': u.adt.pmgr_dev_get_id(d),
                 'parents': list(u.adt.pmgr_dev_get_parents(d)),
                 'address': hex(u.adt.pmgr_dev_get_addr(d)),
                 'block': [hex(x) for x in u.adt.pmgr_dev_get_block(d)]}
                for d in node.devices if str(d.name).startswith(('GFX', 'SGX', 'GPX'))
                and not d.flags.no_ps]
        result['nodes'][path] = entry
    name = 'gpu-topology-live-' + time.strftime('%Y%m%dT%H%M%SZ', time.gmtime()) + '.json'
    path = Path(output) / name
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as stream:
        json.dump(result, stream, indent=2)
        stream.write('\n')
    print('J813_GPU_ADT_INVENTORY_SAVED', path.name, flush=True)
    return path
