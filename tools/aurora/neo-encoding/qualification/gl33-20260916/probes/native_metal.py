"""Read-only access to a digest-pinned A18-family user-mode Metal binary.

Extracted from this host's macOS 27 dyld cache, not captured from the Neo.
Bundle version 360.34.5; DTPlatformBuild 26A5412a. This provides native HAL200
encoder evidence, not a claim of runtime qualification on the J700 firmware.
"""

import hashlib
from pathlib import Path
import struct

from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

from native_b1 import NativeB1


class NativeMetal(NativeB1):
    def __init__(self):
        path = Path(__file__).parent / 'native-metal-host27' / 'AGXMetalG17P'
        self.image = path.read_bytes()
        self.sha256 = hashlib.sha256(self.image).hexdigest()
        assert self.sha256 == (
            '666816a37045b6dcd351b63ad56a1f5ce284f3acb2157262226e090ade9cfca2'
        )
        magic, cpu = struct.unpack_from('<II', self.image)
        assert magic == 0xfeedfacf and cpu == 0x100000c
        cursor, self.segments, self.symtab = 32, [], None
        for _ in range(struct.unpack_from('<I', self.image, 16)[0]):
            command, length = struct.unpack_from('<II', self.image, cursor)
            assert length >= 8 and cursor + length <= len(self.image)
            if command == 0x19:
                va, _, file_offset, file_size = struct.unpack_from(
                    '<QQQQ', self.image, cursor + 24)
                assert file_offset + file_size <= len(self.image)
                self.segments.append((va, file_offset, file_size))
            elif command == 2:
                assert self.symtab is None
                self.symtab = struct.unpack_from('<IIII', self.image, cursor + 8)
            cursor += length
        assert self.symtab is not None
        self.decoder = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

    def symbols(self):
        offset, count, strings, strings_size = self.symtab
        assert offset + count * 16 <= len(self.image)
        assert strings + strings_size <= len(self.image)
        result = []
        for index in range(count):
            name_offset, kind, section, _, value = struct.unpack_from(
                '<IBBHQ', self.image, offset + index * 16)
            if kind & 0xe0 or kind & 0x0e != 0x0e or not section:
                continue
            assert name_offset < strings_size
            end = self.image.index(b'\0', strings + name_offset,
                                   strings + strings_size)
            name = self.image[strings + name_offset:end].decode()
            result.append((value, name))
        return sorted(result)

    def function(self, name_fragment):
        symbols = self.symbols()
        matches = [(i, address, name) for i, (address, name) in enumerate(symbols)
                   if name_fragment in name and name.startswith('__ZN')]
        assert len(matches) == 1, matches
        index, start, name = matches[0]
        end = next(address for address, _ in symbols[index + 1:]
                   if address > start)
        return name, self.disassemble(start, end)


if __name__ == '__main__':
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('symbol', help='unique mangled function-name fragment')
    parser.add_argument('--offset', type=int, default=0)
    parser.add_argument('--limit', type=int, default=32)
    args = parser.parse_args()
    reader = NativeMetal()
    name, instructions = reader.function(args.symbol)
    print(reader.sha256, name, 'instructions', len(instructions))
    for instruction in instructions[args.offset:args.offset + args.limit]:
        print(f'{instruction.address:#x}: {instruction.mnemonic} '
              f'{instruction.op_str}')
