"""Host checks for bounded reads through an existing native user pmap."""
import importlib.util
from pathlib import Path
import struct
from types import SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location(
    'user_memory', Path(__file__).with_name('read-user-memory.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class UserMemoryTests(unittest.TestCase):
    def setUp(self):
        self.dram = 0x10000000000
        self.root = self.dram + 0x4000
        self.level2 = self.dram + 0x8000
        self.level3 = self.dram + 0xc000
        self.payload = self.dram + 0x10000
        self.va = 0x100000000
        self.metadata = {}
        self.memory = {}
        self.reads = []
        pmap, attr = 0xfffffe0000001000, 0xfffffe0000002000
        self.metadata[pmap + 8] = struct.pack('<4Q', self.root, 0, 1 << 47, attr)
        geometry = bytearray(0x78)
        struct.pack_into('<I', geometry, 0x40, 1)
        struct.pack_into('<I', geometry, 0x48, 3)
        struct.pack_into('<QQ', geometry, 0x58, 0x4000, 14)
        self.metadata[attr] = geometry
        self.put(self.root, self.level2 | 3)
        self.put(self.level2 + ((self.va >> 25) & 0x7ff) * 8, self.level3 | 3)
        self.put(self.level3, self.payload | 3)
        self.memory[self.payload] = b'native'
        hv = SimpleNamespace(
            ctx=SimpleNamespace(spsr=SimpleNamespace(M=5)),
            j813_kernelcache_verified=True,
            u=SimpleNamespace(adt={'/chosen': SimpleNamespace(chip_id=0x8142,
                                                            dram_base=self.dram)},
                              ba=SimpleNamespace(mem_size_actual=0x100000)),
            readmem=lambda a, n: self.metadata[a][:n],
            iface=SimpleNamespace(readmem=self.read))
        self.hv, self.pmap, self.attr = hv, pmap, attr
        self.reader = module.M5UserMemory(hv, pmap)

    def put(self, address, value):
        self.memory[address] = struct.pack('<Q', value)

    def read(self, address, size):
        self.reads.append((address, size))
        return self.memory.get(address, bytes(size))[:size]

    def test_resident_page(self):
        self.assertEqual(self.reader.read(self.va, 6), b'native')

    def test_absent_page_does_not_read_payload(self):
        self.put(self.level3, 0)
        with self.assertRaisesRegex(RuntimeError, 'not resident'):
            self.reader.read(self.va, 6)
        self.assertNotIn((self.payload, 6), self.reads)

    def test_table_pointer_cannot_escape_to_mmio(self):
        self.put(self.root, 0xb82000003)
        with self.assertRaisesRegex(RuntimeError, 'outside DRAM'):
            self.reader.read(self.va, 6)
        self.assertTrue(all(self.dram <= a < self.dram + 0x100000 for a, _ in self.reads))

    def test_payload_pointer_cannot_escape_to_mmio(self):
        self.put(self.level3, 0xb82000003)
        with self.assertRaisesRegex(RuntimeError, 'outside DRAM'):
            self.reader.read(self.va, 6)
        self.assertNotIn((0xb82000000, 6), self.reads)

    def test_cross_page_request_validates_both_before_fetching(self):
        with self.assertRaisesRegex(RuntimeError, 'not resident'):
            self.reader.read(self.va + 0x3ffc, 8)
        self.assertNotIn((self.payload + 0x3ffc, 4), self.reads)

    def test_wrong_page_geometry_rejected(self):
        struct.pack_into('<Q', self.metadata[self.attr], 0x58, 0x1000)
        with self.assertRaisesRegex(RuntimeError, 'geometry'):
            module.M5UserMemory(self.hv, self.pmap)

    def test_unverified_kernel_rejected(self):
        self.hv.j813_kernelcache_verified = False
        with self.assertRaisesRegex(RuntimeError, 'exact M5'):
            module.M5UserMemory(self.hv, self.pmap)

    def test_invalid_user_range_rejected_before_io(self):
        for va, size in ((1 << 47, 1), (self.va, 0), (self.va, 0x10001), (-1, 1)):
            with self.assertRaisesRegex(RuntimeError, 'Invalid bounded'):
                self.reader.read(va, size)
        self.assertEqual(self.reads, [])


if __name__ == '__main__':
    unittest.main()
