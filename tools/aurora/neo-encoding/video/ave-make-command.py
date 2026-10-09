#!/usr/bin/env python3
"""Build address-free bring-up commands; never replay macOS runtime pointers.

Config keeps the driver's default bit-17 option at byte 0x4a enabled. It is
independent of the optional MCC DSID field. Restoring this bit alone did not
resolve the AddSharedMemory size/alignment assertion. The startup log mapping
was incorrectly advertised as 0x20200 instead of the page-aligned 0x24000.
DSID remains zero as in MakeFwCmd_Config's no-MCC-object path.
The module replaces common timeout fields immediately before publication.
"""
import argparse
from pathlib import Path
import struct

p = argparse.ArgumentParser()
p.add_argument('command', choices=['config', 'open', 'complete', 'stop', 'close'])
p.add_argument('output', type=Path)
p.add_argument('--client', type=int, default=1)
p.add_argument('--request-id', type=int)
p.add_argument('--codec', choices=['h264', 'hevc'], default='h264')
a = p.parse_args()
assert 1 <= a.client <= 0xffff
packet = bytearray(0x78 if a.command == 'config' else 0x48)
if a.command == 'config':
    struct.pack_into('<I', packet, 0, 1)
    struct.pack_into('<I', packet, 0x20, 0xffffffff)
    packet[0x49] = 1
    packet[0x4a] = 1
elif a.command == 'open':
    # CHM::MakeFwCmd_Open and the verified three-frame AVC capture. The two
    # scalar request/client tokens are local to this experiment; no pointers.
    struct.pack_into('<I', packet, 0, 3)
    struct.pack_into('<QQ', packet, 8, 1, a.client)
    struct.pack_into('<IIIIII', packet, 0x18, 1, 1, 3, 0xc8, 0x36, 0x1e0)
else:
    # Captured flush/stop/close requests; all fields are scalars.
    opcode = {'complete': 9, 'stop': 7, 'close': 4}[a.command]
    token = {'complete': 4, 'stop': 5, 'close': 6}[a.command]
    struct.pack_into('<I', packet, 0, opcode)
    struct.pack_into('<QQ', packet, 8, token, a.client)
    struct.pack_into('<IIIIII', packet, 0x18, 1, 1, opcode,
                     0 if a.command == 'complete' else 0xc8, 0x36, 0x140)
if a.request_id is not None:
    assert 0 <= a.request_id < 2**64
    struct.pack_into('<Q', packet, 8, a.request_id)
if a.codec == 'hevc' and a.command != 'config':
    # Exact scalar differences in the verified 25G76 HEVC capture. Open
    # retains its original final word; flush/stop/close use 0x20.
    struct.pack_into('<I', packet, 0x1c, 2)
    if a.command != 'open':
        struct.pack_into('<I', packet, 0x2c, 0x20)
a.output.write_bytes(packet)
print(f'AVE_COMMAND_CREATED command={a.command} bytes={len(packet)} runtime_addresses=0')
