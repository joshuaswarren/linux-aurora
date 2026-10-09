# SPDX-License-Identifier: MIT
"""Read bounded manager/configuration views for the pinned M5 graphics draw.

The caller supplies its existing translated-DRAM capture function. No address
is installed and no guest state is changed. These are reference observations,
not Linux ownership or rendering evidence.
"""
import struct


def word(data, offset):
    return struct.unpack_from('<I', data, offset)[0]


def quad(data, offset):
    return struct.unpack_from('<Q', data, offset)[0]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def capture_owners(save, raw, channel, context, stage):
    require(stage in ('ta', 'fragment'), 'Unsupported graphics stage')
    descriptor_size = 0x9c0 if stage == 'ta' else 0x2240
    require(len(raw) >= descriptor_size and len(channel) >= 0xc0,
            'Short graphics descriptor/channel')
    pointers = save('owner-channel-pointers', quad(channel, 0), 0x100)
    producer = word(pointers, 0x40)
    require(1 <= producer <= 16, 'Expected a fresh bounded graphics channel')
    items = save('owner-channel-items', quad(channel, 8), producer * 8)
    configs = []
    for index in range(producer):
        address = quad(items, index * 8)
        require(address != 0, 'Empty published channel item')
        record = save('owner-channel-item-%02d' % index, address, 0x100)
        if word(record, 0) != 15:
            continue
        configs.append(dict(index=index, address=hex(address),
            queue_id=struct.unpack_from('<H', record, 0x18)[0],
            install=word(record, 0x1a), priority=word(record, 0x1e),
            data_master=word(record, 0x22), context=word(record, 0x32),
            scheduler_update=word(record, 0x52), scheduler_index=word(record, 0x56),
            stamp_index=word(record, 0x5a), channel_data_master=word(record, 0x5e),
            context_update=word(record, 0x62), owner_context=word(record, 0x66),
            extended=word(record, 0x6a)))
        for offset in (0x36, 0x6e):
            event = quad(record, offset)
            if event:
                save('owner-channel-event-%02d-%02x' % (index, offset), event, 0x1000)
    require(len(configs) == 1, 'Expected exactly one current channel configuration')
    report = dict(channel_configs=configs, linux_ownership_proven=False)
    if stage != 'fragment':
        return report

    pm = save('owner-pm-state', quad(raw, 0x28), 0x1000)
    page_bytes = word(pm, 0x30)
    require(0 < page_bytes <= 0x80000 and page_bytes % 0x4000 == 0,
            'Invalid PM page-list extent')
    high = save('owner-pm-page-list-high', quad(pm, 0x20), page_bytes)
    low = save('owner-pm-page-list-user', quad(pm, 0x28), page_bytes, context=context)
    require(high == low, 'PM page-list aliases differ')
    # +0x7c is not a byte extent: using it crossed an unmapped page in the
    # native owner capture. Keep this an explicit first-page observation.
    block_address = quad(pm, 0x44)
    require(block_address != 0 and block_address % 8 == 0,
            'Invalid PM block-table address')
    block_bytes = 0x4000 - (block_address & 0x3fff)
    save('owner-pm-block-list', block_address, block_bytes)
    save('owner-pm-shared-control', quad(pm, 0x4c), 0x1000)
    save('owner-pm-counter', quad(pm, 0x64), 0x1000)
    report['pm'] = dict(hardware_id=word(pm, 0x0c), page_count=word(pm, 0x34),
        block_count=word(pm, 0x3c), block_bytes=word(pm, 0x58),
        page_list_bytes=page_bytes, block_table_bytes=block_bytes,
        block_capacity_field=word(pm, 0x38), page_capacity_field=word(pm, 0x7c))

    # These are the pinned 25G76 firmware-global descriptor tables already
    # identified by the M5 initialization/compute work. Save complete 16-KiB
    # views so interpretation is independently checkable after capture.
    save('owner-pb-descriptors', 0xfffffc20015e0000, 0x4000)
    uma = save('owner-uma-descriptors', 0xfffffc20015e8000, 0x4000)
    registers = dict(struct.unpack_from('<IQ', raw, 0xa0 + index * 12)
                     for index in range(90))
    usc_id = registers[0x14048]
    require(usc_id < 128, 'Reference has no bounded USC FList owner')
    values = struct.unpack_from('<4Q', uma, usc_id * 32)
    address = values[0] << 7
    populated = values[2] & 0xffffffff
    capacity = values[2] >> 32
    require(address != 0 and address < (1 << 42) and address % 0x4000 == 0
            and 0 < populated <= min(capacity, 0x10000) and capacity <= 0x80000,
            'USC descriptor differs from the measured M5 UMA format')
    save('owner-usc-directory-populated', address, populated * 8, context=context)
    report['usc'] = dict(hardware_id=usc_id, directory=hex(address),
        populated=populated, capacity=capacity,
        descriptor=[hex(value) for value in values])
    return report
