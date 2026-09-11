#!/usr/bin/env python3
"""Exercise context capture bounds and coherence without opening a device."""
import struct
import unittest

from m5_context_capture import PAGE,survey_context


class ContextSurveyTest(unittest.TestCase):
    def setUp(self):
        self.region=0x100000
        self.context=3
        self.root_slot=self.region+48
        self.tables={a:bytearray(PAGE) for a in (0x200000,0x204000,0x208000,0x20c000)}
        self.root=0x200001
        # Two virtual windows alias the same resource, with distinct leaves.
        struct.pack_into('<Q',self.tables[0x200000],1*8,0x204003)
        struct.pack_into('<Q',self.tables[0x200000],16*8,0x204003)
        struct.pack_into('<Q',self.tables[0x204000],0,0x208003)
        struct.pack_into('<Q',self.tables[0x208000],0,0xc0000000400c8b)
        self.read_counts={}

    def read(self,address,size):
        self.read_counts[address]=self.read_counts.get(address,0)+1
        if address==self.root_slot and size==8:return struct.pack('<Q',self.root)
        if address in self.tables and size==PAGE:return bytes(self.tables[address])
        raise ValueError('Attempted non-table resource read')

    def survey(self,**kwargs):
        return survey_context(self.read,self.region,3,[(0x1000000000,PAGE),(0x10000000000,PAGE)],**kwargs)[0]

    def test_aliases_preserve_two_leaves_but_count_one_physical_page(self):
        result=self.survey(max_pages=1)
        self.assertEqual(result['unique_resource_pages'],1)
        self.assertEqual([m['address'] for m in result['mappings']],[0x1000000000,0x10000000000])
        self.assertEqual(len(result['tables']),3)
        self.assertEqual(set(self.read_counts.values()),{2})

    def test_missing_leaf_is_not_read_as_a_resource(self):
        self.tables[0x208000][:8]=bytes(8)
        self.assertEqual(self.survey()['mappings'],[])

    def test_block_descriptor_is_rejected(self):
        struct.pack_into('<Q',self.tables[0x204000],0,0x208001)
        with self.assertRaisesRegex(ValueError,'block'):self.survey()

    def test_firmware_root_window_is_rejected(self):
        with self.assertRaisesRegex(ValueError,'user root'):
            survey_context(self.read,self.region,3,[(1<<42,PAGE)])
        self.assertFalse(self.read_counts)

    def test_root_replacement_is_rejected(self):
        def read(address,size):
            data=self.read(address,size)
            if address==self.root_slot and self.read_counts[address]>1:return struct.pack('<Q',0x20c001)
            return data
        with self.assertRaisesRegex(ValueError,'root changed'):
            survey_context(read,self.region,3,[(0x1000000000,PAGE)])

    def test_table_replacement_is_rejected(self):
        def read(address,size):
            data=self.read(address,size)
            if address==0x208000 and self.read_counts[address]>1:return bytes(PAGE)
            return data
        with self.assertRaisesRegex(ValueError,'table changed'):
            survey_context(read,self.region,3,[(0x1000000000,PAGE)])

    def test_resource_limit_and_table_limit_reject(self):
        struct.pack_into('<Q',self.tables[0x208000],8,0xc0000000404c8b)
        with self.assertRaisesRegex(ValueError,'page limit'):
            survey_context(self.read,self.region,3,[(0x1000000000,2*PAGE)],max_pages=1)
        with self.assertRaisesRegex(ValueError,'table limit'):self.survey(max_tables=2)


if __name__=='__main__':unittest.main()
