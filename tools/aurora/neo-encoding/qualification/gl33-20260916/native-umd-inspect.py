#!/usr/bin/env python3
"""Read-only disassembly of selected functions in the extracted local G17P UMD."""
import argparse
from pathlib import Path
import re
import struct
import subprocess
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

base = Path(__file__).resolve().parent
path = base/'native-umd-current/AGXMetalG17P'
data = path.read_bytes()
assert data[:4] == bytes.fromhex('cffaedfe')
segments = []
pos = 32
for _ in range(struct.unpack_from('<I', data, 16)[0]):
    cmd, size = struct.unpack_from('<II', data, pos)
    if cmd == 0x19:
        vm, length, off, file_length = struct.unpack_from('<QQQQ', data, pos+24)
        segments.append((vm, off, file_length))
    pos += size

nm = subprocess.run(['xcrun','nm','-C',str(path)], check=True,
                    capture_output=True,text=True).stdout
symbols = []
for line in nm.splitlines():
    match = re.match(r'([0-9a-f]+) [tT] (.+)', line)
    if match:
        symbols.append((int(match[1],16), match[2]))
symbols.sort()
names = dict(symbols)

def read(addr, length):
    for vm, off, file_length in segments:
        if vm <= addr and addr+length <= vm+file_length:
            return data[off+addr-vm:off+addr-vm+length]
    raise ValueError(f'unmapped code range {addr:x}+{length:x}')

parser = argparse.ArgumentParser()
parser.add_argument('match', help='Substring of demangled function name')
args = parser.parse_args()
dis = Cs(CS_ARCH_ARM64, CS_MODE_ARM)
for index, (addr, name) in enumerate(symbols):
    if args.match not in name:
        continue
    next_addr = next(a for a,n in symbols[index+1:] if a > addr)
    print(f'\n{name} @ {addr:#x}, size {next_addr-addr:#x}')
    for insn in dis.disasm(read(addr, next_addr-addr),addr):
        comment = ''
        if insn.mnemonic in ('b','bl') and insn.op_str.startswith('#0x'):
            target = int(insn.op_str[1:],16)
            comment = ' // ' + names.get(target, '')
        print(f'{insn.address:012x}: {insn.mnemonic:8s} {insn.op_str}{comment}')
