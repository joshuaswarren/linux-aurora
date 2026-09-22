#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Execute the real sampler instructions against guarded synthetic MMIO."""
import struct
import unittest

from unicorn import Uc, UC_ARCH_ARM64, UC_MODE_ARM, UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE
from unicorn.arm64_const import UC_ARM64_REG_X0, UC_ARM64_REG_X1, UC_ARM64_REG_X2, UC_ARM64_REG_LR
from m1n1.asm import ARMAsm
from m5_native_monitor import source, decode, SGX, SIZE

CODE, OUTPUT, END = 0x100000, 0x200000, 0x300000


class MonitorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.code = ARMAsm(source(), CODE).data

    def run_helper(self, *, powered=True, queue=11, slot=0, pipe_mask=1,
                   queue_changes=False, slot_changes=False, state_changes=False, power_changes=False):
        uc = Uc(UC_ARCH_ARM64, UC_MODE_ARM)
        uc.mem_map(CODE, 0x10000)
        uc.mem_map(OUTPUT, 0x1000)
        uc.mem_map(SGX, 0x1000000)
        uc.mem_write(CODE, self.code)
        uc.mem_write(OUTPUT, bytes(SIZE))
        uc.reg_write(UC_ARM64_REG_X0, SGX)
        uc.reg_write(UC_ARM64_REG_X1, OUTPUT)
        uc.reg_write(UC_ARM64_REG_X2, 11)
        uc.reg_write(UC_ARM64_REG_LR, END)
        observed, tags, writes, counts = [], [], [], {}
        state = (pipe_mask << 16) | 0xd
        fixed = {0x10398: slot, 0x103a8: queue, 0xc078: (11 << 40) | 1,
                 0x21008: 0x31000070005f0001, 0x21020: 1, 0x21028: 0x8000000000}

        def read(uc, access, address, size, value, user):
            offset = address - SGX
            observed.append(offset)
            counts[offset] = counts.get(offset, 0) + 1
            data = fixed.get(offset, 0)
            if offset in (0xe80000, 0xe80110):
                data = 0xff if powered and not (power_changes and counts[offset] > 1) else 0
            elif offset == 0x103a8 and queue_changes and counts[offset] > 1:
                data = 12
            elif offset == 0x10398 and slot_changes and counts[offset] > 1:
                data = 1
            elif offset == 0x4098:
                if tags[-1] == (slot << 44) | (1 << 43) | 0xc020:
                    data = 0 if state_changes and counts[offset] > 1 else state
                elif tags[-1] & ((1 << 44) - 1) == 0x1688:
                    data = 1
                elif tags[-1] & ((1 << 44) - 1) == 0xa730:
                    data = 0
                elif tags[-1] & ((1 << 44) - 1) == (1 << 43) | (3 << 40) | 0xa0a8:
                    data = 0x55
                else:
                    raise AssertionError('Unexpected AP read tag')
            uc.mem_write(address, int(data).to_bytes(size, 'little'))

        def write(uc, access, address, size, value, user):
            writes.append((address - SGX, size, value))
            self.assertEqual((address - SGX, size), (0x4090, 8))
            tags.append(value)

        uc.hook_add(UC_HOOK_MEM_READ, read, begin=SGX, end=SGX + 0xffffff)
        uc.hook_add(UC_HOOK_MEM_WRITE, write, begin=SGX, end=SGX + 0xffffff)
        uc.emu_start(CODE, END, count=1000)
        result = decode(bytes(uc.mem_read(OUTPUT, SIZE)))
        self.assertEqual(uc.reg_read(UC_ARM64_REG_X0), int(result['values']['flags'], 16))
        return result, observed, tags

    def test_power_off_never_touches_low_sgx(self):
        result, reads, tags = self.run_helper(powered=False)
        self.assertFalse(result['powered'])
        self.assertEqual(reads, [0xe80000])
        self.assertEqual(tags, [])

    def test_matching_compute_and_queue(self):
        result, _, tags = self.run_helper()
        self.assertTrue(result['compute_monitor_valid'])
        self.assertTrue(result['queue_view_valid'])
        self.assertEqual(tags, [(1 << 43) | 0xc020, 0x1688, 0xa730,
            (1 << 43) | (3 << 40) | 0xa0a8, (1 << 43) | 0xc020])
        self.assertEqual(result['values']['queue_completed'], '0x8000000000')
        self.assertEqual(result['values']['pipe_1688'], '0x1')
        self.assertEqual(result['values']['pipe_a0a8'], '0x55')

    def test_other_selected_queue_is_not_read(self):
        result, reads, _ = self.run_helper(queue=12)
        self.assertFalse(result['queue_view_valid'])
        self.assertNotIn(0x21008, reads)
        self.assertNotIn(0x21028, reads)

    def test_selected_slot_and_single_pipe_are_indexed(self):
        for slot in (0, 6, 7):
            for pipe in range(3):
                result, _, tags = self.run_helper(slot=slot, pipe_mask=1 << pipe)
                self.assertTrue(result['compute_monitor_valid'])
                self.assertEqual(tags, [(slot << 44) | (1 << 43) | 0xc020,
                    (pipe << 44) | 0x1688, (pipe << 44) | 0xa730,
                    (pipe << 44) | (1 << 43) | (3 << 40) | 0xa0a8,
                    (slot << 44) | (1 << 43) | 0xc020])
                self.assertEqual(int(result['values']['pipe_index'], 16), pipe)

    def test_invalid_slot_or_ambiguous_pipe_skips_compute_monitors(self):
        for kwargs in ({'slot': 8}, {'pipe_mask': 0}, {'pipe_mask': 3}, {'pipe_mask': 7}):
            result, _, tags = self.run_helper(**kwargs)
            self.assertFalse(result['compute_monitor_valid'])
            self.assertNotIn(0x1688, tags)
            self.assertNotIn(0xa730, tags)

    def test_changed_selection_or_state_invalidates_monitor(self):
        for kwargs in ({'queue_changes': True}, {'slot_changes': True},
                       {'state_changes': True}, {'power_changes': True}):
            result, _, _ = self.run_helper(**kwargs)
            self.assertFalse(result['compute_monitor_valid'])


if __name__ == '__main__':
    unittest.main()
