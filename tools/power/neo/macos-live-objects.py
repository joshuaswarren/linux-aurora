#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Read J700/25F84 PMGR mappings or existing ASC mailbox history.

No hooks, MMIO accesses, state changes, or pointer authentication guesses.
Registry anchors are retained by DevMem for the lifetime of the connection.
Offsets come from the matching native constructors and accessors. Build,
class, vtable, code, sizes and double-copy stability are checked. RAM captures
are private because mailbox histories may contain kernel addresses.
"""
import argparse
import ctypes as C
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import struct
import time

spec = importlib.util.spec_from_file_location('history', Path(__file__).with_name('macos-dcp-history.py'))
history = importlib.util.module_from_spec(spec)
spec.loader.exec_module(history)


def retain(dev, entry_id, expected):
    obj = dev.io.IOServiceGetMatchingService(0, dev.io.IORegistryEntryIDMatching(entry_id))
    if not obj:
        # ASC providers are attached but not registered for service matching.
        dev.io.IORegistryCreateIterator.argtypes = [C.c_uint, C.c_char_p, C.c_uint, C.POINTER(C.c_uint)]
        dev.io.IORegistryCreateIterator.restype = C.c_int
        dev.io.IOIteratorNext.argtypes = [C.c_uint]
        dev.io.IOIteratorNext.restype = C.c_uint
        dev.io.IORegistryEntryGetRegistryEntryID.argtypes = [C.c_uint, C.POINTER(C.c_uint64)]
        dev.io.IORegistryEntryGetRegistryEntryID.restype = C.c_int
        iterator = C.c_uint()
        if dev.io.IORegistryCreateIterator(0, b'IOService', 1, C.byref(iterator)):
            raise RuntimeError('registry iteration failed')
        try:
            while True:
                candidate = dev.io.IOIteratorNext(iterator)
                if not candidate:
                    break
                actual = C.c_uint64()
                rc = dev.io.IORegistryEntryGetRegistryEntryID(candidate, C.byref(actual))
                if not rc and actual.value == entry_id:
                    obj = candidate
                    break
                dev.io.IOObjectRelease(candidate)
        finally:
            dev.io.IOObjectRelease(iterator)
        if not obj:
            raise RuntimeError('registry entry missing')
    name = C.create_string_buffer(128)
    rc = dev.io.IOObjectGetClass(obj, name)
    dev.io.IOObjectRelease(obj)
    if rc or name.value.decode() != expected:
        raise RuntimeError('registry class mismatch: ' + repr(name.value))
    abi, flags, actual_id, addr = struct.unpack('<IIQQ', dev.call(8, struct.pack('<IIQ', 7, 0, entry_id), 24))
    if abi != 7 or flags or actual_id != entry_id:
        raise RuntimeError('retain result mismatch')
    pointer(addr)
    return addr


def pointer(addr):
    if addr >> 48 != 0xffff or addr % 8:
        raise RuntimeError('noncanonical or unaligned RAM pointer: %#x' % addr)
    return addr


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--kind', choices=['pmgr', 'mailbox'], required=True)
    ap.add_argument('--registry-id', type=lambda s: int(s, 0), required=True)
    ap.add_argument('--output', type=Path, required=True)
    a = ap.parse_args()
    os.umask(0o077)
    if history.command('sysctl', '-n', 'hw.model') != 'Mac17,5' or history.command('sw_vers', '-buildVersion') != '25F84':
        ap.error('requires reviewed J700 / 25F84')
    a.output.mkdir(parents=True, exist_ok=False)
    dev = history.DevMem()
    result = dict(kind=a.kind, registry_id=hex(a.registry_id), wall_time=time.time(),
                  boot_uuid=history.command('sysctl', '-n', 'kern.bootsessionuuid'),
                  tool_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest())
    def save():
        (a.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    try:
        abi, page, *fields = struct.unpack('<II12Q', dev.call(0, b'', 104))
        if abi != 7 or page != 16384 or fields[0] != 4096:
            raise RuntimeError('DevMem ABI mismatch')
        slide = fields[8] - history.BZERO
        if not 0 <= slide < 1 << 32 or slide % 16384:
            raise RuntimeError('invalid slide')
        guards = json.loads(Path(__file__).with_name('macos-live-code-guards.json').read_text())
        wanted = {'getRegMap', 'initRegMap', 'pmgrCtor', 'deviceOffset', 'deviceMap', 'deviceGroup', 'deviceIndex'} if a.kind == 'pmgr' else {'getStructuredLog', 'initLog', 'logMsg'}
        if {g['name'] for g in guards if g['name'] in wanted} != wanted:
            raise RuntimeError('missing code guards')
        for g in guards:
            if g['name'] in wanted and hashlib.sha256(dev.read(int(g['address'], 0) + slide, g['size'])).hexdigest() != g['sha256']:
                raise RuntimeError('live code mismatch: ' + g['name'])
        cls, vtable = ('AppleT8140PMGR', 0xfffffe0008149e48) if a.kind == 'pmgr' else ('AppleASCWrapV6', 0xfffffe0007e00648)
        addr = retain(dev, a.registry_id, cls)
        vptr, = struct.unpack('<Q', dev.read(addr, 8))
        if (vptr & ((1 << 47) - 1)) != ((vtable + slide) & ((1 << 47) - 1)):
            raise RuntimeError('vtable mismatch')
        result.update(object=hex(addr), slide=hex(slide), raw_vptr=hex(vptr))
        save()
        if a.kind == 'pmgr':
            # getRegMap: this+0x29d0+regmap*0x60+die*0x30, 137 IDs.
            dies, = struct.unpack('<I', dev.read(addr + 0x6304, 4))
            if dies not in (1, 2):
                raise RuntimeError('unexpected die count')
            first = dev.read(addr + 0x29d0, 137 * 0x60)
            second = dev.read(addr + 0x29d0, 137 * 0x60)
            (a.output / 'mappings-first.bin').write_bytes(first)
            (a.output / 'mappings-second.bin').write_bytes(second)
            if first != second:
                raise RuntimeError('mapping table changed')
            rows = []
            for map_id in range(137):
                for die in range(dies):
                    off = map_id * 0x60 + die * 0x30
                    adt_reg, = struct.unpack_from('<I', second, off + 8)
                    obj, va, pa, size = struct.unpack_from('<QQQI', second, off + 0x10)
                    if obj or va or pa or size:
                        rows.append(dict(regmap_id=map_id, die=die, adt_reg_index=adt_reg,
                                         map_object=hex(obj), virtual=hex(va), physical=hex(pa), size=size))
            result.update(dies=dies, mappings=rows)
            group_ptr, group_count = struct.unpack('<QI', dev.read(addr + 0x60a8, 12))
            mode, = struct.unpack('<I', dev.read(addr + 0x1c08, 4))
            if not 1 <= group_count <= 64 or mode != 2:
                raise RuntimeError('unexpected native PS group mode/count')
            groups = dev.read(pointer(group_ptr), group_count * 12)
            if dev.read(group_ptr, len(groups)) != groups:
                raise RuntimeError('PS groups changed')
            (a.output / 'ps-groups.bin').write_bytes(groups)
            device_ptr, = struct.unpack('<Q', dev.read(addr + 0x9090, 8))
            pointer(device_ptr)
            table = json.loads(Path(__file__).with_name('j700-pmgr-domains.json').read_text())
            wanted_devices = {'DISP_CPU', 'DISPEXT0_CPU', 'DISP_FE', 'DISP_SYS', 'SIO_CPU'}
            devices = []
            for d in table:
                if d['name'] not in wanted_devices:
                    continue
                device_id = d['device_id']
                if not 0 < device_id < 800:
                    raise RuntimeError('invalid device ID')
                index, = struct.unpack('<H', dev.read(addr + 0x9d28 + device_id * 2, 2))
                if index >= 296:
                    raise RuntimeError('invalid native device index')
                record = dev.read(device_ptr + index * 48, 48)
                if dev.read(device_ptr + index * 48, 48) != record:
                    raise RuntimeError('device record changed')
                (a.output / (d['name'] + '.bin')).write_bytes(record)
                group = record[0x13]
                offset = struct.unpack_from('<I', record, 0x10)[0] & 0xffffff
                if group >= group_count or offset % 8:
                    raise RuntimeError('invalid native group/offset')
                regmap, base_offset, mask = struct.unpack_from('<III', groups, group * 12)
                mapping = next(x for x in rows if x['regmap_id'] == regmap and x['die'] == 0)
                native_pa = int(mapping['physical'], 0) + base_offset + offset
                devices.append(dict(name=d['name'], device_id=device_id, index=index,
                                    record=record.hex(), group=group, regmap=regmap,
                                    group_offset=hex(base_offset), device_offset=hex(offset),
                                    physical=hex(native_pa), archived_physical=hex(int(d['base'], 0) + d['offset'])))
            if len(devices) != len(wanted_devices):
                raise RuntimeError('missing reviewed devices')
            result.update(ps_group_mode=mode, ps_group_count=group_count, devices=devices)
        else:
            # AppleIOP::getStructuredLog returns this+0x90.
            head = dev.read(addr + 0x90, 0x20)
            buf, words, reserved, pos, payload_words, role = struct.unpack_from('<Q5I', head)
            pointer(buf)
            stride = payload_words + 3
            if payload_words != 2 or words != 128 * stride or pos >= words or pos % stride:
                raise RuntimeError('unexpected mailbox ring layout')
            first = dev.read(buf, words * 8)
            second = dev.read(buf, words * 8)
            tail = dev.read(addr + 0x90, 0x20)
            (a.output / 'header.bin').write_bytes(head)
            (a.output / 'ring-first.bin').write_bytes(first)
            (a.output / 'ring-second.bin').write_bytes(second)
            if first != second or head != tail:
                raise RuntimeError('mailbox changed during capture; retain and retry')
            rows = []
            for slot in [*range(pos // stride, 128), *range(pos // stride)]:
                ticks, continuous, event, msg0, msg1 = struct.unpack_from('<5Q', second, slot * stride * 8)
                if ticks:
                    row = dict(slot=slot, ticks=ticks, continuous_ticks=continuous, event=event,
                               kind={0:'receive', 1:'receive_interrupt_or_timeout', 2:'send', 3:'transmit_interrupt'}.get(event, 'unknown'),
                               msg0=hex(msg0), msg1=hex(msg1))
                    if event in (0, 2):
                        row.update(endpoint=msg1 & 0xff, management_type=(msg0 >> 52) & 0xfff)
                    rows.append(row)
            if any(b['ticks'] < a['ticks'] for a, b in zip(rows, rows[1:])):
                raise RuntimeError('mailbox timestamps out of order')
            result.update(buffer=hex(buf), words=words, position=pos, role=hex(role), records=rows)
        if history.command('sysctl', '-n', 'kern.bootsessionuuid') != result['boot_uuid']:
            raise RuntimeError('boot changed')
        result['status'] = 'captured'
        save()
        print(json.dumps({k:v for k,v in result.items() if k not in ('records', 'mappings')}))
    except BaseException as e:
        result.update(status='error', error=repr(e))
        save()
        raise
    finally:
        dev.io.IOServiceClose(dev.connection)


if __name__ == '__main__':
    main()
