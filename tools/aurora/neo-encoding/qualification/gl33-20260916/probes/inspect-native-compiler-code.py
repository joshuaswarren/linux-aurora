#!/usr/bin/env python3
"""Inspect the pinned compiler's ARM64 code and direct string references.

Read-only, offline analysis. No compiler execution, OS driver calls or GPU I/O.
Addresses refer to the arm64 Mach-O slice, not to a running process.
"""
import argparse
import bisect
import hashlib
import importlib.util
import json
from pathlib import Path
import struct

from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('schema', HERE / 'inspect-native-shader-schema.py')
schema = importlib.util.module_from_spec(spec)
spec.loader.exec_module(schema)


class Compiler:
    def __init__(self):
        self.path = schema.default_compiler()
        raw = self.path.read_bytes()
        self.sha256 = hashlib.sha256(raw).hexdigest()
        assert self.sha256 == schema.COMPILER_SHA256, 'Compiler changed; re-audit first'
        magic, count = struct.unpack_from('>II', raw)
        assert magic == 0xcafebabe and count < 8
        slices = [struct.unpack_from('>IIIII', raw, 8 + 20 * i) for i in range(count)]
        matches = [x for x in slices if x[0] == 0x100000c]
        assert len(matches) == 1
        _, self.cpu_subtype, offset, size, _ = matches[0]
        assert offset + size <= len(raw)
        self.image = raw[offset:offset + size]
        assert struct.unpack_from('<I', self.image)[0] == 0xfeedfacf
        self.segments, self.sections = [], []
        self.function_starts = None
        cursor = 32
        for _ in range(struct.unpack_from('<I', self.image, 16)[0]):
            command, length = struct.unpack_from('<II', self.image, cursor)
            assert length >= 8 and cursor + length <= len(self.image)
            if command == 0x19:
                name = self.image[cursor + 8:cursor + 24].rstrip(b'\0').decode()
                va, vmsize, file_offset, file_size = struct.unpack_from('<QQQQ', self.image, cursor + 24)
                assert file_offset + file_size <= len(self.image)
                self.segments.append((name, va, vmsize, file_offset, file_size))
                nsects = struct.unpack_from('<I', self.image, cursor + 64)[0]
                for i in range(nsects):
                    at = cursor + 72 + i * 80
                    assert at + 80 <= cursor + length
                    section = self.image[at:at + 16].rstrip(b'\0').decode()
                    address, extent, source = struct.unpack_from('<QQI', self.image, at + 32)
                    self.sections.append((name, section, address, extent, source))
            elif command == 0x26:
                assert self.function_starts is None
                self.function_starts = struct.unpack_from('<II', self.image, cursor + 8)
            cursor += length
        text = [s for s in self.sections if s[:2] == ('__TEXT', '__text')]
        assert len(text) == 1 and self.function_starts is not None
        _, _, self.text_va, self.text_size, self.text_offset = text[0]
        assert self.text_offset + self.text_size <= len(self.image)
        base = next(s[1] for s in self.segments if s[0] == '__TEXT')
        source, extent = self.function_starts
        assert source + extent <= len(self.image)
        self.starts = []
        value = shift = 0
        for byte in self.image[source:source + extent]:
            value |= (byte & 0x7f) << shift
            if byte & 0x80:
                shift += 7
                assert shift <= 63
                continue
            if value == 0:
                break
            base += value
            assert self.text_va <= base < self.text_va + self.text_size
            self.starts.append(base)
            value = shift = 0
        assert self.starts and self.starts == sorted(set(self.starts))

    def va(self, offset):
        for _, address, _, source, size in self.segments:
            if source <= offset < source + size:
                return address + offset - source
        raise ValueError('Unmapped file offset')

    def bounds(self, address):
        i = bisect.bisect_right(self.starts, address) - 1
        assert i >= 0
        return self.starts[i], (self.starts[i + 1] if i + 1 < len(self.starts)
                                else self.text_va + self.text_size)

    def string_references(self, text):
        needle = text.encode() + b'\0'
        offsets, start = [], 0
        while True:
            at = self.image.find(needle, start)
            if at < 0:
                break
            offsets.append(at)
            start = at + 1
        targets = {self.va(o) for o in offsets}
        pages = {a & ~4095 for a in targets}
        code = self.image[self.text_offset:self.text_offset + self.text_size]
        records = []
        for i in range(0, len(code) - 3, 4):
            word = struct.unpack_from('<I', code, i)[0]
            if word & 0x9f000000 != 0x90000000:  # ADRP
                continue
            immediate = ((word >> 5 & 0x7ffff) << 2) | (word >> 29 & 3)
            if immediate & (1 << 20):
                immediate -= 1 << 21
            pc = self.text_va + i
            page = (pc & ~4095) + (immediate << 12)
            if page not in pages:
                continue
            register = word & 31
            for j in range(i + 4, min(i + 36, len(code) - 3), 4):
                add = struct.unpack_from('<I', code, j)[0]
                if add & 0xff800000 != 0x91000000 or (add >> 5 & 31) != register:
                    continue
                value = page + ((add >> 10 & 0xfff) << (12 if add & (1 << 22) else 0))
                if value in targets:
                    begin, end = self.bounds(pc)
                    records.append(dict(adrp=hex(pc), add=hex(self.text_va + j),
                                        string=hex(value), function=hex(begin),
                                        function_end=hex(end)))
        return dict(text=text, string_addresses=[hex(x) for x in sorted(targets)],
                    candidate_references=records,
                    scope='ADRP/ADD candidates within eight following instructions; inspect register lifetime before treating as a proven reference')

    def disassemble(self, begin, end):
        assert self.text_va <= begin < end <= self.text_va + self.text_size
        start = self.text_offset + begin - self.text_va
        return [dict(address=hex(i.address), mnemonic=i.mnemonic, operands=i.op_str)
                for i in Cs(CS_ARCH_ARM64, CS_MODE_ARM).disasm(self.image[start:start + end - begin], begin)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    choice = parser.add_mutually_exclusive_group(required=True)
    choice.add_argument('--string')
    choice.add_argument('--function', type=lambda s: int(s, 0))
    parser.add_argument('--limit', type=int, default=80)
    parser.add_argument('--export-function', type=Path,
                        help='Save this function alone for disassembly at its original address')
    parser.add_argument('--through-function', type=lambda s: int(s, 0),
                        help='Extend the export through this subsequent function, preserving addresses')
    args = parser.parse_args()
    assert 1 <= args.limit <= 1000
    assert not args.export_function or args.function is not None
    assert args.through_function is None or args.export_function
    compiler = Compiler()
    result = dict(compiler_path=str(compiler.path), compiler_sha256=compiler.sha256,
                  arm64_sha256=hashlib.sha256(compiler.image).hexdigest(),
                  function_count=len(compiler.starts), hardware_execution=False)
    if args.string:
        result.update(compiler.string_references(args.string))
    else:
        begin, end = compiler.bounds(args.function)
        result.update(function=hex(begin), function_end=hex(end),
                      instructions=compiler.disassemble(begin, end)[:args.limit])
        if args.export_function:
            export_end = end
            if args.through_function is not None:
                assert args.through_function >= begin
                _, export_end = compiler.bounds(args.through_function)
            offset = compiler.text_offset + begin - compiler.text_va
            data = compiler.image[offset:offset + export_end - begin]
            with args.export_function.open('xb') as target:
                target.write(data)
            result.update(export_path=str(args.export_function.resolve()),
                          export_sha256=hashlib.sha256(data).hexdigest(),
                          export_base_address=hex(begin), export_size=len(data),
                          export_end_address=hex(export_end),
                          export_functions=[hex(x) for x in compiler.starts
                                            if begin <= x < export_end])
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
