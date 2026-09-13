#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Bounded H17 firmware bootstrap experiment; called with ANE domains active."""
import hashlib
import importlib.util
import pathlib
import struct
import time

PAGE = 0x4000
IOVA = 0x10000000000
CLUSTER = 0x400000000
DART = 0x401800000
FW_SHA256 = "319726efbccbe1c7b46a814c5a0e0bffdd0188e238df94fa0977fbbf6019a2f8"
IPC_OFFSET = 0x380000
IPC_SIZE = 0x10000
COMMAND_OFFSET = 0x390000
COMMAND_SIZE = 0x40000
HEAP_OFFSET = 0x3d0000
HEAP_SIZE = 0xf00000
ARENA_SIZE = HEAP_OFFSET + HEAP_SIZE
BOOT_ARGS_OFFSET = COMMAND_OFFSET + 0x100


def prepare_image(path):
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != FW_SHA256:
        raise ValueError("not the J700-selected H17 firmware from 25G76")
    spec = importlib.util.spec_from_file_location(
        "ane_inspector", pathlib.Path(__file__).with_name("inspect-firmware.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    layout = module.inspect_image(data)
    if layout["entry_point"] != 0 or layout["memory_end"] > 0x380000:
        raise ValueError("unexpected H17 firmware layout")
    result = bytearray(0x380000)
    for segment in layout["segments"]:
        start, size = segment["address"], segment["file_size"]
        offset = segment["file_offset"]
        result[start:start + size] = data[offset:offset + size]
    return bytes(result)


def upload_image(p, u, image, handshake=False):
    if handshake:
        arena = bytearray(ARENA_SIZE)
        arena[:len(image)] = image
        values = (IOVA, IOVA + IPC_OFFSET, 0x380000, 0xfc80000,
                  IOVA + HEAP_OFFSET, HEAP_SIZE, 0, 0, 0, 0,
                  IPC_SIZE, IOVA + COMMAND_OFFSET, COMMAND_SIZE)
        struct.pack_into("<13Q", arena, BOOT_ARGS_OFFSET, *values)
        struct.pack_into("<I", arena, BOOT_ARGS_OFFSET + 0x68, 64)
        image = bytes(arena)
    buffers = []
    for size in (len(image), PAGE, PAGE, PAGE):
        buffers.append(u.memalign(PAGE, size))
    firmware, root, middle, leaf = buffers
    tables = [bytearray(PAGE) for unused in range(3)]
    struct.pack_into("<Q", tables[0], ((IOVA >> 36) & 0x7ff) * 8, (middle >> 4) | 1)
    struct.pack_into("<Q", tables[1], 0, (leaf >> 4) | 1)
    for index in range(len(image) // PAGE):
        entry = ((firmware + index * PAGE) >> 4) | (0xfff << 40) | 1
        struct.pack_into("<Q", tables[2], index * 8, entry)
    for address, data in [(firmware, image), *zip(buffers[1:], tables)]:
        print(f"FW_UPLOAD {address:#x} size={len(data):#x}", flush=True)
        u.compressed_writemem(address, data)
        p.dc_cvac(address, len(data))
    return buffers


def run_bootstrap(p, u, buffers, quiesce_children, handshake=False, command=False):
    def read(address):
        value = p.read32(address)
        if p.get_exc_count():
            raise RuntimeError(f"guarded read fault at {address:#x}")
        return value

    def write(address, value):
        print(f"FW_WRITE {address:#x}={value:#x}", flush=True)
        p.write32(address, value)
        if p.get_exc_count():
            raise RuntimeError(f"guarded write fault at {address:#x}")

    def poll(address, mask, expected, seconds=0.5):
        deadline = time.monotonic() + seconds
        while True:
            value = read(address)
            if value & mask == expected:
                return value
            if time.monotonic() >= deadline:
                raise RuntimeError(f"poll timeout {address:#x}: {value:#x}")

    def invalidate():
        write(DART + 0x80, 0x100)  # Flush stream 0 only.
        poll(DART + 0x80, 1 << 31, 0)

    original = {offset: read(DART + offset) for offset in
                (0x200, 0xc00, 0x1000, 0x1400, 0x100)}
    if original[0x200] & 1 or original[0xc00] or original[0x1400] or original[0x100]:
        raise RuntimeError("DART locked, owned, or faulted; refusing to replace it")
    if original[0x1000] != 1 or read(CLUSTER + 0x1600044):
        raise RuntimeError("unexpected DART mode or running firmware CPU")
    vector = p.read64(CLUSTER + 0x1050000)
    if p.get_exc_count() or vector != IOVA | 1:
        raise RuntimeError(f"unexpected inherited firmware reset vector {vector:#x}")
    mapped = False
    running = False
    stopped = False
    try:
        firmware, root, middle, leaf = buffers
        # Empty private stream: install complete tables before enabling DMA.
        mapped = True
        write(DART + 0x1000, 9)  # Three translation tables for the 42-bit IOVA.
        write(DART + 0x1400, (root >> 12) | 1)
        if read(DART + 0x1000) != 9 or read(DART + 0x1400) != (root >> 12) | 1:
            raise RuntimeError("DART configuration did not read back")
        invalidate()
        write(DART + 0xc00, 1)
        for offset in range(0x1880020, 0x1880040, 4):
            write(CLUSTER + offset, 0)
        write(CLUSTER + 0x1880038, 1)  # Native H17 cold-boot scratch6.
        write(CLUSTER + 0x1600044, 0)
        running = True
        write(CLUSTER + 0x1600044, 0x10)
        deadline = time.monotonic() + 3
        previous = None
        while time.monotonic() < deadline:
            scratch = tuple(read(CLUSTER + offset) for offset in range(0x1880020, 0x1880040, 4))
            state = (read(CLUSTER + 0x1600048), read(DART + 0x100), scratch)
            if state != previous:
                print(f"FW_STATE cpu={state[0]:#x} dart_error={state[1]:#x} "
                      f"scratch={[hex(x) for x in scratch]}", flush=True)
                previous = state
            if state[1] & 0x80000000:
                print(f"FW_DART_FAULT lo={read(DART + 0x170):#x} "
                      f"hi={read(DART + 0x174):#x}", flush=True)
                break
            if scratch[7] == 0x8042006:
                # Earlier fields may have been read before the ready marker.
                scratch = tuple(read(CLUSTER + offset)
                                for offset in range(0x1880020, 0x1880040, 4))
                print("FW_READY_PAYLOAD " + str([hex(x) for x in scratch]), flush=True)
                print("ANE_FIRMWARE_WAKE_OBSERVED", flush=True)
                if handshake:
                    if scratch[1] + 1 > IPC_SIZE or scratch[3] > HEAP_SIZE:
                        raise RuntimeError("firmware requested more memory than the mapped arena")
                    boot_args = firmware + BOOT_ARGS_OFFSET
                    u.iface.writemem(boot_args + 0x50, struct.pack("<Q", scratch[1] + 1))
                    p.dc_cvac(boot_args, 0x174)
                    pointer = IOVA + BOOT_ARGS_OFFSET
                    write(CLUSTER + 0x1880020, pointer & 0xffffffff)
                    write(CLUSTER + 0x1880024, pointer >> 32)
                    write(CLUSTER + 0x188003c, 0xf7fbdff9)
                    deadline = time.monotonic() + 3
                    second = None
                    while time.monotonic() < deadline:
                        second = tuple(read(CLUSTER + offset)
                                       for offset in range(0x1880020, 0x1880040, 4))
                        error = read(DART + 0x100)
                        if error & 0x80000000:
                            print(f"FW_SECOND_DART_FAULT error={error:#x} "
                                  f"lo={read(DART + 0x170):#x} hi={read(DART + 0x174):#x}",
                                  flush=True)
                            break
                        if second[7] == 0x8042006:
                            second = tuple(read(CLUSTER + offset)
                                           for offset in range(0x1880020, 0x1880040, 4))
                            print("ANE_FIRMWARE_CHANNEL_TABLE_READY " +
                                  str([hex(x) for x in second]), flush=True)
                            p.dc_ivac(firmware + IPC_OFFSET, 0x2000)
                            table = u.iface.readmem(firmware + IPC_OFFSET, 0x2000)
                            channels = {}
                            for index in range(min(scratch[0], 32)):
                                descriptor = table[index * 0x100:(index + 1) * 0x100]
                                name = descriptor[:64].split(b"\0", 1)[0].decode("ascii")
                                direction, source, count, address = struct.unpack_from(
                                    "<IIQQ", descriptor, 0x40)
                                print(f"FW_CHANNEL {index} {name} type={direction} "
                                      f"source={source} slots={count} iova={address:#x}", flush=True)
                                offset = address - IOVA
                                length = count * 0x40
                                if not (IPC_OFFSET <= offset and 0 < count <= 512 and
                                        offset + length <= IPC_OFFSET + IPC_SIZE):
                                    raise RuntimeError("ring outside validated IPC allocation")
                                channels[name] = (direction, source, count, firmware + offset)
                                if direction == 0:
                                    host = firmware + offset
                                    p.dc_ivac(host, length)
                                    ring = bytearray(u.iface.readmem(host, length))
                                    for slot in range(count):
                                        struct.pack_into("<3Q", ring, slot * 0x40, 1, 0, 0)
                                    u.iface.writemem(host, ring)
                                    p.dc_cvac(host, length)
                                    print(f"FW_CHANNEL_HOST_INITIALIZED {name}", flush=True)
                            write(CLUSTER + 0x188002c, 0x8042006)
                            poll(CLUSTER + 0x188002c, 0xffffffff, 0, seconds=3)
                            print("ANE_FIRMWARE_HOST_HANDSHAKE_COMPLETE", flush=True)
                            if command:
                                run_command(p, u, firmware, channels, read, write)
                            return {"wake": True, "handshake": True,
                                    "first_scratch": scratch, "second_scratch": second}
                    print("FW_SECOND_STATE " + str(second), flush=True)
                    return {"wake": True, "handshake": False, "second_scratch": second}
                return {"wake": True, "scratch": scratch}
        return {"wake": False, "last_state": previous}
    finally:
        # Revoke execution before destroying any mappings or freeing memory.
        if running:
            write(CLUSTER + 0x1600044, 0)
            poll(CLUSTER + 0x1600044, 0x10, 0)
            # RUN clear is not a synchronous halt after the IPC scheduler starts.
            # Keep DMA mappings intact until both child domains are power-gated.
            quiesce_children()
        stopped = True
        if mapped:
            write(DART + 0xc20, 1)
            poll(DART + 0xc00, 1, 0)
            write(DART + 0x1400, original[0x1400])
            write(DART + 0x1000, original[0x1000])
            invalidate()
        if stopped:
            for address in reversed(buffers):
                u.free(address)
        print("FW_CPU_STOPPED_AND_MAPPING_REMOVED", flush=True)


def run_command(p, u, firmware, channels, read, write):
    """Issue the native 12-byte startup request (opcode 4) on the IO channel."""
    tx = channels["IO"]
    rx = channels["IO_T2H"]
    if tx[:3] != (0, 1, 16) or rx[:3] != (1, 6, 64):
        raise RuntimeError("unexpected H17 command channel layout")
    command_address = firmware + COMMAND_OFFSET
    command_iova = IOVA + COMMAND_OFFSET
    request = struct.pack("<IHHI", 0, 4, 0, 0)
    u.iface.writemem(command_address, request)
    p.dc_cvac(command_address, len(request))
    p.dc_ivac(tx[3], 64)
    slot = bytearray(u.iface.readmem(tx[3], 64))
    if struct.unpack_from("<Q", slot)[0] != 1:
        raise RuntimeError("IO command ring is not empty")
    # Payload and ownership are published together after the request is clean.
    struct.pack_into("<3Q", slot, 0, command_iova, len(request), len(request))
    u.iface.writemem(tx[3], slot)
    p.dc_cvac(tx[3], 64)
    write(CLUSTER + 0x1884000, 1 << tx[1])
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        error = read(DART + 0x100)
        if error:
            raise RuntimeError(f"command DART error {error:#x}")
        p.dc_ivac(rx[3], 64)
        response_slot = bytearray(u.iface.readmem(rx[3], 64))
        pointer, length, extra = struct.unpack_from("<3Q", response_slot)
        if pointer & 1:
            continue
        print(f"FW_COMMAND_RESPONSE pointer={pointer:#x} length={length:#x} "
              f"extra={extra:#x}", flush=True)
        if pointer & ~3 != command_iova or length > COMMAND_SIZE:
            raise RuntimeError("unexpected command response descriptor")
        p.dc_ivac(command_address, max(len(request), length))
        response = u.iface.readmem(command_address, max(len(request), length))
        print("FW_COMMAND_BYTES " + response[:256].hex(), flush=True)
        struct.pack_into("<Q", response_slot, 0, pointer | 1)
        u.iface.writemem(rx[3], response_slot)
        p.dc_cvac(rx[3], 64)
        opcode, status = struct.unpack_from("<HH", response, 4)
        if status or opcode != 4:
            raise RuntimeError(f"startup command failed: status={status:#x} opcode={opcode:#x}")
        print("ANE_FIRMWARE_STARTUP_COMMAND_PASS", flush=True)
        return
    for name in ("IO", "IO_T2H", "BUF_T2H", "SHAREDMALLOC", "TERMINAL"):
        p.dc_ivac(channels[name][3], 64)
        print(f"FW_CHANNEL_TIMEOUT {name} " + u.iface.readmem(channels[name][3], 64).hex(), flush=True)
    raise RuntimeError("firmware startup command response timed out")
