#!/usr/bin/env python3
"""Keep a reference HEVC sample description while muxing bounded keyframe samples."""
import argparse
from pathlib import Path
import struct

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('reference',type=Path);p.add_argument('output',type=Path)
p.add_argument('samples',nargs='+',type=Path);a=p.parse_args()
assert not a.output.exists() and 1<=len(a.samples)<=60
data=a.reference.read_bytes();assert len(data)<=64*1024*1024
def atoms(blob):
    offset=0;result=[]
    while offset<len(blob):
        if len(blob)-offset<8:
            assert not any(blob[offset:]);break
        size,kind=struct.unpack_from('>I4s',blob,offset)
        assert 8<=size<=len(blob)-offset, 'Require bounded ordinary MOV atoms'
        result.append((kind,blob[offset+8:offset+size]));offset+=size
    return result
def box(kind,payload):return struct.pack('>I4s',len(payload)+8,kind)+payload
top=atoms(data);assert sum(k==b'moov' for k,v in top)==1 and sum(k==b'ftyp' for k,v in top)==1
moov=next(v for k,v in top if k==b'moov');assert sum(k==b'trak' for k,v in atoms(moov))==1
samples=[f.read_bytes() for f in a.samples]
for sample in samples:
    assert 6<=len(sample)<=4*1024*1024
    offset=0;slices=[]
    while offset<len(sample):
        size=struct.unpack_from('>I',sample,offset)[0];assert 2<=size<=len(sample)-offset-4
        nal=sample[offset+4:offset+4+size];assert not nal[0]&128 and nal[1]&7
        typ=nal[0]>>1&63
        if typ<32:slices.append(typ)
        offset+=4+size
    assert slices and all(16<=typ<=23 for typ in slices), 'Only independently decodable HEVC keyframes are supported'
count=len(samples);chunk_offset=0
containers={b'moov',b'trak',b'mdia',b'minf',b'stbl'}
def rewrite(blob):
    result=[]
    for kind,payload in atoms(blob):
        if kind in containers:payload=rewrite(payload)
        elif kind in (b'mvhd',b'mdhd'):
            assert payload[0]==0
            scale=struct.unpack_from('>I',payload,12)[0];assert scale%30==0
            mutable=bytearray(payload);struct.pack_into('>I',mutable,16,count*scale//30);payload=bytes(mutable)
        elif kind==b'tkhd':
            assert payload[0]==0
            mvhd=next(v for k,v in atoms(moov) if k==b'mvhd')
            scale=struct.unpack_from('>I',mvhd,12)[0];assert scale%30==0
            mutable=bytearray(payload);struct.pack_into('>I',mutable,20,count*scale//30);payload=bytes(mutable)
        elif kind==b'stts':
            # Timescale comes from the unique track's mdhd.
            trak=next(v for k,v in atoms(moov) if k==b'trak')
            mdia=next(v for k,v in atoms(trak) if k==b'mdia')
            mdhd=next(v for k,v in atoms(mdia) if k==b'mdhd')
            assert mdhd[0]==0
            scale=struct.unpack_from('>I',mdhd,12)[0];assert scale%30==0
            payload=struct.pack('>4I',0,1,count,scale//30)
        elif kind==b'stsc':payload=struct.pack('>5I',0,1,1,count,1)
        elif kind==b'stsz':payload=struct.pack('>3I',0,0,count)+struct.pack('>'+str(count)+'I',*[len(s) for s in samples])
        elif kind in (b'stco',b'co64'):
            kind=b'stco';payload=struct.pack('>3I',0,1,chunk_offset)
        elif kind==b'stss':payload=struct.pack('>2I',0,count)+struct.pack('>'+str(count)+'I',*range(1,count+1))
        elif kind in (b'ctts',b'edts',b'sdtp',b'sgpd',b'sbgp'):
            continue  # No reordering/edit timeline or old per-sample groups.
        result.append(box(kind,payload))
    return b''.join(result)
ftyp=box(b'ftyp',next(v for k,v in top if k==b'ftyp'))
rewritten=box(b'moov',rewrite(moov));chunk_offset=len(ftyp)+len(rewritten)+8
final=box(b'moov',rewrite(moov));assert len(final)==len(rewritten)
a.output.write_bytes(ftyp+final+box(b'mdat',b''.join(samples)))
print('REFERENCE_HEVC_MOV_REPACKED',count,a.output)
