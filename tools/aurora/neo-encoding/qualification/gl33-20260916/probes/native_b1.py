"""Read-only access to the digest-pinned J700 25G83 B1 firmware image."""
import hashlib
from pathlib import Path
import struct
import zipfile
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM


class NativeB1:
    def __init__(self):
        ipsw = Path('/Users/mcruz/Library/iTunes/Mac Software Updates/UniversalMac_26.6.2_25G83_Restore.ipsw')
        with zipfile.ZipFile(ipsw) as archive:
            data = archive.read('Firmware/agx/armfw_g17p.im4p')
        assert hashlib.sha256(data).hexdigest() == '9b2afdfbf6c3d4b2892a1e4b98766d57a322c95a2a3a535403c50b2dbfa2955b'
        marker = data.index(b'rkosftab')
        count = struct.unpack_from('<I', data, marker + 8)[0]
        records = [struct.unpack_from('<4sIII', data, marker + 16 + i * 16) for i in range(count)]
        matches = [r for r in records if r[0] == b'b100']
        assert len(matches) == 1
        _, offset, size, _ = matches[0]
        self.image = data[marker - 32 + offset:marker - 32 + offset + size]
        assert hashlib.sha256(self.image).hexdigest() == 'c1d35018d1226f8b7db374b0cc4332c6e43479ad7a0d001c5a7d0ecda1354412'
        self.segments = []
        cursor = 32
        for _ in range(struct.unpack_from('<I', self.image, 16)[0]):
            command, length = struct.unpack_from('<II', self.image, cursor)
            if command == 0x19:
                va, _, file_offset, file_size = struct.unpack_from('<QQQQ', self.image, cursor + 24)
                self.segments.append((va - 0xfffffc0000000000, file_offset, file_size))
            cursor += length
        self.decoder = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

    def disassemble(self, start, end):
        assert start < end
        for va, file_offset, file_size in self.segments:
            if va <= start < end <= va + file_size:
                return list(self.decoder.disasm(self.image[file_offset + start - va:file_offset + end - va], start))
        raise ValueError('Unmapped firmware range')


class NativeAGX(NativeB1):
    """Read a fileset from the pinned kernelcache; defaults to the A18 GPU kext."""
    def __init__(self, fileset=b'com.apple.AGXG17P'):
        path = Path('/Users/mcruz/Developer/aurora-silicon/neo-update/kernel/25G83/kernelcache.release.mac17p.decompressed')
        self.image = path.read_bytes()
        assert hashlib.sha256(self.image).hexdigest() == 'b1ea68d23c452099da9fbfe06b19634e0175d4b2b1194f7249abac430a9a7a0d'
        cursor, matches = 32, []
        for _ in range(struct.unpack_from('<I', self.image, 16)[0]):
            command, length = struct.unpack_from('<II', self.image, cursor)
            if command == 0x80000035:
                _, offset, name_offset, _ = struct.unpack_from('<QQII', self.image, cursor + 8)
                name = self.image[cursor + name_offset:cursor + length].split(b'\0')[0]
                if name == fileset:
                    matches.append(offset)
            cursor += length
        assert len(matches) == 1
        base, = matches
        self.fileset_base = base
        cursor, self.segments = base + 32, []
        for _ in range(struct.unpack_from('<I', self.image, base + 16)[0]):
            command, length = struct.unpack_from('<II', self.image, cursor)
            if command == 0x19:
                va, _, file_offset, file_size = struct.unpack_from('<QQQQ', self.image, cursor + 24)
                self.segments.append((va, file_offset, file_size))
            cursor += length
        self.decoder = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

    def symbols(self):
        """Return defined symbols from this exact fileset's Mach-O table."""
        cursor = self.fileset_base + 32
        symtab = None
        for _ in range(struct.unpack_from('<I', self.image, self.fileset_base + 16)[0]):
            command, length = struct.unpack_from('<II', self.image, cursor)
            if command == 2:
                assert symtab is None
                symtab = struct.unpack_from('<IIII', self.image, cursor + 8)
            cursor += length
        assert symtab is not None
        offset, count, strings, strings_size = symtab
        assert offset + count * 16 <= len(self.image)
        assert strings + strings_size <= len(self.image)
        result = []
        for index in range(count):
            name_offset, kind, section, _, value = struct.unpack_from(
                '<IBBHQ', self.image, offset + index * 16)
            if kind & 0xe0 or kind & 0x0e != 0x0e or not section:
                continue
            assert name_offset < strings_size
            end = self.image.index(b'\0', strings + name_offset, strings + strings_size)
            name = self.image[strings + name_offset:end].decode()
            result.append((value, name))
        return sorted(result)
