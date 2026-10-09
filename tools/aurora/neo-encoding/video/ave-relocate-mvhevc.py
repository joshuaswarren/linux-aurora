#!/usr/bin/env python3
"""Relocate the inspected MV first-pair prefix into the bounded 32 MiB arena."""
import argparse
import hashlib
import json
from pathlib import Path
import runpy
import struct

ROOT=Path(__file__).resolve().parents[1]
CAPTURE=ROOT/'artifacts/mvhevc-prefix-reference-20260909'
OLD_BASE=0x100002a8000
SCRATCH_BYTES=0x1b4c000
INPUT_OFFSET=0x1c00200
FRAME_BYTES=320*240*3//2
FIELDS=runpy.run_path(str(Path(__file__).with_name('ave-relocate-hevc-precision.py')))['FIELDS']+[
    *range(0x5b0,0x650,0x10),*range(0x9f0,0xa90,0x10),*range(0xda0,0xdf0,0x10),
    *range(0xfa0,0xff0,0x10),*range(0x1240,0x1290,0x10)]

def captured():
    manifest=json.loads((CAPTURE/'manifest.json').read_text())
    assert not manifest['complete_capture'] and not manifest['native_linux_validated']
    paths=['inspection.json','protocol.json']
    protocol=json.loads((CAPTURE/'protocol.json').read_text())
    records=[r for r in protocol['pairs'] if r['channel']==1 and r['opcode'] in (6,8,9)]
    assert [r['opcode'] for r in records]==[6,8,8,9]
    assert records[0]['sha256']=='936dd083dd949cb0d956ae0623ca989ad9c5d61a9100b74b80fd57ff2fd949a0'
    paths += ['ave-command-buffers/'+r['buffer'] for r in records]
    for name in paths:
        data=(CAPTURE/name).read_bytes();expected=manifest['files'][name]
        assert len(data)==expected['bytes'] and hashlib.sha256(data).hexdigest()==expected['sha256']
    return [(CAPTURE/'ave-command-buffers'/r['buffer']).read_bytes() for r in records],records

def relocate(command,arena_dma,arena_bytes,client,frame_index=0):
    assert command in ('start','pair') and 1<=client<=0xffff and 0<=frame_index<0x80000
    assert arena_dma%0x4000==0 and arena_bytes>=INPUT_OFFSET+2*FRAME_BYTES
    assert arena_dma>=0x10000000000 and arena_dma+arena_bytes<1<<48
    assert SCRATCH_BYTES<INPUT_OFFSET
    packets,records=captured();relocations=[]
    def pointer(packet,offset,size,delta=None):
        original=struct.unpack_from('<Q',packet,offset)[0]
        if delta is None:
            delta=original-OLD_BASE
            assert 0<=delta and delta+size<=SCRATCH_BYTES
        assert 0<=delta and delta+size<=arena_bytes
        struct.pack_into('<Q',packet,offset,arena_dma+delta)
        relocations.append(dict(field=hex(offset),old=hex(original),new=hex(arena_dma+delta),bytes=size))
    def header(packet,request):
        struct.pack_into('<QQ',packet,8,request,client);packet[0x30:0x40]=bytes(16)
    if command=='start':
        packet=bytearray(packets[0]);assert len(packet)==0x13f28
        assert len(set(FIELDS))==len(FIELDS)==85
        for offset in FIELDS:pointer(packet,offset,struct.unpack_from('<I',packet,offset+8)[0])
        for offset in [0x2580,0x3750,0xdcc0]:assert packet[offset:offset+8]==packets[0][offset:offset+8]
        header(packet,2);result=bytes(packet)
    else:
        views=[];completes=[]
        for view in range(2):
            packet=bytearray(packets[1+view]);assert len(packet)==0xb1c0
            assert struct.unpack_from('<I',packet,0x20)[0]==0x15+view
            assert all(struct.unpack_from('<I',packet,o)[0]==view for o in [0xac84,0xacf8,0xad34])
            assert not any(packet[0xa340:0xa380])
            for tile in range(256):pointer(packet,0xd88+tile*8,1024)
            pointer(packet,0xac88,struct.unpack_from('<I',packet,0xac98)[0]);pointer(packet,0xac90,0xc000)
            for plane,offset in enumerate([0xa300,0xa320]):
                size=[76800,38400][plane]
                assert struct.unpack_from('<II',packet,offset+8)==(size,320)
                pointer(packet,offset,size,INPUT_OFFSET+view*FRAME_BYTES+(76800 if plane else 0))
            header(packet,3+frame_index*4+view)
            struct.pack_into('<I',packet,0x5d90,1)
            struct.pack_into('<I',packet,0xacf8,frame_index*2+view)
            struct.pack_into('<Q',packet,0xad40,frame_index*480000)
            views.append(bytes(packet))
            complete=bytearray(packets[3]);assert len(complete)==72 and struct.unpack_from('<I',complete)[0]==9
            header(complete,5+frame_index*4+view);completes.append(bytes(complete))
        result=struct.pack('<5I',0x32505641,0xb1c0,0xb1c0,72,72)+b''.join(views+completes)
        assert len(result)==91172
    return result,dict(command=command,source_status='inspected incomplete reference prefix',native_validated=False,
        source_sha256=[r['sha256'] for r in records],client=client,frame_index=frame_index,
        scratch_bytes=SCRATCH_BYTES,input_offset=INPUT_OFFSET,input_bytes_per_eye=FRAME_BYTES,
        relocations=relocations,sha256=hashlib.sha256(result).hexdigest())

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('command',choices=['start','pair'])
    p.add_argument('--arena-dma',type=lambda x:int(x,0),required=True)
    p.add_argument('--arena-bytes',type=lambda x:int(x,0),required=True)
    p.add_argument('--client',type=int,default=1);p.add_argument('--frame-index',type=int,default=0)
    p.add_argument('--output',type=Path,required=True);a=p.parse_args();assert not a.output.exists()
    packet,meta=relocate(a.command,a.arena_dma,a.arena_bytes,a.client,a.frame_index)
    a.output.write_bytes(packet);a.output.with_suffix('.json').write_text(json.dumps(meta,indent=2)+'\n')
    print('MVHEVC_PACKET_PREPARED',a.command,len(packet),len(meta['relocations']),'relocations; hardware untested')
