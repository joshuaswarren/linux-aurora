#!/usr/bin/env python3
"""Run bounded 10-bit HEVC sessions through the existing native AVE diagnostic."""
import argparse
from array import array
import hashlib
import json
import math
from pathlib import Path
import re
import runpy
import shutil
import struct
import subprocess
import sys
import tarfile

ROOT=Path(__file__).resolve().parents[1]
PYTHON=ROOT.parent/'neo-update/.venv/bin/python'
DEBUG='/sys/kernel/debug/285100000.video-codec'
relocator=runpy.run_path(str(Path(__file__).with_name('ave-relocate-hevc-precision.py')))
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('output',type=Path);p.add_argument('--profile',choices=relocator['PROFILES'],required=True)
p.add_argument('--frames',type=int,default=1);p.add_argument('--client',type=int)
p.add_argument('--pixel-format',choices=['p010le','p210le','grayf32le'],help='Require the profile-matched input layout')
p.add_argument('--bit-depth',type=int,choices=[10,32],help='Input precision; auxiliary float32 is compacted to L010')
p.add_argument('--module-binary',type=Path,default=ROOT/'artifacts/linux-ave/apple-ave.ko')
p.add_argument('--alpha',choices=['none'],default='none')
p.add_argument('--pattern',choices=['row-wrap','smooth-depth'],default='row-wrap')
p.add_argument('--validate-only',action='store_true')
p.add_argument('--prepare-only',action='store_true');p.add_argument('--arena-dma',type=lambda x:int(x,0))
p.add_argument('--arena-bytes',type=lambda x:int(x,0))
a=p.parse_args();assert 1<=a.frames<=60 and not(a.validate_only and a.prepare_only)
out=a.output.resolve()
if a.validate_only:assert out.is_dir()
else:out.mkdir(parents=True,exist_ok=False)
cfg=relocator['PROFILES'][a.profile]
auxiliary=a.profile in ('depth','disparity')
assert a.pattern=='row-wrap' or auxiliary, 'Smooth depth pattern requires auxiliary float input'
assert a.pixel_format is None or a.pixel_format==cfg['pixel_format'], 'Pixel format does not match HEVC profile'
assert a.bit_depth is None or a.bit_depth==(32 if auxiliary else 10), 'Input precision does not match layout'
reference=(ROOT/f'artifacts/coverage-reference-20260909/guest/hevc-{a.profile}' if auxiliary
           else ROOT/f'artifacts/ave-hevc-{a.profile}-capture-20260909/reference')
ref=json.loads((reference/'result.json').read_text())
assert ref['passed'] and ref['hardware_required'] and ref['using_hardware_encoder']
assert ref['model']=='Mac17,5' and '25G76' in ref['system_version']
assert ref['codec']==({'depth':'deph','disparity':'dish'}[a.profile] if auxiliary else 'hvc1')
assert ref['input_pixel_format']==cfg['pixel_format']
if not auxiliary:assert ref['configuration']['output_bit_depth']==10
frame_bytes=320*240*4 if auxiliary else cfg['luma_bytes']+cfg['chroma_bytes']
dma_bytes=cfg['luma_bytes']+cfg['chroma_bytes'];upload_bytes=(dma_bytes+511)//512*512
input_offset=relocator['INPUT_OFFSET']
source=(reference/'input.raw').read_bytes()[:frame_bytes];assert len(source)==frame_bytes
companding=runpy.run_path(str(Path(__file__).with_name('depth-companding.py'))) if auxiliary else None
if auxiliary:
    assert len(companding['encode_f32'](source,a.profile))==320*240*2
else:
    samples=array('H',source)
    assert all(v&63==0 for v in samples) and {v>>6&3 for v in samples}=={0,1,2,3}

def run(command,name):
    with (out/(name+'.log')).open('w') as log:
        result=subprocess.run([str(x) for x in command],stdout=log,stderr=subprocess.STDOUT)
    if result.returncode:raise RuntimeError(name+' failed; inspect saved evidence, no automatic hardware retry')
    return (out/(name+'.log')).read_text(errors='replace')
def native(command,name,timeout=30):
    return run([PYTHON,ROOT/'console.py','--timeout',timeout,command],name)
def replies(text):
    part=text.split('\nAVE_REPLY_BEGIN\n',1)[1].split('\nAVE_REPLY_END\n',1)[0]
    data=bytes.fromhex(' '.join(l.strip() for l in part.splitlines() if re.fullmatch(r'(?: [0-9a-f]{2})+',l)))
    results=[]
    while data:
        channel,size,flags,reserved,address=struct.unpack_from('<IIIIQ',data)
        assert channel==2 and reserved==0 and size in (72,80) and len(data)>=24+size
        results.append(data[24:24+size]);data=data[24+size:]
    return results

if not a.validate_only:
    if a.prepare_only:
        assert a.arena_dma is not None and a.arena_bytes is not None
        arena_dma,arena_bytes,client=a.arena_dma,a.arena_bytes,a.client or 1
    else:
        initial=native(f'cat {DEBUG}/ave-status; printf "AVE_REPLY_BEGIN\\n"; od -An -v -tx1 {DEBUG}/ave-command; printf "AVE_REPLY_END\\n"','initial-state')
        state=dict(re.findall(r'^([a-z0-9_]+)=([^\r\n]+)',initial,re.M))
        assert state['ipc_ready']=='1' and state['command_failed']=='0' and state['powered_domains']=='5'
        last=replies(initial);assert len(last)==1 and struct.unpack_from('<I',last[0])[0] in (1,4)
        client=a.client or struct.unpack_from('<Q',last[0],0x10)[0]+1
        arena_dma,arena_bytes=int(state['arena_dma'],0),int(state['arena_bytes'],0)
        module_sha=hashlib.sha256(a.module_binary.read_bytes()).hexdigest()
        module_check=native('sha256sum /tmp/apple-ave.ko','module-identity')
        assert re.search(r'^'+module_sha+r'\s+/tmp/apple-ave.ko$',module_check,re.M), 'Loaded boot artifact differs'
    assert input_offset+upload_bytes<=arena_bytes
    payload=out/'payload';payload.mkdir();shutil.copyfile(ROOT/'video/linux-ave/run-command.sh',payload/'run.sh')
    def scalar(name,kind,request):
        run([sys.executable,ROOT/'video/ave-make-command.py',kind,payload/(name+'.bin'),
            '--codec','hevc','--client',client,'--request-id',request],'make-'+name)
    scalar('open','open',1);scalar('stop','stop',3+2*a.frames);scalar('close','close',4+2*a.frames)
    originals=[]
    for frame in range(a.frames):
        stride=1280 if auxiliary else 640;shift=(frame*8%320)*(4 if auxiliary else 2)
        image=b''.join(source[y*stride+shift:(y+1)*stride]+source[y*stride:y*stride+shift] for y in range(frame_bytes//stride))
        if auxiliary and a.pattern=='smooth-depth':
            # Translation of a smooth depth ramp exercises temporal precision
            # without adding the row-wrap pattern's artificial near/far edge.
            f32=companding['f32']
            image=source if frame==0 else array('f',(
                f32(v+frame/256) if a.profile=='depth' else f32(1/f32(f32(1/v)+frame/256))
                for v in array('f',source))).tobytes()
        originals.append(image)
        dma=(companding['encode_f32'](image,a.profile)+bytes(64)) if auxiliary else image
        assert len(dma)==dma_bytes
        (payload/f'input-{frame:03d}.raw').write_bytes(dma+bytes(upload_bytes-dma_bytes))
        for kind,index in ([('start',0),('process',0)] if frame==0 else [('process',frame)]):
            packet,meta=relocator['relocate'](a.profile,kind,arena_dma,arena_bytes,client,index)
            name='start' if kind=='start' else f'process-{frame:03d}'
            (payload/(name+'.bin')).write_bytes(packet);(payload/(name+'.json')).write_text(json.dumps(meta,indent=2)+'\n')
        scalar(f'complete-{frame:03d}','complete',4+2*frame)
    (out/'input.raw').write_bytes(b''.join(originals))
    run_meta=dict(profile=a.profile,frames=a.frames,client=client,arena_dma=arena_dma,arena_bytes=arena_bytes,input_offset=input_offset,
        module_binary=str(a.module_binary.resolve()),module_sha256=hashlib.sha256(a.module_binary.read_bytes()).hexdigest(),
        pattern=a.pattern,dma_bytes=dma_bytes,upload_bytes=upload_bytes,input_conversion='float32 to half to L010 LUT; 64 trailing allocation bytes zero' if auxiliary else 'none')
    (out/'run.json').write_text(json.dumps(run_meta,indent=2)+'\n')
    archive=out/'payload.tar.gz'
    with tarfile.open(archive,'w:gz') as tar:
        for f in sorted(payload.iterdir()):tar.add(f,arcname=f.name)
    if a.prepare_only:
        print('HEVC_PRECISION_PREPARED',a.profile,a.frames,archive);raise SystemExit(0)
    remote='/tmp/hevc-precision-'+str(client)
    run([PYTHON,ROOT/'push-linux.py',archive,remote+'.tar.gz'],'upload')
    clear_pages=(cfg['surface_bytes']+16383)//16384
    native(f'mkdir {remote} && tar -xzf {remote}.tar.gz -C {remote} && dd if=/dev/zero of={DEBUG}/ave-arena bs=16384 count={clear_pages} conv=notrunc','prepare')
    def command(name,opcode):
        response=replies(native(f'sh {remote}/run.sh {remote}/{name}.bin',name))
        assert len(response)==1
        words=struct.unpack('<'+'I'*(len(response[0])//4),response[0]);expected=[0]*len(words)
        expected[0],expected[4],expected[8]=opcode,client,0x15 if opcode==8 else opcode
        assert list(words)==expected,(name,words)
    command('open',3);command('start',6)
    for frame in range(a.frames):
        offset,count=input_offset//512,upload_bytes//512
        assert input_offset%512==0 and upload_bytes%512==0
        native(f'dd if={remote}/input-{frame:03d}.raw of={DEBUG}/ave-arena bs=512 seek={offset} count={count} conv=notrunc && '
               f'dd if={DEBUG}/ave-arena of={remote}/verify.raw bs=512 skip={offset} count={count} && '
               f'cmp {remote}/input-{frame:03d}.raw {remote}/verify.raw',f'input-{frame:03d}')
        command(f'process-{frame:03d}',8);command(f'complete-{frame:03d}',9)
        native(f'printf "AVE_ARENA_BEGIN\\n"; dd if={DEBUG}/ave-arena bs=16384 count=239 2>/dev/null | gzip -c | base64; printf "AVE_ARENA_END\\n"',f'arena-{frame:03d}',45)
        print('HEVC_PRECISION_FRAME',a.profile,frame+1,'/',a.frames,flush=True)
    command('stop',7);command('close',4)
    native(f'cat /sys/devices/system/cpu/online; /opt/neo/bin/g17p-vulkan-compute; cat {DEBUG}/ave-status; strings {DEBUG}/ave-firmware-log; dmesg','post-health',45)

# Everything below is repeatable without contacting the device.
run_meta=json.loads((out/'run.json').read_text())
assert run_meta['profile']==a.profile and run_meta['frames']==a.frames
for name in ['open','start','stop','close']+[f'{kind}-{i:03d}' for i in range(a.frames) for kind in ['process','complete']]:
    evidence=(out/(name+'.log')).read_text()
    assert re.search(r'^AVE_PACKET_RESULT=0$',evidence,re.M) and re.search(r'^command_failed=0$',evidence,re.M)
health=(out/'post-health.log').read_text()
assert re.search(r'^0-5$',health,re.M) and 'g17p-vulkan-compute: PASS' in health
assert not re.search(r'CRIT:|kernel panic|SError|dart.*(?:fault|translation error)',health)
segments=[]
for frame in range(a.frames):
    dest=out/f'frame-{frame:03d}.hevc'
    run([sys.executable,ROOT/'video/ave-extract-frame.py',out/f'arena-{frame:03d}.log',dest,
         '--codec','hevc','--hevc-profile',a.profile,'--allow-p-frame'],f'extract-{frame:03d}')
    segments.append(dest.read_bytes())
(out/'video.hevc').write_bytes(b''.join(segments))
if auxiliary:
    run([sys.executable,ROOT/'video/validate-hevc-auxiliary-linux.py',out,'--profile',a.profile], 'auxiliary-validation')
    print('HEVC_AUXILIARY_VALIDATED',a.profile,a.frames,out,flush=True)
    raise SystemExit(0)
run(['ffmpeg','-v','error','-y','-i',out/'video.hevc','-pix_fmt',cfg['pixel_format'],'-f','rawvideo',out/'decoded.raw'],'decode')
decoded=(out/'decoded.raw').read_bytes();original=(out/'input.raw').read_bytes()
assert len(decoded)==len(original)==a.frames*frame_bytes
assert all(decoded[i*frame_bytes:(i+1)*frame_bytes]!=decoded[(i+1)*frame_bytes:(i+2)*frame_bytes] for i in range(a.frames-1))
quality=[]
for frame in range(a.frames):
    x=array('H',decoded[frame*frame_bytes:(frame+1)*frame_bytes]);y=array('H',original[frame*frame_bytes:(frame+1)*frame_bytes])
    mse=sum(((u>>6)-(v>>6))**2 for u,v in zip(x,y))/len(x)
    psnr=10*math.log10(1023**2/mse) if mse else 1000.0;assert psnr>=25
    quality.append(psnr)
run(['ffmpeg','-v','error','-y','-i',reference/'video.mov','-frames:v','1','-pix_fmt',cfg['pixel_format'],'-f','rawvideo',out/'reference-decoded.raw'],'reference-decode')
assert decoded[:frame_bytes]==(out/'reference-decoded.raw').read_bytes(),'First native precision frame differs from hardware reference'
probe=json.loads(run(['ffprobe','-v','error','-show_streams','-of','json',out/'video.hevc'],'probe'))['streams'][0]
assert probe['codec_name']=='hevc' and (probe['width'],probe['height'])==(320,240)
assert probe['pix_fmt']==('yuv420p10le' if a.profile=='main10' else 'yuv422p10le')
assert probe['profile']==('Main 10' if a.profile=='main10' else 'Rext')
frame_probe=json.loads(run(['ffprobe','-v','error','-show_frames','-show_entries','frame=pict_type',
    '-of','json',out/'video.hevc'],'frame-types'))['frames']
assert len(frame_probe)==a.frames and all(f['pict_type'] in ('I','P') for f in frame_probe)
for suffix in ['mp4','mkv']:
    # Each Process is drained before the next input: verified I/P pictures have
    # no reordering. Raw HEVC has no timestamps; assign the known 30 Hz cadence.
    run(['ffmpeg','-v','error','-y','-r','30','-i',out/'video.hevc','-c:v','copy',
        '-bsf:v','setts=pts=N/(30*TB):dts=N/(30*TB):duration=1/(30*TB)',out/('video.'+suffix)],'mux-'+suffix)
    container=json.loads(run(['ffprobe','-v','error','-count_frames','-show_streams','-show_format','-of','json',
        out/('video.'+suffix)],'probe-'+suffix))
    stream=container['streams'][0]
    assert stream['codec_name']=='hevc' and (stream['width'],stream['height'])==(320,240)
    assert stream['pix_fmt']==probe['pix_fmt'] and int(stream['nb_read_frames'])==a.frames
    assert abs(float(container['format']['duration'])-a.frames/30)<0.04
    run(['ffmpeg','-v','error','-y','-i',out/('video.'+suffix),'-pix_fmt',cfg['pixel_format'],'-f','rawvideo',out/(suffix+'-decoded.raw')],'decode-'+suffix)
    assert (out/(suffix+'-decoded.raw')).read_bytes()==decoded
validation=dict(passed=True,codec='HEVC',profile=a.profile,pixel_format=probe['pix_fmt'],bit_depth=10,frames=a.frames,
    width=320,height=240,fps=30,minimum_psnr_db=min(quality),first_frame_matches_macos_reference=True,
    session_closed=True,cpus_online='0-5',vulkan_values=32,client=run_meta['client'],
    hardware_source='native Neo Linux AVE',decoded_sha256=hashlib.sha256(decoded).hexdigest(),
    frame_policy='forced keyframes; inter-frame scheduling remains outside this qualification',
    module_sha256=run_meta['module_sha256'])
(out/'validation.json').write_text(json.dumps(validation,indent=2)+'\n')
print('HEVC_PRECISION_VALIDATED',json.dumps(validation),flush=True)
