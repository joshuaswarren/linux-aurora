#!/usr/bin/env python3
"""Disassemble selected AVE command paths with direct-call symbol labels."""
import bisect
import argparse
import json
from pathlib import Path
import re
import capstone

parser = argparse.ArgumentParser()
parser.add_argument('root', type=Path)
parser.add_argument('--match', help='Regular expression selecting demangled function names')
parser.add_argument('--output', default='command-paths.asm')
args = parser.parse_args()
root = args.root
manifest = json.loads((root/'manifest.json').read_text())
symbols = json.loads((root/'functions.json').read_text())
symbols.sort(key=lambda x: int(x['address'],16))
addresses = sorted(set(int(x['address'],16) for x in symbols))
names = {int(x['address'],16):x['demangled'] for x in symbols}
segment = next(x for x in manifest['segments'] if x['name']=='__TEXT_EXEC')
base = int(segment['address'],16)
data = (root/'__TEXT_EXEC.bin').read_bytes()
decoder = capstone.Cs(capstone.CS_ARCH_ARM64,capstone.CS_MODE_ARM)
decoder.detail = True
selection = [x for x in symbols if ((x['demangled'].startswith('AVE_HwC::') and
             any(key in x['demangled'] for key in ['SendFwCmd','MakeFwCmd','StartUpIOP',
                                                   'ProcessIntr_IPCCh','ProcessIntr_CmdAck'])) or
             x['demangled'].startswith('AVE_CHM_MakeFwCmd_') or
             x['demangled'].startswith('AVE_Surface::GetKernelAddr(') or
             x['demangled'].startswith('AVE_Surface::GetSize(') or
             x['demangled'].startswith('AVE_IPC::') or
             x['demangled'].startswith('AppleAVEIOProcessorChannel::')) and
             '.cold.' not in x['demangled']]
if args.match:
    pattern = re.compile(args.match)
    selection = [x for x in symbols if pattern.search(x['demangled']) and '.cold.' not in x['demangled']]
output = root/args.output
with output.open('w') as stream:
    for function in selection:
        start = int(function['address'],16)
        index = bisect.bisect_right(addresses,start)
        end = addresses[index] if index < len(addresses) else base+len(data)
        stream.write('\n'+function['address']+' '+function['demangled']+'\n')
        for instruction in decoder.disasm(data[start-base:end-base],start):
            comment = ''
            if instruction.mnemonic in ('bl','b') and instruction.operands and instruction.operands[0].type == capstone.arm64.ARM64_OP_IMM:
                target = instruction.operands[0].imm & ((1<<64)-1)
                if target in names: comment = ' ; '+names[target]
            stream.write(f'{instruction.address:016x}: {instruction.mnemonic:8} {instruction.op_str}{comment}\n')
print('AVE2_DISASSEMBLED',len(selection),'command-path functions')
