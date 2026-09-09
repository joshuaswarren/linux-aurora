#!/usr/bin/env python3
"""Relocate captured precision and auxiliary HEVC into an owned Linux arena."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

ROOT=Path(__file__).resolve().parents[1]
FIELDS=[0x48,*range(0x390,0x430,0x10),*range(0x7d0,0x870,0x10),0xc10,
        *range(0xd50,0xda0,0x10),*range(0xf50,0xfa0,0x10),*range(0x1130,0x1180,0x10),
        0x1350,0x1390,0x13d0,0x1410,0x1420,0x1430,0x1440,0x1490,0x14a0,
        0x14b0,0x14c0,0x2520,0x2530]
PROFILES={
    'main10':dict(start_sha256='153e4a5524bf529707c7401ed0502b8ef978cb71ad493c1a276dca013aa4d551',
        surface_bytes=0x560000,luma_bytes=153600,chroma_bytes=76800,stream=0xdc000,stream_bytes=0x48000,
        metadata=0x258000,tile=0x2a8000,pixel_format='p010le'),
    'main42210':dict(start_sha256='6856c431ca01b82c9df71ee100b292d3cce38502368e2bf37ea4e738220db3f2',
        surface_bytes=0x680000,luma_bytes=153600,chroma_bytes=153600,stream=0x118000,stream_bytes=0x6c000,
        metadata=0x348000,tile=0x398000,pixel_format='p210le'),
    'depth':dict(start_sha256='3925775b9d39f67b587755d91811d5cd545616821ca78e475eb9eef4f806c0a7',
        surface_bytes=0x4d4000,luma_bytes=153664,chroma_bytes=0,stream=0xa0000,stream_bytes=0x48000,
        metadata=0x21c000,tile=0x26c000,pixel_format='grayf32le'),
    'disparity':dict(start_sha256='18c2aaa960163a60a99b310fab11ec5eff896b7afa6683a27caa6e0b7e3ba81e',
        surface_bytes=0x4d4000,luma_bytes=153664,chroma_bytes=0,stream=0xa0000,stream_bytes=0x48000,
        metadata=0x21c000,tile=0x26c000,pixel_format='grayf32le'),
}
INPUT_OFFSET=0xa00200
OLD_BASE=0x100002a8000

def relocate(profile,command,arena_dma,arena_bytes,client,frame_index):
    cfg=PROFILES[profile];auxiliary=profile in ('depth','disparity')
    capture=ROOT/f'artifacts/ave-hevc-{profile}-{"protocol" if auxiliary else "capture"}-20260909'
    protocol=json.loads((capture/'protocol.json').read_text())
    assert protocol['transport_validation_passed'] and protocol['phase_session_closed']
    assert 1<=client<=0xffff and 0<=frame_index<0x100000 and arena_dma%0x4000==0
    assert cfg['surface_bytes']<INPUT_OFFSET and INPUT_OFFSET+cfg['luma_bytes']+cfg['chroma_bytes']<=arena_bytes
    records=[e for e in protocol['pairs'] if e['channel']==1 and e['opcode'] in (6,8)]
    assert len(records)==4 and records[0]['sha256']==cfg['start_sha256']
    samples=[]
    for item in records:
        data=(capture/'ave-command-buffers'/item['buffer']).read_bytes()
        assert len(data)==item['bytes'] and hashlib.sha256(data).hexdigest()==item['sha256']
        samples.append(data)
    assert len(samples[0])==0x13f28
    for i,sample in enumerate(samples[1:]):
        assert len(sample)==0xb1c0
        assert struct.unpack_from('<I',sample,0xacf8)[0]==i
        assert struct.unpack_from('<II',sample,0xa308)==(cfg['luma_bytes'],640)
        if auxiliary:
            assert not any(sample[0xa320:0xa380]), 'Unexpected auxiliary input planes'
        else:
            assert struct.unpack_from('<II',sample,0xa328)==(cfg['chroma_bytes'],640)
        stream=struct.unpack_from('<Q',sample,0xac88)[0]-OLD_BASE
        metadata=struct.unpack_from('<Q',sample,0xac90)[0]-OLD_BASE
        tile_base=struct.unpack_from('<Q',sample,0xd88)[0]-OLD_BASE
        assert 0<=stream and stream+cfg['stream_bytes']<=cfg['surface_bytes']
        assert 0<=metadata and metadata+0xc000<=cfg['surface_bytes']
        assert 0<=tile_base and tile_base+256*1024<=cfg['surface_bytes']
        if i==0:
            assert (stream,metadata,tile_base)==(cfg['stream'],cfg['metadata'],cfg['tile'])
        assert struct.unpack_from('<I',sample,0xac98)[0]==cfg['stream_bytes']
        for tile in range(256):
            assert struct.unpack_from('<Q',sample,0xd88+tile*8)[0]==OLD_BASE+tile_base+tile*1024
    packet=bytearray(samples[0 if command=='start' else 1]);relocs=[]
    def field(offset,size,delta=None):
        old=struct.unpack_from('<Q',packet,offset)[0]
        if delta is None:
            delta=old-OLD_BASE
            assert 0<=delta and delta+size<=cfg['surface_bytes']
        assert 0<=delta and delta+size<=arena_bytes
        struct.pack_into('<Q',packet,offset,arena_dma+delta)
        relocs.append(dict(offset=hex(offset),old=hex(old),new=hex(arena_dma+delta),bytes=size))
    if command=='start':
        for offset in FIELDS:field(offset,struct.unpack_from('<I',packet,offset+8)[0])
    else:
        for tile in range(256):field(0xd88+tile*8,1024)
        field(0xac88,cfg['stream_bytes']);field(0xac90,0xc000)
        field(0xa300,cfg['luma_bytes'],INPUT_OFFSET)
        if cfg['chroma_bytes']:field(0xa320,cfg['chroma_bytes'],INPUT_OFFSET+cfg['luma_bytes'])
        # The reference permits B-frame reordering. The diagnostic transport
        # waits for each Process reply and cannot queue future dependencies.
        # Use the captured force-keyframe selector for every changing input.
        struct.pack_into('<I',packet,0x5d90,1)
        struct.pack_into('<I',packet,0xacf8,frame_index)
        struct.pack_into('<Q',packet,0xad40,frame_index*480000)
    struct.pack_into('<QQ',packet,8,2 if command=='start' else 3+frame_index*2,client)
    packet[0x30:0x40]=bytes(16)
    return bytes(packet),dict(profile=profile,command=command,source_sha256=records[0 if command=='start' else 1]['sha256'],
        relocations=relocs,input_offset=INPUT_OFFSET,force_keyframe=True,**cfg)

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('command',choices=['start','process']);p.add_argument('--profile',choices=PROFILES,required=True)
    p.add_argument('--arena-dma',type=lambda x:int(x,0),required=True);p.add_argument('--arena-bytes',type=lambda x:int(x,0),required=True)
    p.add_argument('--client',type=int,default=1);p.add_argument('--frame-index',type=int,default=0)
    p.add_argument('--output',type=Path,required=True);a=p.parse_args()
    assert not a.output.exists()
    packet,metadata=relocate(a.profile,a.command,a.arena_dma,a.arena_bytes,a.client,a.frame_index)
    a.output.write_bytes(packet);metadata['sha256']=hashlib.sha256(packet).hexdigest()
    a.output.with_suffix('.json').write_text(json.dumps(metadata,indent=2)+'\n')
    print('HEVC_PRECISION_PACKET',a.profile,a.command,len(packet),len(metadata['relocations']))
