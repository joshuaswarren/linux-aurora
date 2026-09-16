#!/usr/bin/env python3
"""Emulate pinned native COPY encoder helpers only; never execute GPU code.

Requires the isolated .native-oracle-venv. Native image pages are read-only,
control flow is limited to the constructor/emitter and their local callees,
and the only writable memory is an emulated stack/object/output arena.
Pointer-authentication prologue/return instructions are modeled as ABI
bookkeeping; no OS driver, system call, or hardware interface is invoked.
"""
import argparse
import bisect
import json
from pathlib import Path

from unicorn import Uc, UC_ARCH_ARM64, UC_MODE_ARM, UC_HOOK_CODE
from unicorn import UC_PROT_ALL, UC_PROT_READ, UC_PROT_EXEC
from unicorn.arm64_const import UC_ARM64_REG_X0, UC_ARM64_REG_X29, UC_ARM64_REG_X30
from unicorn.arm64_const import UC_ARM64_REG_PC, UC_ARM64_REG_SP

from native_metal import NativeMetal

CTOR, EMIT = 0x216b61e24, 0x216b61fd8
STACK, HEAP, STOP = 0x60000000, 0x61000000, 0x62000000
OBJECT, OUTPUT, CANARY = HEAP + 0x100, HEAP + 0x2000, HEAP + 0x1ff00


class CopyOracle:
    def __init__(self):
        self.native = NativeMetal()
        symbols = self.native.symbols()
        starts = sorted(set(a for a, _ in symbols))
        self.instructions, self.functions = {}, []
        pending = [CTOR, EMIT]
        while pending:
            start = pending.pop()
            if start in self.functions:
                continue
            assert len(self.functions) < 16
            end = starts[bisect.bisect_right(starts, start)]
            instructions = self.native.disassemble(start, end)
            assert len(instructions) < 1000
            self.functions.append(start)
            for ins in instructions:
                self.instructions[ins.address] = ins
                if ins.mnemonic == 'bl':
                    target = int(ins.op_str.removeprefix('#'), 16)
                    # Stack-check failure is deliberately not implemented.
                    if target in starts and 0x216000000 <= target < 0x218000000:
                        pending.append(target)

        self.uc = Uc(UC_ARCH_ARM64, UC_MODE_ARM)
        regions = []
        for va, offset, size in self.native.segments:
            low, high = va & ~4095, (va + size + 4095) & ~4095
            regions.append((low, high))
        merged = []
        for low, high in sorted(regions):
            if merged and low <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(high, merged[-1][1]))
            else:
                merged.append((low, high))
        for low, high in merged:
            self.uc.mem_map(low, high - low, UC_PROT_ALL)
        for va, offset, size in self.native.segments:
            self.uc.mem_write(va, self.native.image[offset:offset + size])
        for low, high in merged:
            self.uc.mem_protect(low, high - low, UC_PROT_READ | UC_PROT_EXEC)
        self.uc.mem_map(STACK, 0x20000, UC_PROT_ALL)
        self.uc.mem_map(HEAP, 0x20000, UC_PROT_ALL)
        self.uc.mem_map(STOP, 4096, UC_PROT_READ | UC_PROT_EXEC)
        # The emit helper checks a canary imported through the shared cache.
        # Supply a constant in emulated memory, not any live process address.
        self.uc.mem_map(0x270154000, 4096, UC_PROT_ALL)
        self.uc.mem_write(0x270154c78, CANARY.to_bytes(8, 'little'))
        self.uc.mem_write(CANARY, bytes.fromhex('184f537928b6cad0'))
        self.uc.mem_protect(0x270154000, 4096, UC_PROT_READ)
        self.uc.hook_add(UC_HOOK_CODE, self.hook)
        self.steps = 0

    def hook(self, uc, address, size, _):
        if address == STOP:
            uc.emu_stop()
            return
        ins = self.instructions.get(address)
        if ins is None:
            raise ValueError(f'control flow left qualified helpers at {address:#x}')
        self.steps += 1
        if ins.mnemonic in ('pacibsp', 'paciasp', 'autibsp', 'autiasp'):
            uc.reg_write(UC_ARM64_REG_PC, address + 4)
        elif ins.mnemonic in ('retab', 'retaa'):
            uc.reg_write(UC_ARM64_REG_PC, uc.reg_read(UC_ARM64_REG_X30))

    def call(self, address, args):
        # Unicorn's ARM64 X0..X28 identifiers are consecutive. X29/X30 are not.
        for i in range(29):
            self.uc.reg_write(UC_ARM64_REG_X0 + i, 0)
        self.uc.reg_write(UC_ARM64_REG_X29, 0)
        for i, value in enumerate(args):
            self.uc.reg_write(UC_ARM64_REG_X0 + i, value)
        self.uc.reg_write(UC_ARM64_REG_SP, STACK + 0x1f000)
        self.uc.reg_write(UC_ARM64_REG_X30, STOP)
        self.uc.emu_start(address, STOP, count=10000)
        if self.uc.reg_read(UC_ARM64_REG_PC) != STOP:
            raise ValueError('helper did not return within instruction bound')
        return self.uc.reg_read(UC_ARM64_REG_X0)

    def encode(self, destination_type, destination_half, destination_value,
               source_type, source_half, source_value):
        assert 0 <= destination_half <= 0xffff and 0 <= source_half <= 0xffff
        assert 0 <= destination_type <= 9 and 0 <= source_type <= 9
        self.uc.mem_write(OBJECT, bytes(0x100))
        self.uc.mem_write(OUTPUT, bytes(0x100))
        self.steps = 0
        self.call(CTOR, [OBJECT, destination_half | destination_type << 32,
                         destination_value, source_half | source_type << 32,
                         source_value])
        encoded_object = bytes(self.uc.mem_read(OBJECT, 0x50)).hex()
        end = self.call(EMIT, [OBJECT, OUTPUT])
        assert OUTPUT < end <= OUTPUT + 32, hex(end)
        return {'hex': bytes(self.uc.mem_read(OUTPUT, end - OUTPUT)).hex(),
                'size': end - OUTPUT, 'object': encoded_object, 'steps': self.steps}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('registers', nargs=6, type=lambda value: int(value, 0),
                        help='destination type/half/value then source type/half/value')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    oracle = CopyOracle()
    result = {'native_sha256': oracle.native.sha256, 'registers': args.registers,
              'functions': [hex(x) for x in oracle.functions],
              'copy': oracle.encode(*args.registers), 'hardware_execution': False}
    if args.output:
        args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))
