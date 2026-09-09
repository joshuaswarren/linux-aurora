"""Read resident user pages through existing M5 25G76 16-KiB page tables.

No guest register changes, page faults, allocations or page-table writes.
Reject absent/compressed mappings; callers must record any missing pages.
"""
import struct
import hashlib
import json
import os
from pathlib import Path
import uuid


class M5UserMemory:
    def __init__(self, hv, pmap):
        if (hv.ctx is None or hv.ctx.spsr.M == 0 or
                int(hv.u.adt['/chosen'].chip_id) != 0x8142 or
                not getattr(hv, 'j813_kernelcache_verified', False)):
            raise RuntimeError('Requires paused exact M5 kernel mapping')
        self.hv = hv
        self.base = int(hv.u.adt['/chosen'].dram_base)
        self.end = self.base + int(hv.u.ba.mem_size_actual)
        self.root, self.low, self.high, attr = struct.unpack('<4Q', hv.readmem(pmap + 8, 32))
        geometry = hv.readmem(attr, 0x78)
        first, last = (struct.unpack_from('<I', geometry, offset)[0] for offset in (0x40, 0x48))
        page_size, page_shift = struct.unpack_from('<QQ', geometry, 0x58)
        if ((first, last, page_size, page_shift) != (1, 3, 0x4000, 14) or
                self.root & 0x3fff or not self.base <= self.root <= self.end - 0x4000 or
                not 0 <= self.low < self.high <= 1 << 47):
            raise RuntimeError('Unexpected native user pmap geometry')

    def physical_read(self, address, size):
        if not 0 < size <= 0x10000 or not self.base <= address < address + size <= self.end:
            raise RuntimeError('User memory translation falls outside DRAM')
        return self.hv.iface.readmem(address, size)

    def translate(self, address):
        if not self.low <= address < self.high:
            raise RuntimeError('User VA is outside this process pmap')
        page = self.root
        for shift in (36, 25, 14):
            pte, = struct.unpack('<Q', self.physical_read(page + ((address >> shift) & 0x7ff) * 8, 8))
            kind = pte & 3
            if kind == 3:
                page = pte & 0x0000ffffffffc000
                if shift == 14:
                    return page | (address & 0x3fff)
            elif kind == 1 and shift != 14:
                mask = (1 << shift) - 1
                return (pte & 0x0000ffffffffc000 & ~mask) | (address & mask)
            else:
                raise RuntimeError(f'User page is not resident at {address:#x}, level shift {shift}')
        raise RuntimeError('Incomplete user translation')

    def read(self, address, size):
        if not 0 < size <= 0x10000 or not self.low <= address < address + size <= self.high:
            raise RuntimeError('Invalid bounded user memory request')
        ranges = []
        current, left = address, size
        while left:
            length = min(left, 0x4000 - (current & 0x3fff))
            pa = self.translate(current)
            if not self.base <= pa < pa + length <= self.end:
                raise RuntimeError('User payload is outside DRAM')
            ranges.append((pa, length))
            current += length
            left -= length
        return b''.join(self.physical_read(pa, length) for pa, length in ranges)


def capture_user_image(reader, base, output):
    """Save resident Mach-O file-backed pages, explicitly recording holes."""
    header = reader.read(base, 0x4000)
    magic, cpu, subtype, kind, count, commands = struct.unpack_from('<6I', header)
    if (magic != 0xfeedfacf or cpu != 0x100000c or kind != 2 or
            not 0 < count <= 256 or not 0 < commands <= len(header) - 32):
        raise RuntimeError('Not a bounded native arm64 executable header')
    segments, image_uuid, cursor = [], None, 32
    for _ in range(count):
        command, size = struct.unpack_from('<II', header, cursor)
        if size < 8 or cursor + size > 32 + commands:
            raise RuntimeError('Malformed Mach-O load command')
        if command == 0x19:
            if size < 72:
                raise RuntimeError('Truncated Mach-O segment')
            name = header[cursor + 8:cursor + 24].split(b'\0')[0].decode('ascii')
            va, virtual_size, offset, file_size = struct.unpack_from('<4Q', header, cursor + 24)
            if file_size:
                if not file_size <= virtual_size or offset + file_size > 0x1000000:
                    raise RuntimeError('Executable segment exceeds capture bounds')
                segments.append(dict(name=name, va=va, offset=offset, size=file_size))
        elif command == 0x1b:
            image_uuid = str(uuid.UUID(bytes=header[cursor + 8:cursor + 24]))
        cursor += size
    text = [s for s in segments if s['name'] == '__TEXT' and s['offset'] == 0]
    if len(text) != 1 or image_uuid is None:
        raise RuntimeError('Ambiguous executable identity')
    slide = base - text[0]['va']
    image = bytearray(max(s['offset'] + s['size'] for s in segments))
    pages, holes = [], []
    for segment in segments:
        for offset in range(0, segment['size'], 0x4000):
            size = min(0x4000, segment['size'] - offset)
            va = slide + segment['va'] + offset
            record = dict(segment=segment['name'], va=hex(va),
                          offset=segment['offset'] + offset, size=size)
            try:
                data = reader.read(va, size)
            except RuntimeError as error:
                if 'not resident' not in str(error):
                    raise
                holes.append({**record, 'reason': str(error)})
                continue
            image[record['offset']:record['offset'] + size] = data
            pages.append({**record, 'sha256': hashlib.sha256(data).hexdigest()})
    output = Path(output)
    receipt = dict(uuid=image_uuid, load_address=hex(base), slide=hex(slide),
                   pmap_root=hex(reader.root), segments=segments, pages=pages,
                   missing_pages=holes, sha256=hashlib.sha256(image).hexdigest(),
                   note='Live memory reconstruction; loader relocations/runtime data retained; missing pages are zeros')
    for path, data in ((output, bytes(image)),
                       (output.with_suffix('.json'), json.dumps(receipt, indent=2).encode())):
        with open(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as handle:
            handle.write(data)
    print('M5_USER_IMAGE_CAPTURE', str(output), 'resident_pages', len(pages), 'missing_pages', len(holes))
    return output
