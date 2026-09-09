"""Read M5 GPT/APFS volume metadata through the normal m1n1 USB proxy.

Only namespace reads are issued after initializing the m1n1 NVMe driver.
Run after leaving/rebooting the hypervisor, with exclusive proxy ownership.
The OMAP layout follows m1n1/rust/src/apfs.rs; newer checkpoint keys win.
"""
import json
import os
from pathlib import Path
import struct
import sys
import time
import uuid
import zlib

ROOT = Path(__file__).resolve().parent
SECTOR = 4096


def u16(data, offset):
    return struct.unpack_from('<H', data, offset)[0]


def u32(data, offset):
    return struct.unpack_from('<I', data, offset)[0]


def u64(data, offset):
    return struct.unpack_from('<Q', data, offset)[0]


def object_valid(data):
    if len(data) != SECTOR:
        return False
    mod = 0xffffffff
    s1 = s2 = 0
    for value in struct.unpack('<1022I', data[8:]):
        s1 = (s1 + value) % mod
        s2 = (s2 + s1) % mod
    c1 = mod - ((s1 + s2) % mod)
    c2 = mod - ((s1 + c1) % mod)
    return u64(data, 0) == (c2 << 32) | c1


def partitions(read):
    header = read(1)
    if header[:8] != b'EFI PART':
        raise RuntimeError('Missing 4-KiB GPT header')
    size = u32(header, 12)
    if not 92 <= size <= SECTOR:
        raise RuntimeError('Invalid GPT header size')
    check = bytearray(header[:size])
    check[16:20] = b'\0' * 4
    if zlib.crc32(check) != u32(header, 16):
        raise RuntimeError('GPT header checksum mismatch')
    lba, count, stride = u64(header, 72), u32(header, 80), u32(header, 84)
    if not 0 < count <= 4096 or stride != 128:
        raise RuntimeError('Unexpected GPT entry layout')
    data = b''.join(read(lba + i) for i in range((count * stride + SECTOR - 1) // SECTOR))
    data = data[:count * stride]
    if zlib.crc32(data) != u32(header, 88):
        raise RuntimeError('GPT entry checksum mismatch')
    for i in range(count):
        entry = data[i * stride:(i + 1) * stride]
        if entry[:16] == b'\0' * 16:
            continue
        start, end = u64(entry, 32), u64(entry, 40)
        if start > end or end > u64(header, 48):
            raise RuntimeError('GPT partition outside usable disk')
        yield dict(index=i + 1, start=start, end=end,
                   type=str(uuid.UUID(bytes_le=entry[:16])),
                   name=entry[56:128].decode('utf-16-le').split('\0')[0])


def volumes(read, partition):
    def block(index, checked=True):
        if not 0 <= index <= partition['end'] - partition['start']:
            raise RuntimeError('APFS block outside container')
        data = read(partition['start'] + index)
        if checked and not object_valid(data):
            raise RuntimeError(f'APFS object checksum mismatch at {index}')
        return data
    first = block(0, checked=False)
    if first[32:36] != b'NXSB':
        return []
    if not object_valid(first) or u32(first, 36) != SECTOR:
        raise RuntimeError('Invalid APFS container superblock')
    count, start = u32(first, 104), u64(first, 112)
    if not 0 < count <= 8192:
        raise RuntimeError('Unsupported APFS checkpoint descriptor layout')
    candidates = [first]
    for i in range(count):
        data = block(start + i, checked=False)
        if data[32:36] == b'NXSB' and object_valid(data):
            candidates.append(data)
    superblock = max(candidates, key=lambda data: u64(data, 16))
    xid = u64(superblock, 16)
    ids = {u64(superblock, 184 + 8 * i) for i in range(100)} - {0}
    omap = block(u64(superblock, 160))
    pending = [u64(omap, 48)]
    seen, mappings = set(), {}
    while pending:
        address = pending.pop()
        if address in seen or len(seen) >= 4096:
            raise RuntimeError('OMAP cycle or node bound exceeded')
        seen.add(address)
        node = block(address)
        flags, level, count = u16(node, 32), u16(node, 34), u32(node, 36)
        if not flags & 4 or count > 1000 or level > 8:
            raise RuntimeError('Unsupported OMAP node layout')
        table = 56 + u16(node, 40)
        key_start = table + u16(node, 42)
        val_end = SECTOR - (40 if flags & 1 else 0)
        for i in range(count):
            if table + 4 * i + 4 > key_start:
                raise RuntimeError('OMAP table bounds')
            key = key_start + u16(node, table + 4 * i)
            value = val_end - u16(node, table + 4 * i + 2)
            if not key_start <= key <= val_end - 16 or not key_start <= value <= val_end - (8 if level else 16):
                raise RuntimeError('OMAP record bounds')
            if level:
                pending.append(u64(node, value))
            else:
                oid, version = u64(node, key), u64(node, key + 8)
                if oid in ids and version <= xid and version > mappings.get(oid, (-1, 0, 0))[0]:
                    mappings[oid] = (version, u64(node, value + 8), u32(node, value))
    result = []
    for oid in sorted(ids):
        if oid not in mappings or mappings[oid][2] & 1:
            continue
        data = block(mappings[oid][1])
        if data[32:36] != b'APSB' or u64(data, 8) != oid:
            raise RuntimeError('APFS volume superblock identity mismatch')
        result.append(dict(partition=partition['index'], oid=oid,
                           uuid=str(uuid.UUID(bytes=data[240:256])),
                           volume_group_uuid=str(uuid.UUID(bytes=data[1008:1024])),
                           name=data[704:960].split(b'\0')[0].decode('utf-8'),
                           role=u16(data, 964), allocated_blocks=u64(data, 88),
                           files=u64(data, 184), directories=u64(data, 192),
                           snapshots=u64(data, 216), fs_flags=hex(u64(data, 264)),
                           root_to_xid=u64(data, 968), integrity_meta_oid=u64(data, 1024)))
    return result


def main():
    sys.path.insert(0, str(ROOT / 'm1n1/proxyclient'))
    from m1n1.proxy import UartInterface, M1N1Proxy
    from m1n1.proxyutils import ProxyUtils, bootstrap_port
    device = '/dev/cu.usbmodemH4X2LL69WK1'
    iface = UartInterface(device)
    proxy = M1N1Proxy(iface)
    bootstrap_port(iface, proxy)
    if proxy.get_chipid() != 0x8142:
        raise RuntimeError('Volume inventory requires the M5 target')
    util = ProxyUtils(proxy)
    if util.adt['/arm-io'].compatible[0] != 'arm-io,t8142':
        # Chip ID above is authoritative; retain platform evidence in output.
        print('M5_IO_COMPAT', list(util.adt['/arm-io'].compatible))
    if not proxy.nvme_init():
        raise RuntimeError('m1n1 NVMe initialization failed')
    buffer = proxy.memalign(SECTOR, SECTOR)
    if not buffer or buffer & (SECTOR - 1):
        raise RuntimeError('NVMe buffer allocation failed')
    cache = {}
    def read(lba):
        if lba not in cache:
            if len(cache) >= 20000:
                raise RuntimeError('Inventory read limit reached')
            if not proxy.nvme_read(1, lba, buffer):
                raise RuntimeError(f'NVMe read failed at LBA {lba}')
            cache[lba] = iface.readmem(buffer, SECTOR)
        return cache[lba]
    try:
        parts = list(partitions(read))
        result = dict(chip='0x8142', boot_uuid=str(util.adt['/chosen'].boot_uuid),
                      apfs_preboot_uuid=str(util.adt['/chosen']._properties.get('apfs-preboot-uuid')),
                      partitions=parts, volumes=[])
        for part in parts:
            result['volumes'].extend(volumes(read, part))
        result['blocks_read'] = len(cache)
        directory = ROOT / 'logs/volume-inventory'
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        path = directory / f'm5-{time.time_ns()}.json'
        with open(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as out:
            json.dump(result, out, indent=2)
        print('M5_VOLUME_INVENTORY', str(path))
    finally:
        proxy.free(buffer)
        iface.dev.close()


if __name__ == '__main__':
    main()
