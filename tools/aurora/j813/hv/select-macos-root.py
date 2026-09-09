"""Select the full M5 macOS volume group in the guest ADT using a disk receipt."""
import hashlib
import json
import os
from pathlib import Path
import uuid


def select_macos_root(hv, path):
    if int(hv.adt['/chosen'].chip_id) != 0x8142 or not hv.j813_kernelcache_verified:
        raise RuntimeError('Root selection requires the verified M5 guest')
    raw = Path(path).read_bytes()
    data = json.loads(raw)
    if data.get('chip') != '0x8142' or not 0 < data.get('blocks_read', 0) <= 20000:
        raise RuntimeError('Not an M5 disk inventory')
    current = str(uuid.UUID(data['boot_uuid'])).upper()
    if str(uuid.UUID(data['apfs_preboot_uuid'])).upper() != current:
        raise RuntimeError('Inventory root selectors disagree')
    source = [v for v in data['volumes'] if v['role'] == 1 and
              str(uuid.UUID(v['volume_group_uuid'])).upper() == current]
    if len(source) != 1 or source[0]['integrity_meta_oid'] or source[0]['snapshots']:
        raise RuntimeError('The source was not identified as the unsealed boot stub')
    candidates = [v for v in data['volumes'] if v['role'] == 1 and
                  v['integrity_meta_oid'] and v['snapshots'] and v['files'] > 1000]
    if len(candidates) != 1:
        raise RuntimeError('Disk inventory does not identify one full macOS System volume')
    destination = str(uuid.UUID(candidates[0]['volume_group_uuid'])).upper()
    if destination == current or uuid.UUID(destination).int == 0:
        raise RuntimeError('Invalid destination macOS volume group')
    chosen = hv.adt['/chosen']
    for key in ('boot-uuid', 'apfs-preboot-uuid'):
        if str(uuid.UUID(chosen._properties[key])).upper() != current:
            raise RuntimeError('Current guest selectors do not match the disk inventory')
    for key in ('boot-uuid', 'apfs-preboot-uuid'):
        chosen._properties[key] = destination
    # HV.start() rebuilds and uploads this guest ADT after bootstrap scripts.
    # Other iBoot handoff inputs retain their original bytes. This identifies
    # the intended root; it does not assert that root authentication succeeds.
    hv.m5_root_inventory_sha256 = hashlib.sha256(raw).hexdigest()
    hv.m5_root_volume_group = destination
    print('M5_MACOS_ROOT_SELECTED', candidates[0]['name'], hv.m5_root_inventory_sha256)


if 'hv' in globals():
    select_macos_root(hv, os.environ['M5_MACOS_VOLUME_INVENTORY'])
