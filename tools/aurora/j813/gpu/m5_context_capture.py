# SPDX-License-Identifier: MIT
"""Bounded, read-only enumeration of selected native M5 user GPU mappings.

The caller supplies the same DRAM-only reader used by M5GPUCapture. A survey
reads page tables, not mapped resources. It never walks the firmware root or
accepts block mappings, and checks roots/tables again before returning.
"""
import hashlib
import struct

PAGE = 0x4000
PA_MASK = ((1 << 48)-1) & ~(PAGE-1)


def survey_context(read_dram, gpu_region, context, windows, max_pages=256, max_tables=32):
    if not 0<context<256 or not 1<=max_pages<=512 or not 1<=max_tables<=64:
        raise ValueError('Invalid bounded context survey')
    if not 1<=len(windows)<=4:
        raise ValueError('Expected one to four context windows')
    intervals=[]
    for start,size in windows:
        if (start%PAGE or size%PAGE or not 0<size<=0x2000000
                or not 0<=start<start+size<=1<<42):
            raise ValueError('Context windows must remain in the user root')
        if any(start<end and begin<start+size for begin,end in intervals):
            raise ValueError('Overlapping context windows')
        intervals.append((start,start+size))
    root_slot=gpu_region+context*16
    root_bytes=read_dram(root_slot,8)
    if len(root_bytes)!=8:
        raise ValueError('Short context root read')
    root,=struct.unpack('<Q',root_bytes)
    if not root&1:
        raise ValueError('Context root is absent')
    tables={}

    def table(address):
        if address not in tables:
            if len(tables)>=max_tables:
                raise ValueError('Context table limit exceeded')
            data=read_dram(address,PAGE)
            if len(data)!=PAGE:
                raise ValueError('Short context table read')
            tables[address]=data
        return tables[address]

    def child(parent,index):
        value,=struct.unpack_from('<Q',table(parent),index*8)
        if not value&1:
            return None
        if value&3!=3:
            raise ValueError('Unexpected GPU block mapping')
        return value&PA_MASK

    root_page=root&PA_MASK
    mappings=[]
    unique=set()
    for start,end in intervals:
        for address in range(start,end,PAGE):
            l2=child(root_page,(address>>36)&63)
            if l2 is None:continue
            l3=child(l2,(address>>25)&2047)
            if l3 is None:continue
            leaf_slot=l3+((address>>14)&2047)*8
            leaf,=struct.unpack_from('<Q',table(l3),leaf_slot-l3)
            if not leaf&1:continue
            if leaf&3!=3:
                raise ValueError('Unexpected GPU leaf descriptor')
            pa=leaf&PA_MASK
            unique.add(pa)
            if len(unique)>max_pages:
                raise ValueError('Context resource page limit exceeded')
            mappings.append(dict(address=address,physical=pa,leaf_slot=leaf_slot,leaf=leaf))
    if read_dram(root_slot,8)!=root_bytes:
        raise ValueError('Context root changed during survey')
    for address,initial in tables.items():
        if read_dram(address,PAGE)!=initial:
            raise ValueError('Context table changed during survey')
    result=dict(context=context,root_slot=root_slot,root_entry=root,windows=windows,
        unique_resource_pages=len(unique),mappings=mappings,
        tables=[dict(physical=pa,sha256=hashlib.sha256(data).hexdigest()) for pa,data in tables.items()],
        table_bytes_read=16+2*PAGE*len(tables),stable=True)
    return result,tables
