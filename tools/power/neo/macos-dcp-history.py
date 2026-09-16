#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Read the existing AppleDCPExpert trace ring on J700 / macOS 25F84.

Uses DevMem ABI 7 without replacing the kext or installing hooks. Holds the
registry entry through the same user-client connection for every memory read.
Validates build, class, vtable and live _enterTrace instructions before reading
the 0x2cb8-byte object. The ring is inline at +0x4b8, 256 records of 40 bytes;
+0x4b0 is the next write index. These offsets come from the matching binary's
constructor and _enterTrace disassembly, not another Mac's object layout.

The capture is accepted only when two complete ring copies and their indices
agree. It is a bounded observation of existing history, not a tracing hook.
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

BZERO = 0xfffffe000b384da0
VTABLE = 0xfffffe0007ea6fa8
ENTER_TRACE = 0xfffffe0008d1f498
TRACE_SHA256 = 'e84faae575beb697b447f673fc0f06006b2bdf74cf42d0d109c6df546a4da32d'
CONSTRUCTOR = 0xfffffe0008d1b6f0
CONSTRUCTOR_SHA256 = 'b768b17cbf3377bea7572c7db9257eaa126a780a3b01d714cad6fdbe2c2af443'
EXTENT, INDEX, RING, STRIDE, COUNT = 0x2cb8, 0x4b0, 0x4b8, 40, 256


def command(*args):
    return subprocess.check_output(args, text=True, timeout=15).strip()


class DevMem:
    def __init__(self):
        self.io = C.CDLL('/System/Library/Frameworks/IOKit.framework/IOKit')
        self.system = C.CDLL('/usr/lib/libSystem.B.dylib')
        signatures = {
            'IOServiceMatching': ([C.c_char_p], C.c_void_p),
            'IORegistryEntryIDMatching': ([C.c_uint64], C.c_void_p),
            'IOServiceGetMatchingService': ([C.c_uint, C.c_void_p], C.c_uint),
            'IOObjectGetClass': ([C.c_uint, C.c_void_p], C.c_int),
            'IOObjectRelease': ([C.c_uint], C.c_int),
            'IOServiceOpen': ([C.c_uint, C.c_uint, C.c_uint, C.POINTER(C.c_uint)], C.c_int),
            'IOServiceClose': ([C.c_uint], C.c_int),
            'IOConnectCallStructMethod': ([C.c_uint, C.c_uint, C.c_void_p, C.c_size_t,
                                          C.c_void_p, C.POINTER(C.c_size_t)], C.c_int),
        }
        for name, (args, result) in signatures.items():
            fn = getattr(self.io, name)
            fn.argtypes, fn.restype = args, result
        self.system.mach_absolute_time.restype = C.c_uint64
        service = self.io.IOServiceGetMatchingService(0, self.io.IOServiceMatching(b'DevMemService'))
        if not service:
            raise RuntimeError('DevMem service not found')
        self.connection = C.c_uint()
        task = C.c_uint.in_dll(self.system, 'mach_task_self_').value
        rc = self.io.IOServiceOpen(service, task, 0, C.byref(self.connection))
        self.io.IOObjectRelease(service)
        if rc:
            raise RuntimeError('IOServiceOpen: %#x' % (rc & 0xffffffff))

    def call(self, selector, request, size):
        output = C.create_string_buffer(size)
        actual = C.c_size_t(size)
        rc = self.io.IOConnectCallStructMethod(self.connection, selector, request,
                                               len(request), output, C.byref(actual))
        if rc or actual.value != size:
            raise RuntimeError('selector %d: rc=%#x size=%d' % (selector, rc & 0xffffffff, actual.value))
        return output.raw

    def read(self, address, size):
        data = bytearray()
        for offset in range(0, size, 4096):
            length = min(4096, size - offset)
            data.extend(self.call(1, struct.pack('<IIIIQQ', 7, 1, 0, 0, address + offset, length), length))
        return bytes(data)

    def retain_expert(self, entry_id):
        service = self.io.IOServiceGetMatchingService(0, self.io.IORegistryEntryIDMatching(entry_id))
        if not service:
            raise RuntimeError('registry entry missing')
        name = C.create_string_buffer(128)
        rc = self.io.IOObjectGetClass(service, name)
        self.io.IOObjectRelease(service)
        if rc or name.value != b'AppleDCPExpert':
            raise RuntimeError('entry is not AppleDCPExpert')
        result = self.call(8, struct.pack('<IIQ', 7, 0, entry_id), 24)
        version, flags, retained_id, address = struct.unpack('<IIQQ', result)
        if version != 7 or flags or retained_id != entry_id or address >> 48 != 0xffff or address % 8:
            raise RuntimeError('invalid retained object result')
        return address


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--registry-id', type=lambda s: int(s, 0), required=True)
    ap.add_argument('--output', type=Path, required=True)
    args = ap.parse_args()
    os.umask(0o077)
    if command('sysctl', '-n', 'hw.model') != 'Mac17,5' or command('sw_vers', '-buildVersion') != '25F84':
        ap.error('only the validated J700 / 25F84 layout is supported')
    args.output.mkdir(parents=True, exist_ok=False)
    boot = command('sysctl', '-n', 'kern.bootsessionuuid')
    dev = DevMem()
    try:
        info = dev.call(0, b'', 104)
        abi, page, *fields = struct.unpack('<II12Q', info)
        if abi != 7 or fields[0] != 4096:
            raise RuntimeError('unexpected DevMem ABI')
        slide = fields[8] - BZERO
        if not 0 <= slide < 1 << 32 or slide % 16384:
            raise RuntimeError('unexpected kernel slide')
        if hashlib.sha256(dev.read(ENTER_TRACE + slide, 0xb8)).hexdigest() != TRACE_SHA256:
            raise RuntimeError('live AppleDCPExpert::_enterTrace code mismatch')
        if hashlib.sha256(dev.read(CONSTRUCTOR + slide, 0x24)).hexdigest() != CONSTRUCTOR_SHA256:
            raise RuntimeError('live AppleDCPExpert object-size declaration mismatch')
        address = dev.retain_expert(args.registry_id)
        vptr, = struct.unpack('<Q', dev.read(address, 8))
        (args.output / 'identity.json').write_text(json.dumps(dict(
            boot_uuid=boot, registry_id=hex(args.registry_id), object=hex(address),
            raw_vptr=hex(vptr), expected_vptr=hex(VTABLE + slide), slide=hex(slide)), indent=2) + '\n')
        # PAC also uses bit 47 on this build (observed 0x73ed7e004309efa8
        # for the canonical 0xfffffe004309efa8). Compare low 47 address bits;
        # never dereference or authenticate a pointer inferred from this mask.
        if vptr & ((1 << 47) - 1) != (VTABLE + slide) & ((1 << 47) - 1):
            raise RuntimeError('AppleDCPExpert vtable mismatch: %#x' % vptr)
        first = dev.read(address, EXTENT)
        second = dev.read(address, EXTENT)
        (args.output / 'object-first.bin').write_bytes(first)
        (args.output / 'object-second.bin').write_bytes(second)
        if first[INDEX:] != second[INDEX:]:
            raise RuntimeError('ring changed during capture; retain raw data and retry')
        index, = struct.unpack_from('<I', second, INDEX)
        if index >= COUNT:
            raise RuntimeError('invalid trace index')
        rows = []
        for slot in [*range(index, COUNT), *range(index)]:
            stamp, event, a, b, c = struct.unpack_from('<QI4xQQQ', second, RING + slot * STRIDE)
            if stamp:
                row = dict(slot=slot, ticks=stamp, event=event, args=[hex(a), hex(b), hex(c)])
                if event == 1:
                    row['decoded'] = dict(kind='power_assertion', asserted=bool(a), client=hex(b))
                elif event == 2:
                    row['decoded'] = dict(kind='dcp_power_change', old_state=a,
                                          requested_state=b, result=c)
                # Event 3 belongs to _setPowerStateGated; keep its raw args
                # until its call convention has been separately decoded.
                rows.append(row)
        if any(b['ticks'] < a['ticks'] for a, b in zip(rows, rows[1:])):
            raise RuntimeError('trace timestamps are not monotonic in ring order')
        timebase = (C.c_uint32 * 2)()
        dev.system.mach_timebase_info(C.byref(timebase))
        result = dict(boot_uuid=boot, wall_time=time.time(), mach_absolute_time=dev.system.mach_absolute_time(),
                      timebase=list(timebase), registry_id=hex(args.registry_id), object=hex(address),
                      slide=hex(slide), index=index, count=len(rows), records=rows,
                      tool_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                      object_sha256=hashlib.sha256(second).hexdigest())
        if command('sysctl', '-n', 'kern.bootsessionuuid') != boot:
            raise RuntimeError('boot changed')
        (args.output / 'history.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps({k:v for k,v in result.items() if k != 'records'}))
    finally:
        dev.io.IOServiceClose(dev.connection)


if __name__ == '__main__':
    main()
