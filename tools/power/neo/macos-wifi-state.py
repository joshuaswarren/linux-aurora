#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Sample reviewed AppleSunriseWLAN RAM fields without stopping the driver.

Pinned to the J700 25F84 arm64e binary. Uses task_for_pid and read-only Mach
copies, not debugger attachment, task suspension, MMIO, or kernel hooks.
The firmware-own field is a software observation, not hardware residency.
"""
import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import time

BINARY = Path('/System/Library/DriverExtensions/com.apple.AppleSunriseWLAN.dext/com.apple.AppleSunriseWLAN')
SHA = '990d66919bdb39d3a2ec30a0cd441c1a5c2f171d337baf02369d03061cbb653d'
BASE = 0x100000000
WDEV = 0x1004a1700


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--pid', type=int, required=True)
    p.add_argument('--seconds', type=int, default=10)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    if not 1 <= a.seconds <= 300:
        p.error('seconds must be 1..300')
    def command(*args):
        return subprocess.check_output(args, text=True, timeout=15).strip()
    if command('sysctl', '-n', 'hw.model') != 'Mac17,5' or command('sw_vers', '-buildVersion') != '25F84':
        p.error('requires reviewed J700 / 25F84')
    binary = BINARY.read_bytes()
    if binary[:4] == bytes.fromhex('cafebabe'):
        slices = []
        for i in range(struct.unpack_from('>I', binary, 4)[0]):
            cpu, subtype, off, size, align = struct.unpack_from('>5I', binary, 8 + 20*i)
            if cpu == 0x100000c and subtype & 0xffffff == 2:
                slices.append(binary[off:off+size])
        if len(slices) != 1:
            p.error('expected one arm64e slice')
        binary = slices[0]
    if hashlib.sha256(binary).hexdigest() != SHA:
        p.error('native binary identity differs')
    segments = []
    pos = 32
    for _ in range(struct.unpack_from('<I', binary, 16)[0]):
        cmd, size = struct.unpack_from('<II', binary, pos)
        if cmd == 0x19:
            va, vsize, off, fsize = struct.unpack_from('<4Q', binary, pos+24)
            segments.append((va, off, fsize))
        pos += size
    lib = C.CDLL('/usr/lib/libSystem.B.dylib')
    lib.task_for_pid.argtypes = [C.c_uint, C.c_int, C.POINTER(C.c_uint)]
    lib.task_info.argtypes = [C.c_uint, C.c_uint, C.c_void_p, C.POINTER(C.c_uint)]
    lib.mach_vm_read_overwrite.argtypes = [C.c_uint, C.c_uint64, C.c_uint64, C.c_void_p, C.POINTER(C.c_uint64)]
    lib.mach_port_deallocate.argtypes = [C.c_uint, C.c_uint]
    me = C.c_uint.in_dll(lib, 'mach_task_self_').value
    task = C.c_uint()
    rc = lib.task_for_pid(me, a.pid, C.byref(task))
    if rc:
        raise RuntimeError('task_for_pid: %d' % rc)
    def read(address, size):
        if not 0x100000000 <= address < 1 << 47 or not 1 <= size <= 65536:
            raise RuntimeError('invalid user RAM range')
        out = C.create_string_buffer(size)
        actual = C.c_uint64()
        rc = lib.mach_vm_read_overwrite(task.value, address, size, out, C.byref(actual))
        if rc or actual.value != size:
            raise RuntimeError('RAM read failed: %d, %d' % (rc, actual.value))
        return out.raw
    os.umask(0o077)
    a.output.mkdir(parents=True, exist_ok=False)
    try:
        boot = command('sysctl', '-n', 'kern.bootsessionuuid')
        info = C.create_string_buffer(24)
        count = C.c_uint(5)
        rc = lib.task_info(task.value, 17, info, C.byref(count))
        if rc or count.value != 5:
            raise RuntimeError('TASK_DYLD_INFO failed')
        dyld, extent, fmt = struct.unpack_from('<QQI', info.raw)
        if fmt != 1:
            raise RuntimeError('requires 64-bit dyld info')
        version, nimages, images = struct.unpack('<IIQ', read(dyld, 16))
        if not 1 <= nimages <= 1024:
            raise RuntimeError('invalid image count')
        # dyld's first image is the executable; verify path and Mach header.
        main, name, modified = struct.unpack('<QQQ', read(images, 24))
        path = read(name, len(bytes(BINARY))+1).split(b'\0', 1)[0]
        if path != bytes(BINARY) or read(main, 32) != binary[:32]:
            raise RuntimeError('process executable mismatch')
        slide = main - BASE
        for addr, length in [(0x1000d48d4, 0x4ec), (0x1000d3a7c, 0x100),
                             (0x10007eb7c, 0x20), (0x1000f1d24, 0x24),
                             (0x10033f448, 0x120)]:
            va, off, size = next(s for s in segments if s[0] <= addr and addr+length <= s[0]+s[2])
            if read(addr+slide, length) != binary[off+addr-va:off+addr-va+length]:
                raise RuntimeError('live ownership code differs')
        def get_adapter():
            # wlanGetWiphy -> AppleSunriseWLAN::getAdapter. g_prAdapter
            # exists in this binary but is unused (NULL) in the live driver.
            wdev, = struct.unpack('<Q', read(WDEV+slide, 8))
            wiphy, = struct.unpack('<Q', read(wdev, 8))
            adapter, = struct.unpack('<Q', read(wiphy+0x842a8, 8))
            return adapter
        adapter = get_adapter()
        if adapter % 8:
            raise RuntimeError('unaligned adapter')
        metadata = dict(boot_uuid=boot, pid=a.pid, binary_sha256=SHA, slide=hex(slide),
                        adapter=hex(adapter), tool_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                        seconds=a.seconds, interval_s=0.2, wall_time=time.time(),
                        power_profiles_raw=read(adapter+0x32248, 16).hex(),
                        power_profile_votes_raw=read(adapter+0x32258, 16).hex())
        (a.output/'metadata.json').write_text(json.dumps(metadata, indent=2)+'\n')
        samples = []
        start = time.monotonic()
        with (a.output/'samples.jsonl').open('x') as output:
            while time.monotonic()-start < a.seconds:
                if get_adapter() != adapter:
                    raise RuntimeError('adapter changed')
                block = read(adapter+0x123a4, 32)
                row = dict(elapsed_s=time.monotonic()-start, raw=block.hex(),
                           interrupts_enabled=block[4], firmware_own=block[6],
                           own_enable_word=struct.unpack_from('<I', block, 0x18)[0],
                           ipc_own_selected=read(adapter+0x197941, 1)[0])
                if row['firmware_own'] not in (0, 1):
                    raise RuntimeError('unexpected ownership flag')
                samples.append(row)
                output.write(json.dumps(row)+'\n'); output.flush()
                time.sleep(0.2)
        if boot != command('sysctl', '-n', 'kern.bootsessionuuid'):
            raise RuntimeError('boot changed')
        result = dict(samples=len(samples), firmware_own_samples=sum(s['firmware_own'] for s in samples),
                      elapsed_s=time.monotonic()-start, status='captured')
        (a.output/'result.json').write_text(json.dumps(result, indent=2)+'\n')
        print(json.dumps(result))
    finally:
        lib.mach_port_deallocate(me, task.value)


if __name__ == '__main__':
    main()
