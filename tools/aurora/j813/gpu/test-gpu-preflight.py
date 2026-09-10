#!/usr/bin/env python3
"""Reject GPU carveouts that could expose unrelated RAM during Linux boot."""
import copy
from pathlib import Path
import runpy
import unittest

api = runpy.run_path(str(Path(__file__).with_name('gpu-boot-preflight.py')))
validate = api['validate_regions']


class ReservationTests(unittest.TestCase):
    def setUp(self):
        self.dram = (0x10000000000, 0x400000000)
        self.regions = {name: (self.dram[0] + 0x100000 + i * 0x100000, 0x80000)
                        for i, name in enumerate(api['REGIONS'])}

    def test_disjoint_and_adjacent_regions(self):
        validate(self.regions, self.dram)
        self.regions['gfx1-data'] = (self.regions['gfx-data'][0] + 0x80000, 0x4000)
        validate(self.regions, self.dram)

    def test_unsafe_regions(self):
        for invalid in [(0, 0x4000), (self.dram[0], 0), (self.dram[0] + 1, 0x4000),
                        (self.dram[0], 0x4001), (self.dram[0] - 0x4000, 0x4000),
                        (sum(self.dram) - 0x4000, 0x8000), ((1 << 64) - 0x4000, 0x8000),
                        self.regions['gfx-data']]:
            with self.subTest(invalid=invalid):
                candidate = copy.deepcopy(self.regions)
                candidate['gfx1-data'] = invalid
                with self.assertRaises(ValueError):
                    validate(candidate, self.dram)

    def test_page_table_capacity_and_missing_region(self):
        self.regions['gfx-shared-region'] = (self.regions['gfx-shared-region'][0], 0x40000)
        with self.assertRaises(ValueError):
            validate(self.regions, self.dram)
        del self.regions['gpu-region']
        with self.assertRaises(KeyError):
            validate(self.regions, self.dram)


if __name__ == '__main__':
    unittest.main()
