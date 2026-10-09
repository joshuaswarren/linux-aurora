#!/usr/bin/env python3
"""Verify native RLD scheduling-field provenance without compiling or using a GPU.

Runs only two pinned compiler leaf helpers and a pinned, named driver mapper in
ARM64 emulation. Native pages are read-only. This establishes a field mapping,
not the scheduling policy or hardware safety of enabling Mesa preambles.
"""
import argparse
import hashlib
import importlib.util
import itertools
import json
from pathlib import Path
import struct

from unicorn import Uc, UC_ARCH_ARM64, UC_MODE_ARM, UC_HOOK_CODE
from unicorn import UC_PROT_ALL, UC_PROT_READ, UC_PROT_EXEC
from unicorn.arm64_const import UC_ARM64_REG_X0, UC_ARM64_REG_X30, UC_ARM64_REG_PC

from native_metal import NativeMetal

spec = importlib.util.spec_from_file_location(
    'compiler_inspector', Path(__file__).with_name('inspect-native-compiler-code.py'))
inspector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inspector)

HELPER = (0x381198, 0x381254)
MAPPER = (0x40caac, 0x40cda0)
NATIVE_MAPPER = (0x2168171dc, 0x216817540)
HEAP, STOP = 0x60000000, 0x61000000
CONTEXT, INST, OPERANDS = HEAP, HEAP + 0x100, HEAP + 0x200
TYPED, FIELDS = HEAP + 0x400, HEAP + 0x1000


def read_va(image, segments, address, size):
    for va, offset, length in segments:
        if va <= address and address + size <= va + length:
            return image[offset + address - va:offset + address - va + size]
    raise ValueError(f'unmapped native bytes: {address:#x}+{size:#x}')


class LeafOracle:
    def __init__(self, image, segments, functions, constants=()):
        self.uc = Uc(UC_ARCH_ARM64, UC_MODE_ARM)
        self.functions = functions
        pages = set()
        for begin, end in list(functions) + list(constants):
            pages.update(range(begin & ~4095, (end + 4095) & ~4095, 4096))
        for page in sorted(pages):
            self.uc.mem_map(page, 4096, UC_PROT_ALL)
            self.uc.mem_write(page, read_va(image, segments, page, 4096))
            self.uc.mem_protect(page, 4096, UC_PROT_READ | UC_PROT_EXEC)
        self.uc.mem_map(HEAP, 0x10000, UC_PROT_ALL)
        self.uc.mem_map(STOP, 4096, UC_PROT_READ | UC_PROT_EXEC)
        self.uc.hook_add(UC_HOOK_CODE, self.hook)

    def hook(self, uc, address, size, opaque):
        if not any(begin <= address < end for begin, end in self.functions):
            raise ValueError(f'control flow escaped qualified leaf helpers: {address:#x}')

    def call(self, address, args):
        for i in range(29):
            self.uc.reg_write(UC_ARM64_REG_X0 + i, 0)
        for i, value in enumerate(args):
            self.uc.reg_write(UC_ARM64_REG_X0 + i, value)
        self.uc.reg_write(UC_ARM64_REG_X30, STOP)
        self.uc.emu_start(address, STOP, count=1000)
        assert self.uc.reg_read(UC_ARM64_REG_PC) == STOP

    def map(self, typed, address):
        self.uc.mem_write(TYPED, typed)
        self.uc.mem_write(FIELDS, bytes(0x520))
        self.call(address, [TYPED, FIELDS])
        return bytes(self.uc.mem_read(FIELDS, 0x518))


def verify():
    compiler, native = inspector.Compiler(), NativeMetal()
    segments = [(va, off, size) for _, va, _, off, size in compiler.segments if size]
    read = lambda a, n: read_va(compiler.image, segments, a, n)
    # Actual kind dispatch: table[kind], with kind 0x91 written by each of the
    # three RLD converters. The raw chained rebase word is pinned exactly;
    # this does not pretend arbitrary low bits are a general fixup decoder.
    dispatch = compiler.disassemble(0x3e1c18, 0x3e1c28)
    assert [(i['mnemonic'], i['operands']) for i in dispatch] == [
        ('adrp', 'x8, #0x48f7000'), ('add', 'x8, x8, #0xb90'),
        ('ldr', 'x0, [x8, w0, uxtw #3]'), ('ret', '')]
    assert struct.unpack('<Q', read(0x48f7b90 + 0x91 * 8, 8))[0] == 0x1000000040caac
    converters = []
    for start, kind_pc, args_pc, call_pc in [
        (0x383c3c, 0x383c64, 0x383ce8, 0x383d00),
        (0x383f58, 0x383f80, 0x384004, 0x38401c),
        (0x3840dc, 0x384104, 0x384188, 0x3841a0),
    ]:
        kind = compiler.disassemble(kind_pc, kind_pc + 8)
        assert [(i['mnemonic'], i['operands']) for i in kind] == [
            ('mov', 'w8, #0x91'), ('str', 'w8, [x2]')]
        args = compiler.disassemble(args_pc, args_pc + 12)
        assert [(i['mnemonic'], i['operands']) for i in args] == [
            ('add', 'x3, x25, #0x70'), ('add', 'x4, x25, #0x44'),
            ('add', 'x5, x25, #0x50')]
        assert compiler.disassemble(call_pc, call_pc + 4)[0]['operands'] == '#0x381198'
        converters.append(hex(start))
    names = [name for address, name in native.symbols()
             if address == NATIVE_MAPPER[0]]
    assert any('mapInstrSpecFields_rld' in name for name in names)
    oracle = LeafOracle(compiler.image, segments, [HELPER, MAPPER],
                        [(0x1b94b20, 0x1b94b60)])
    driver = LeafOracle(native.image, native.segments, [NATIVE_MAPPER])
    oracle.uc.mem_write(INST + 0x10, struct.pack('<Q', OPERANDS))
    cases, digest = 0, hashlib.sha256()
    examples = []
    shifts = (32, 36, 40, 46)
    metadata = {0, *[1 << bit for bit in range(64)]}
    metadata.update(sum(value << shift for value, shift in zip(values, shifts))
                    for values in itertools.product(range(4), repeat=4))
    # Add all dependency-wait bits to every hint tuple to verify separation.
    wait_bits = sum(1 << bit for bit in range(24, 32))
    metadata.update(value | wait_bits for value in tuple(metadata))
    for family in range(4):
        oracle.uc.mem_write(CONTEXT, struct.pack('<I', family))
        for flags in sorted(metadata):
            typed = bytearray(0x74)
            struct.pack_into('<I', typed, 0, family)
            struct.pack_into('<I', typed, 0xc, 2)  # GPR destination variant
            struct.pack_into('<I', typed, 0x28, 4)  # static TPS source variant
            struct.pack_into('<I', typed, 0x2c, 1)
            oracle.uc.mem_write(TYPED, bytes(typed))
            oracle.uc.mem_write(OPERANDS + 8, struct.pack('<Q', flags))
            oracle.call(HELPER[0], [CONTEXT, INST, 0, TYPED + 0x70,
                                     TYPED + 0x44, TYPED + 0x50])
            typed = bytes(oracle.uc.mem_read(TYPED, 0x74))
            fields = oracle.map(typed, MAPPER[0])
            assert fields == driver.map(typed, NATIVE_MAPPER[0]), (family, hex(flags))
            values = struct.unpack('<' + 'Q' * (len(fields) // 8), fields)
            expected = [0] * 8
            indices = (0, 1, 2) if family == 0 else (3, 4, 5, 6)
            for index, shift in zip(indices, shifts):
                expected[index] = flags >> shift & 3
            assert list(values[40:48]) == expected
            assert values[41 if family == 0 else 44] == flags >> 36 & 3
            assert list(values[30:38]) == [flags >> bit & 1 for bit in range(24, 32)]
            assert values[48] == flags >> 6 & 1
            assert values[23] == 4
            digest.update(struct.pack('<IQ', family, flags) + typed + fields)
            cases += 1
            if family in (0, 3) and flags in (1 << 36, 2 << 36):
                examples.append(dict(family=family, metadata=hex(flags),
                                     field=41 if family == 0 else 44,
                                     value=flags >> 36 & 3))
    return dict(compiler_sha256=compiler.sha256,
                compiler_arm64_sha256=hashlib.sha256(compiler.image).hexdigest(),
                native_driver_sha256=native.sha256,
                converters=converters, kind=0x91, mapper=hex(MAPPER[0]),
                helper=hex(HELPER[0]), named_driver_mapper=names,
                metadata_cases=len(metadata), comparisons=cases,
                all_compiler_and_named_driver_fields_identical=True,
                result_sha256=digest.hexdigest(), examples=examples,
                established='RLD field 41 (family 0) or 44 (families 1-3) is metadata bits 36:37; scheduling fields and wait fields are distinct.',
                not_established=['Scheduling hint placement policy',
                                 'Hardware meaning of hint values',
                                 'Safe Mesa preamble enablement', 'CTS conformance'],
                hardware_execution=False)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = verify()
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({key: result[key] for key in (
        'comparisons', 'all_compiler_and_named_driver_fields_identical',
        'result_sha256', 'hardware_execution')}))
