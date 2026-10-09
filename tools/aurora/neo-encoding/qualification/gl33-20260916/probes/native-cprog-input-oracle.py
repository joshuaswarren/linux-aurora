#!/usr/bin/env python3
"""CPU-only native constant-program input copy oracle.

Emulates copyGprToCprog and the qualified COPY constructor/emitter. The only
modeled external operation is std::vector byte insertion: bytes produced by
the real emitter are captured and appended to an emulated output buffer.
This is an encoder oracle, not a GPU execution or complete EslBuilder model.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess

from unicorn.arm64_const import UC_ARM64_REG_X0, UC_ARM64_REG_X30, UC_ARM64_REG_PC

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('copy_oracle', HERE / 'native-ioto-copy-oracle.py')
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
CPROG, INSERT = 0x216b59054, 0x216b63608
BUILDER, ENCODER, BUFFER = base.HEAP + 0x3000, base.HEAP + 0x3100, base.HEAP + 0x4000


class CprogOracle(base.CopyOracle):
    def __init__(self):
        super().__init__()
        for ins in self.native.disassemble(CPROG, 0x216b5936c):
            self.instructions[ins.address] = ins
        self.functions.append(CPROG)
        self.records, self.constructors = [], []

    def hook(self, uc, address, size, opaque):
        ins = self.instructions.get(address)
        if address == base.CTOR:
            self.constructors.append([uc.reg_read(UC_ARM64_REG_X0 + i) for i in range(1, 5)])
        if ins and ins.mnemonic == 'bl' and int(ins.op_str.removeprefix('#'), 16) == INSERT:
            args = [uc.reg_read(UC_ARM64_REG_X0 + i) for i in range(5)]
            owner, destination, begin, end, length = args
            used = sum(len(record) for record in self.records)
            assert owner == ENCODER and destination == BUFFER + used
            assert 0 < length <= 10 and end - begin == length and used + length <= 32
            assert base.STACK <= begin <= end <= base.STACK + 0x20000
            record = bytes(uc.mem_read(begin, length))
            self.records.append(record)
            uc.mem_write(destination, record)
            uc.mem_write(ENCODER + 8, (destination + length).to_bytes(8, 'little'))
            uc.reg_write(UC_ARM64_REG_X30, address + 4)
            uc.reg_write(UC_ARM64_REG_PC, address + 4)
            self.steps += 1
            return
        super().hook(uc, address, size, opaque)

    def copy_inputs(self, destination_raw, mask, source_half=10, fast=False,
                    source_value=1):
        assert destination_raw >= 0 and destination_raw % 2 == 0
        assert mask in (1, 2, 3)
        assert source_value in (0, 1, 2, 3, 4, 5, 8)
        self.records, self.constructors, self.steps = [], [], 0
        self.uc.mem_write(BUILDER, bytes(0x300))
        self.uc.mem_write(BUILDER + 0x30, ENCODER.to_bytes(8, 'little'))
        self.uc.mem_write(ENCODER + 8, BUFFER.to_bytes(8, 'little'))
        self.uc.mem_write(ENCODER + 0x100, bytes([bool(fast)]))
        returned = self.call(CPROG, [BUILDER, source_half | 3 << 32,
                                     source_value, mask, destination_raw])
        return {'destination_raw': destination_raw, 'mask': mask,
                'source_half': source_half, 'source_value': source_value,
                'encoder_fast_flag': bool(fast),
                'returned': returned, 'constructors': self.constructors,
                'records': [record.hex() for record in self.records],
                'steps': self.steps}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    oracle = CprogOracle()
    cases = [oracle.copy_inputs(offset, mask, fast=fast)
             for fast in (False, True)
             for offset in (0, 2, 4, 8, 16, 32, 148, 150, 152, 156, 256)
             for mask in (1, 2, 3)]
    records = sorted({record for case in cases for record in case['records']})
    decoded = subprocess.check_output([str(HERE / 'decode-static-spill'), *records], text=True)
    decode_map = {item['hex']: item for item in map(json.loads, decoded.splitlines())}
    for case in cases:
        case['decoded'] = [decode_map[record] for record in case['records']]
        assert case['returned'] == case['mask'].bit_count() * 2
        uses_tps = not case['encoder_fast_flag'] or case['destination_raw'] >= 152
        if uses_tps:
            expected_tps = 8 + case['destination_raw'] // 2 - (76 if case['encoder_fast_flag'] else 0)
            expected_src = case['source_half'] + (case['mask'] == 2)
            for item in case['decoded']:
                assert item['kind'] == 'store' and item['replay_equal']
                assert item['tps_half'] == expected_tps and item['src'] == expected_src
                halves = item['width']  # W16=1, W32=2, explicitly checked below.
                assert halves in (1, 2)
                expected_tps += halves
                expected_src += halves
            assert expected_src - case['source_half'] == (2 if case['mask'] == 2 else case['mask'].bit_count())
        else:
            # The native constructor classifies this path as GPR destinations
            # (Register types 2/3); it must not masquerade as a TPS store.
            assert all(item['kind'] == 'outside_decoder_subset' for item in case['decoded'])
            assert all(((args[0] >> 32) & 0xffffffff) in (2, 3)
                       for args in case['constructors'])
    result = {'native_sha256': oracle.native.sha256,
              'script_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'base_oracle_sha256': hashlib.sha256((HERE / 'native-ioto-copy-oracle.py').read_bytes()).hexdigest(),
              'decoder_sha256': hashlib.sha256((HERE / 'decode-static-spill').read_bytes()).hexdigest(),
              'functions': [hex(x) for x in oracle.functions],
              'modeled_external_operation': 'byte-vector insertion only',
              'hardware_execution': False, 'cases': cases}
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'receipt': str(args.output), 'cases': len(cases),
                      'default_tps_mapping_verified': sum(not c['encoder_fast_flag'] for c in cases),
                      'unique_records': len(records)}))


if __name__ == '__main__':
    main()
