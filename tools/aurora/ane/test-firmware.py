#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Synthetic malformed-image tests; no Apple firmware or hardware required."""
import importlib.util
import pathlib
import struct
import unittest

SPEC = importlib.util.spec_from_file_location(
    "ane_firmware", pathlib.Path(__file__).with_name("inspect-firmware.py"))
FW = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FW)


def image():
    segment = struct.pack("<II16sQQQQiiII", 0x19, 72, b"__TEXT", 0, 0x4000,
                          0x4000, 0x100, 5, 5, 0, 0)
    thread = bytearray(288)
    struct.pack_into("<IIII", thread, 0, 5, 288, 6, 68)
    commands = segment + thread
    header = struct.pack("<8I", 0xfeedfacf, 0x100000c, 0, 5, 2, len(commands), 0, 0)
    return bytearray((header + commands).ljust(0x4100, b"\0"))


class FirmwareValidation(unittest.TestCase):
    def reject(self, data, reason):
        with self.assertRaisesRegex(FW.FirmwareError, reason):
            FW.inspect_image(data)

    def test_zero_entry_and_zero_fill(self):
        result = FW.inspect_image(image())
        self.assertEqual(result["entry_point"], 0)
        self.assertEqual(result["segments"][0]["zero_fill_size"], 0x3f00)
        self.assertFalse(result["hardware_compatibility_verified"])
        self.assertFalse(result["firmware_loaded"])

    def test_truncations(self):
        for length in (0, 31, 32, 100, 391, 0x40ff):
            with self.subTest(length=length):
                self.reject(image()[:length], ".")

    def test_wrong_architecture(self):
        data = image()
        struct.pack_into("<I", data, 4, 0x1000007)
        self.reject(data, "not ARM64")

    def test_zero_command_size(self):
        data = image()
        struct.pack_into("<I", data, 36, 0)
        self.reject(data, "command size")

    def test_section_count_overrun(self):
        data = image()
        struct.pack_into("<I", data, 32 + 64, 0xffffffff)
        self.reject(data, "section count")

    def test_huge_memory_address(self):
        data = image()
        struct.pack_into("<Q", data, 32 + 24, 0xffffffffffffffff)
        self.reject(data, "memory range")

    def test_payload_over_header(self):
        data = image()
        struct.pack_into("<Q", data, 32 + 40, 0)
        self.reject(data, "overlaps Mach-O headers")

    def test_file_larger_than_memory(self):
        data = image()
        struct.pack_into("<Q", data, 32 + 32, 1)
        self.reject(data, "exceeds memory size")

    def test_nonexecuting_entry(self):
        data = image()
        struct.pack_into("<Q", data, 32 + 72 + 16 + 32 * 8, 0x200)
        self.reject(data, "file-backed executable")

    def test_write_execute_permissions(self):
        data = image()
        struct.pack_into("<ii", data, 32 + 56, 7, 7)
        self.reject(data, "writable executable")

    def test_unknown_load_command(self):
        data = image()
        struct.pack_into("<I", data, 32, 0x80000028)
        self.reject(data, "unsupported load command")

    def test_overlapping_segments(self):
        data = image()
        second = bytearray(data[32:104])
        second[8:24] = b"__OTHER".ljust(16, b"\0")
        for memory_overlap in (True, False):
            with self.subTest(memory_overlap=memory_overlap):
                struct.pack_into("<Q", second, 24, 0 if memory_overlap else 0x4000)
                commands = data[32:104] + second + data[104:392]
                changed = data.copy()
                changed[32:32 + len(commands)] = commands
                struct.pack_into("<II", changed, 16, 3, len(commands))
                self.reject(changed, "overlapping segment")


if __name__ == "__main__":
    unittest.main()
