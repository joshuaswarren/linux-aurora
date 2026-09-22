#!/usr/bin/env python3
"""Submit bounded native JPEG packets and independently validate a moving sequence."""
import argparse
import base64
import hashlib
import io
import json
from pathlib import Path
import re
import struct
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
PYTHON = ROOT.parent/'neo-update/.venv/bin/python'
DEBUG = '/sys/kernel/debug/apple-jpeg-test'
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('packet',type=Path);p.add_argument('reference',type=Path);p.add_argument('output',type=Path)
p.add_argument('--frames',type=int,default=1);p.add_argument('--prepare-only',action='store_true')
p.add_argument('--codec',choices=['jpeg','mjpeg'],help='JPEG requires one frame; MJPEG requires a sequence')
p.add_argument('--profile',choices=['baseline'],default='baseline')
p.add_argument('--pixel-format',choices=['nv12'],default='nv12')
p.add_argument('--bit-depth',type=int,choices=[8],default=8)
p.add_argument('--alpha',choices=['none'],default='none')
a=p.parse_args();assert 1<=a.frames<=60
assert a.codec is None or (a.codec=='jpeg' and a.frames==1) or (a.codec=='mjpeg' and a.frames>1)
out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
def run(command,name):
    with (out/(name+'.log')).open('w') as log:
        result=subprocess.run([str(x) for x in command],stdout=log,stderr=subprocess.STDOUT)
    if result.returncode:raise RuntimeError(name+' failed; no automatic hardware retry')
    return (out/(name+'.log')).read_text(errors='replace')
def native(command,name,timeout=30):
    return run([PYTHON,ROOT/'console.py','--timeout',timeout,command],name)

packet=a.packet.read_bytes();magic,writes,n,chroma,capacity,reserved=struct.unpack_from('<6I',packet)
assert magic==0x3147504a and 0<writes<=2048 and n==115200 and chroma==76800 and reserved==0
assert 4<=capacity<=1024*1024 and len(packet)==24+writes*8+n
pixels=packet[24+writes*8:]
ref=json.loads((a.reference/'result.json').read_text())
assert ref['passed'] and ref['hardware_required'] and ref['model']=='Mac17,5' and '25G76' in ref['system_version']
assert ref['codec']=='jpeg' and ref['input_pixel_format']=='nv12' and ref['width']==320 and ref['height']==240
source_pixels=(a.reference/'input.raw').read_bytes()[:n]
packet_metadata=json.loads((a.packet.parent/'packet.json').read_text())
assert packet_metadata['packet_sha256']==hashlib.sha256(packet).hexdigest()
assert packet_metadata['capture_sha256']==hashlib.sha256(pixels).hexdigest()
assert packet_metadata['source_sha256']==hashlib.sha256(source_pixels).hexdigest()
assert source_pixels==(a.packet.parent/'source-video-range.nv12').read_bytes()
payload=out/'payload';payload.mkdir();inputs=[]
for frame in range(a.frames):
    shift=frame*8%320
    # Both NV12 planes have 320-byte rows. Even shifts preserve UV pairing.
    source=b''.join(pixels[y*320+shift:(y+1)*320]+pixels[y*320:y*320+shift] for y in range(360))
    (payload/f'packet-{frame:03d}.bin').write_bytes(packet[:24+writes*8]+source)
    inputs.append(b''.join(source_pixels[y*320+shift:(y+1)*320]+source_pixels[y*320:y*320+shift] for y in range(360)))
(out/'input.nv12').write_bytes(b''.join(inputs))
archive=out/'packets.tar.gz'
with tarfile.open(archive,'w:gz') as tar:
    for f in sorted(payload.iterdir()):tar.add(f,arcname=f.name)
if a.prepare_only:
    print('JPEG_PACKETS_PREPARED',a.frames,archive);raise SystemExit(0)
state=native(f'uname -r; cat /proc/device-tree/compatible; echo; cat {DEBUG}/status','initial')
assert '7.1.6-aurora-neo' in state and 'apple,t8140' in state and re.search(r'^failed=0\r?$',state,re.M)
old_frames=int(re.search(r'^frames=(\d+)',state,re.M)[1])
remote='/tmp/jpeg-run-'+hashlib.sha256(str(out).encode()).hexdigest()[:10]
run([PYTHON,ROOT/'push-linux.py',archive,'/tmp/jpeg-packets.tar.gz'],'upload')
native(f'mkdir {remote} && tar -xzf /tmp/jpeg-packets.tar.gz -C {remote}','unpack')
for frame in range(a.frames):
    name=f'{frame:03d}'
    text=native(f'dd if={remote}/packet-{name}.bin of={DEBUG}/submit bs=4194304 count=1 && '
        f'cat {DEBUG}/output > {remote}/frame-{name}.jpg && cat {DEBUG}/status', 'frame-'+name)
    assert re.search(r'^failed=0\r?$',text,re.M) and re.search(r'^frames='+str(old_frames+frame+1)+r'\r?$',text,re.M)
    print('NATIVE_JPEG_FRAME',frame+1,'/',a.frames,flush=True)
text=native(f'printf "JPEG_OUTPUT_BEGIN\\n"; (cd {remote} && tar -czf - frame-*.jpg) | base64; printf "JPEG_OUTPUT_END\\n"','download',60)
encoded=text.replace('\r','').split('\nJPEG_OUTPUT_BEGIN\n',1)[1].split('\nJPEG_OUTPUT_END\n',1)[0]
data=base64.b64decode(''.join(line.strip() for line in encoded.splitlines()),validate=True)
with tarfile.open(fileobj=io.BytesIO(data),mode='r:gz') as tar:
    members=tar.getmembers();assert len(members)==a.frames
    assert {m.name for m in members}=={f'frame-{n:03d}.jpg' for n in range(a.frames)}
    for m in members:
        assert m.isfile() and 4<=m.size<=capacity
        sample=tar.extractfile(m).read();assert sample.startswith(b'\xff\xd8') and sample.endswith(b'\xff\xd9')
        (out/m.name).write_bytes(sample)
run(['ffmpeg','-v','error','-framerate',30,'-i',out/'frame-%03d.jpg','-c:v','copy',out/'video.mov'],'mux')
stream=json.loads(run(['ffprobe','-v','error','-show_streams','-of','json',out/'video.mov'],'probe'))['streams'][0]
assert stream['codec_name']=='mjpeg' and stream['width']==320 and stream['height']==240 and int(stream['nb_frames'])==a.frames
run(['ffmpeg','-v','error','-i',out/'video.mov','-f','rawvideo','-pix_fmt','yuv420p',out/'decoded.yuv'],'decode')
decoded=(out/'decoded.yuv').read_bytes();assert len(decoded)==n*a.frames
assert all(decoded[i*n:(i+1)*n]!=decoded[(i+1)*n:(i+2)*n] for i in range(a.frames-1))
run(['ffmpeg','-v','error','-i',a.reference/'frame-000.sample','-frames:v',1,'-f','rawvideo','-pix_fmt','yuv420p',out/'reference-decoded.yuv'],'reference-decode')
assert decoded[:n]==(out/'reference-decoded.yuv').read_bytes(),'JPEG differs from decoded hardware reference'
stats=out/'psnr.txt'
run(['ffmpeg','-v','error','-i',out/'video.mov','-f','rawvideo','-pixel_format','nv12','-video_size','320x240','-framerate',30,
     '-i',out/'input.nv12','-filter_complex',f'[0:v]format=yuv420p[a];[1:v]format=yuv420p[b];[a][b]psnr=stats_file={stats}','-f','null','-'],'quality')
rows=[dict(w.split(':',1) for w in l.split()) for l in stats.read_text().splitlines()]
assert len(rows)==a.frames;minimum=min(float(row['psnr_avg']) for row in rows);assert minimum>=25
final=native(f'cat {DEBUG}/status; cat /sys/devices/system/cpu/online; /opt/neo/bin/g17p-vulkan-compute; dmesg | tail -15','final',45)
assert re.search(r'^failed=0\r?$',final,re.M) and re.search(r'^0-5\r?$',final,re.M) and 'g17p-vulkan-compute: PASS' in final
result=dict(passed=True,codec='JPEG' if a.frames==1 else 'Motion JPEG',scope='native Neo Linux hardware',
    width=320,height=240,frames=a.frames,fps=30,minimum_psnr_db=minimum,first_frame_matches_macos_reference=True,
    pixel_format=stream['pix_fmt'],profile=stream.get('profile'),packet_sha256=hashlib.sha256(packet).hexdigest(),
    module_sha256=hashlib.sha256((ROOT/'artifacts/linux-jpeg/apple-jpeg-test.ko').read_bytes()).hexdigest())
(out/'validation.json').write_text(json.dumps(result,indent=2)+'\n');print('NATIVE_JPEG_VALIDATED',json.dumps(result),flush=True)
