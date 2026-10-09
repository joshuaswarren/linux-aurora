#!/usr/bin/env python3
"""Extract private firmware analysis inputs under the ignored build directory."""
import hashlib
import os
import sys
from pathlib import Path
import struct

root = Path(__file__).resolve().parent.parent
source = root / 'AuroraSilicon/build/j813-centauri/Firmware/t2026macG1/Release/ftab.bin'
target = root / 'AuroraSilicon/build/j813-centauri/re/firmware'
data = source.read_bytes()
assert hashlib.sha256(data).hexdigest() == '97b6ad3fd20772f1193319957e5a10a4fd469e4921c2061abaacd8c7d90ce1f4'
os.umask(0o077)
target.mkdir(parents=True, exist_ok=True)
for index in range(struct.unpack_from('<I', data, 0x28)[0]):
    tag, offset, size, flags = struct.unpack_from('<4sIII', data, 0x30 + index * 16)
    if tag not in (b'aciw', b'rkos', b'wftp', b'w2lp', b'w5lp', b'_wfa', b'_osl'):
        continue
    assert not flags and offset + size <= len(data)
    payload = data[offset:offset + size]
    if tag == b'wftp':
        # All 958 TX-core assertions match the direct 0x100000 mapping.
        for address in (0x10c3c2, 0x10ca34, 0x10ca92, 0x114442, 0x114472):
            assert payload[address - 0x100000:address - 0x100000 + 2] == b'\x00\xde'
    path = target / (tag.decode() + ('.plist' if tag == b'_wfa' else '.bin'))
    path.write_bytes(payload)
    path.chmod(0o600)
    print(path.name, size, hashlib.sha256(payload).hexdigest())
    if tag == b'aciw':
        # The second UMAC code bank is not contiguous with the first bank.
        # Five independent supplicant assertions match this relocation delta.
        delta = 0x3dd000
        for address in (0x529016, 0x52901e, 0x52e618, 0x52e9ce, 0x52eafc):
            assert payload[address - delta:address - delta + 2] == b'\x00\xde'
        bank = payload[0x500000 - delta:]
        bank_path = target / 'aciw-bank-500000.bin'
        bank_path.write_bytes(bank)
        bank_path.chmod(0o600)
        print(bank_path.name, len(bank), hashlib.sha256(bank).hexdigest())
        # This code range has a different mapping. All 138 logged asserts
        # between 0x4fc000 and 0x508000 match delta 0x3db000.
        prefix_delta = 0x3db000
        for address in (0x4fc01c, 0x4fc150, 0x4fc1a8, 0x4fc1ce, 0x4fc2b8):
            assert payload[address - prefix_delta:address - prefix_delta + 2] == b'\x00\xde'
        prefix = payload[0x4fc000 - prefix_delta:0x508000 - prefix_delta]
        prefix_path = target / 'aciw-code-4fc000.bin'
        prefix_path.write_bytes(prefix)
        prefix_path.chmod(0o600)
        print(prefix_path.name, len(prefix), hashlib.sha256(prefix).hexdigest())
        for low, high, relocation, anchors in (
            (0x570000, 0x574000, 0x3e4000, (0x570ef2, 0x570efa, 0x570f02, 0x570fa8, 0x5722a0)),
            (0x574000, 0x578000, 0x3e4000, (0x5748b0, 0x574aae, 0x575a42, 0x57622c, 0x57626e)),
            (0x58e000, 0x594000, 0x3ec000, (0x58e1ca, 0x58e416, 0x58e70c, 0x58e902)),
            (0x59b000, 0x5a0000, 0x3ed000, (0x59c064, 0x59c116, 0x59e0c0, 0x59e56a, 0x59eb48)),
        ):
            for address in anchors:
                assert payload[address - relocation:address - relocation + 2] == b'\x00\xde'
            # All catalog anchors in these ranges matched the stated mapping:
            # 6 system, 6 connection, 57 security helper, 21 debug assertions.
            region = payload[low - relocation:high - relocation]
            region_path = target / f'aciw-code-{low:x}.bin'
            region_path.write_bytes(region)
            region_path.chmod(0o600)
            print(region_path.name, len(region), hashlib.sha256(region).hexdigest())

# Import the text section separately when Ghidra's Mach-O loader rejects the
# extracted shared-cache image. Preserve its real virtual address in the name.
binary_name = sys.argv[1] if len(sys.argv) > 1 else 'WLANACIAdapter'
assert binary_name in ('WLANACIAdapter', 'CentauriAlpha', 'CentauriControl', 'AppleWLANPrime')
adapter = (target.parent / ('prime' if binary_name == 'AppleWLANPrime' else 'dk') / binary_name).read_bytes()
assert struct.unpack_from('<I', adapter)[0] == 0xfeedfacf
position = 32
for _ in range(struct.unpack_from('<I', adapter, 16)[0]):
    command, command_size = struct.unpack_from('<II', adapter, position)
    if command == 0x19:
        for section in range(struct.unpack_from('<I', adapter, position + 64)[0]):
            entry = position + 72 + section * 80
            name = adapter[entry:entry + 16].rstrip(b'\0')
            if name == b'__text':
                address, size, offset = struct.unpack_from('<QQI', adapter, entry + 32)
                assert offset + size <= len(adapter)
                payload = adapter[offset:offset + size]
                path = target / f'{binary_name}-text-{address:x}.bin'
                path.write_bytes(payload)
                path.chmod(0o600)
                print(path.name, size, hashlib.sha256(payload).hexdigest())
    position += command_size
