#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Check the measured M5 initial compute pool; does not qualify Linux execution."""
import argparse
import hashlib
import json
from pathlib import Path
import struct


def require(condition, message):
    if not condition:
        raise ValueError(message)


def verify(manifest, blobs):
    require(manifest.get('compute_context') == 3, 'Expected controlled M5 context3')
    require(manifest.get('completion', {}).get('same_descriptor'), 'Missing matching completion')
    state = blobs['compute-pool-event-state']
    require(len(state) == 0x70, 'Unexpected shared-control extent')
    directory, = struct.unpack_from('<Q', state, 0x14)
    hardware_id, = struct.unpack_from('<I', state, 8)
    command = blobs['payload-view-0']
    require(len(command) >= 0x220 and struct.unpack_from('<4H', command, 0x18) == (37, 39, 39, 40),
            'Unexpected compute register profile')
    register, selected_pool = struct.unpack_from('<IQ', command, 0x40 + 29 * 12)
    require(register == 0x14028 and selected_pool == hardware_id,
            'Compute register selects a different hardware pool')
    capacity, = struct.unpack_from('<I', state, 0x1c)
    counter_a, counter_b, empty, total = struct.unpack_from('<4I', state, 0x20)
    require(directory == 0x7000000000 and capacity == 0x80000,
            'Unexpected native directory geometry')
    require((counter_a, counter_b, empty, total) == (0, 7168, 0, 7168),
            'Expected initial controlled pool counters')
    require(manifest['compute_pool_uma']['buffer_id'] == hardware_id,
            'UMA slot does not belong to this shared-control record')
    expected = (directory >> 7 | empty << 61, counter_a | counter_b << 32,
                total | capacity << 32, 0)
    require(struct.unpack('<4Q', blobs['compute-pool-uma-slot']) == expected,
            'Descriptor does not use the measured M5 UMA packing')
    page_list = blobs['compute-pool-directory']
    require(len(page_list) == 0x10000, 'Expected captured directory prefix')
    for page in range(7168):
        expected_address = 0x7000448000 + (page // 512) * 0x208000 + (page % 512) * 4096
        require(struct.unpack_from('<Q', page_list, page * 8)[0] == expected_address,
                'Initial page directory differs at entry %d' % page)
    require(not any(page_list[7168 * 8:]), 'Unexpected entries beyond initial page count')
    edges = {int(x['offset'], 16): x for x in manifest['compute_pool_directory_edges']}
    require(all(edges[x].get('mapping') and 'error' not in edges[x]
                for x in (0xffff, 0x10000, 0x3fffff)), 'Directory mapping is incomplete')
    require(all('unmapped' in edges[x].get('error', '') and 'mapping' not in edges[x]
                for x in (0x400000, 0x404000)), 'Missing directory guard pages')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    manifest = json.loads((args.capture / 'manifest.json').read_text())
    blobs = {}
    for item in manifest['objects']:
        if item['label'] not in ('compute-pool-event-state', 'compute-pool-uma-slot',
                                 'compute-pool-directory', 'payload-view-0'):
            continue
        data = (args.capture / item['file']).read_bytes()
        require(item['stable'] and len(data) == item['size'], 'Unstable or short pool object')
        require(hashlib.sha256(data).hexdigest() == item['sha256'], 'Pool object hash mismatch')
        blobs[item['label']] = data
    verify(manifest, blobs)
    rejected = 0
    if args.self_test:
        # These mutations model the actual wrong initial-pool approaches:
        # empty directory, Neo descriptor packing, and absent allocation guards.
        mutations = []
        empty_directory = dict(blobs)
        empty_directory['compute-pool-directory'] = bytes(0x10000)
        mutations.append((manifest, empty_directory))
        neo_descriptor = dict(blobs)
        neo_descriptor['compute-pool-uma-slot'] = struct.pack('<4Q',
            (0x80000 << 41) | (0x7000000000 >> 7), 7168 << 33, 7168, 0)
        mutations.append((manifest, neo_descriptor))
        unguarded = json.loads(json.dumps(manifest))
        unguarded['compute_pool_directory_edges'][-1] = dict(offset='0x404000', mapping=[{}])
        mutations.append((unguarded, blobs))
        wrong_pool = dict(blobs)
        command = bytearray(blobs['payload-view-0'])
        struct.pack_into('<Q', command, 0x40 + 29 * 12 + 4,
                         struct.unpack_from('<I', blobs['compute-pool-event-state'], 8)[0] ^ 1)
        wrong_pool['payload-view-0'] = bytes(command)
        mutations.append((manifest, wrong_pool))
        for bad_manifest, bad_blobs in mutations:
            try:
                verify(bad_manifest, bad_blobs)
            except ValueError:
                rejected += 1
            else:
                raise RuntimeError('Invalid pool mutation was accepted')
    print(json.dumps(dict(initial_page_addresses_verified=7168,
        directory_allocation_bytes=0x400000, m5_uma_slot_verified=True, compute_pool_selection_verified=True,
        invalid_mutations_rejected=rejected, linux_execution_qualified=False)))


if __name__ == '__main__':
    main()
