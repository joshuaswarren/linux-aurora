#!/usr/bin/env python3
"""Exercise auxiliary conversion and DMA boundaries without touching the Neo."""
from array import array
from pathlib import Path
import runpy
import struct
import subprocess
import sys
import tempfile

HERE=Path(__file__).resolve().parent
r=runpy.run_path(str(HERE/'ave-relocate-hevc-precision.py'))
c=runpy.run_path(str(HERE/'depth-companding.py'))
checks=0
for profile in r['PROFILES']:
    for kind in ['start','process']:
        packet,meta=r['relocate'](profile,kind,0x1000e080000,33030144,65535,59)
        assert len(packet)==(0x13f28 if kind=='start' else 0xb1c0)
        assert struct.unpack_from('<Q',packet,0x10)[0]==65535
        for field in meta['relocations']:
            pointer=struct.unpack_from('<Q',packet,int(field['offset'],0))[0]
            assert 0x1000e080000<=pointer<=pointer+field['bytes']<=0x1000e080000+33030144
        checks+=1
    for dma,size,client,frame in [(1,33030144,1,0),(0x1000e080000,4096,1,0),
        (0x1000e080000,33030144,0,0),(0x1000e080000,33030144,1,-1)]:
        try:r['relocate'](profile,'process',dma,size,client,frame)
        except AssertionError:checks+=1
        else:raise AssertionError('Unsafe DMA parameters accepted')
for profile in ['depth','disparity']:
    for value in [float('nan'),float('inf'),-1,65505]:
        try:c['encode_f32'](array('f',[value]).tobytes(),profile)
        except ValueError:checks+=1
        else:raise AssertionError('Unsafe auxiliary input accepted')
    try:c['decode_codes'](array('H',[1024]).tobytes(),profile)
    except ValueError:checks+=1
    else:raise AssertionError('Invalid gray10 value accepted')
with tempfile.TemporaryDirectory() as temporary:
    output=Path(temporary)/'must-not-contact-device'
    for options in [['--codec','h264','--profile','depth'],
        ['--codec','hevc','--profile','depth','--pixel-format','nv12'],
        ['--codec','hevc','--profile','depth','--bit-depth','10'],
        ['--codec','hevc','--profile','main','--pattern','smooth-depth']]:
        result=subprocess.run([sys.executable,str(HERE/'encode-linux-video.py'),str(output),*options],capture_output=True)
        assert result.returncode==2 and not output.exists()
        checks+=1
print('HEVC_AUXILIARY_HOST_CHECKS',checks,'passed; no hardware access')
