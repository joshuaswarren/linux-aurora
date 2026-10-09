#!/usr/bin/env python3
"""Run bounded native APR submissions and validate the returned ProRes frames."""
import argparse
from array import array
import base64
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import struct
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
PYTHON = ROOT.parent / 'neo-update/.venv/bin/python'
DEBUG = '/sys/kernel/debug/apple-prores-test'
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('packet', type=Path)
parser.add_argument('reference', type=Path, help='The matching macOS probe directory')
parser.add_argument('output', type=Path)
parser.add_argument('--frames', type=int, default=1)
parser.add_argument('--warm', action='store_true')
parser.add_argument('--profile', choices=['standard','hq','lt','proxy','4444','xq'], help='Require the matching captured profile')
parser.add_argument('--pixel-format', choices=['uyvy422','ayuv64le'], help='Require the matching source layout')
parser.add_argument('--bit-depth', type=int, choices=[8,12], help='Effective source color precision; AYUV uses 16-bit storage')
parser.add_argument('--alpha', choices=['none','opaque','preserve'], help='Require the captured alpha policy')
parser.add_argument('--target-packetgen',action='store_true',help='Use the byte-verified target file generator to avoid repeated input uploads')
parser.add_argument('--prepare-only', action='store_true', help='Validate and construct packets without opening KIS')
parser.add_argument('--validate-only', action='store_true', help='Recheck saved native evidence without opening KIS')
args = parser.parse_args()
assert 1 <= args.frames <= 60
out = args.output.resolve()
assert not (args.prepare_only and args.validate_only)
if args.validate_only:
    assert out.is_dir()
else:
    out.mkdir(parents=True, exist_ok=False)


def run(command, name):
    if command[0]=='ffmpeg':
        command=[command[0],'-y',*command[1:]]
    with (out / f'{name}.log').open('w') as log:
        result = subprocess.run([str(x) for x in command], stdout=log, stderr=subprocess.STDOUT)
    if result.returncode:
        raise RuntimeError(f'{name} failed; inspect its log. No automatic hardware retry.')
    return (out / f'{name}.log').read_text(errors='replace')


def native(command, name, timeout=30):
    return run([PYTHON, ROOT / 'console.py', '--timeout', timeout, command], name)


packet = args.packet.read_bytes()
magic, size, writes, relocations, n0, n1, n2, reserved = struct.unpack_from('<8I', packet)
assert magic == 0x31525041 and size == 384 and reserved == 0
desc = packet[32 + writes * 8:32 + writes * 8 + size]
relocs = packet[32 + writes * 8 + size:32 + writes * 8 + size + relocations * 12]
planes = packet[32 + writes * 8 + size + relocations * 12:]
assert len(planes) == n0 + n1 + n2 and max(n0,n1,n2) <= 1024*1024
metadata = json.loads((args.reference / 'result.json').read_text())
assert metadata['model'] == 'Mac17,5' and metadata['passed'] and metadata['hardware_required']
assert metadata['width'] == 320 and metadata['height'] == 240 and '25G76' in metadata['system_version']
codec = metadata['codec']
profiles = {'apcn':'Standard','apch':'HQ','apcs':'LT','apco':'Proxy','ap4h':'4444','ap4x':'XQ'}
assert codec in profiles
input_format = metadata['input_pixel_format']
assert input_format in ('uyvy422', 'ayuv64le'), 'Only captured packed source layouts are qualified here'
source_bpp = 2 if input_format == 'uyvy422' else 8
reference = (args.reference / 'input.raw').read_bytes()[:320*240*source_bpp]
assert len(reference) == 320*240*source_bpp
flags = struct.unpack_from('<I',desc)[0]
assert flags & 3 == 3 and not flags & 0x40, 'Require observed linear 16-bit plane layout'
subsampling = (flags >> 2) & 3
assert subsampling in (2,3)
alpha = bool(flags & 0x100)
assert bool(n2) == alpha
expected_profile={'apcn':'standard','apch':'hq','apcs':'lt','apco':'proxy','ap4h':'4444','ap4x':'xq'}[codec]
source_bits=8 if input_format=='uyvy422' else 12
alpha_policy='none' if not alpha else 'opaque' if input_format=='uyvy422' else 'preserve'
assert args.profile is None or args.profile==expected_profile, 'Requested profile differs from capture'
assert args.pixel_format is None or args.pixel_format==input_format, 'Requested pixel layout differs from capture'
assert args.bit_depth is None or args.bit_depth==source_bits, 'Requested precision differs from capture'
assert args.alpha is None or args.alpha==alpha_policy, 'Requested alpha policy differs from capture'
if input_format == 'ayuv64le':
    assert alpha and subsampling == 3 and codec in ('ap4h','ap4x')
    source_words = array('H', reference)
    assert all(v & 15 == 0 for channel in (1,2,3) for v in source_words[channel::4])
    assert all({v >> 4 & 15 for v in source_words[channel::4]} == set(range(16)) for channel in (1,2,3))
    assert len(set(source_words[::4])) > 4096, 'Require changing high-precision alpha input'
strides = [struct.unpack_from('<H',desc,0x20+2*i)[0]*64 for i in range(3)]
plane_bpp = [2,2 if subsampling == 2 else 4,2 if flags & 0x80 else 1]
plane_sizes = [n0,n1,n2]
relocation_list = [struct.unpack_from('<III',relocs,12*i) for i in range(relocations)]
plane_offsets = []
for index,slot in enumerate((0x28,0x38,0x48)):
    if not plane_sizes[index]:
        plane_offsets.append(0)
        continue
    assert not any(off == slot+8 for off,_,_ in relocation_list), 'Tiled/compressed input cannot be shifted as linear rows'
    candidates = [(buffer,offset) for off,buffer,offset in relocation_list if off == slot]
    assert len(candidates)==1 and candidates[0][0]==index+1
    offset=candidates[0][1]
    assert strides[index] >= 320*plane_bpp[index]
    assert offset + 239*strides[index] + 320*plane_bpp[index] <= plane_sizes[index]
    plane_offsets.append(offset)
if input_format == 'ayuv64le':
    expected_uv=array('H')
    for cb,cr in zip(source_words[2::4],source_words[3::4]):expected_uv.extend((cb,cr))
    expected_planes=[source_words[1::4].tobytes(),expected_uv.tobytes(),source_words[::4].tobytes()]
    for index,base in enumerate((0,n0,n0+n1)):
        row_bytes=320*plane_bpp[index]
        visible=b''.join(planes[base+plane_offsets[index]+y*strides[index]:base+plane_offsets[index]+y*strides[index]+row_bytes] for y in range(240))
        assert visible==expected_planes[index], 'Captured AYUV16 plane does not match the source component layout'
input_path=out/('input.uyvy' if input_format=='uyvy422' else 'input.ayuv64le')
if not args.validate_only:
    payload_dir = out / 'payload'
    payload_dir.mkdir()
    originals = []
    for frame in range(args.frames):
        pixels_shift = frame * 8 % 320
        source = bytearray(planes)
        for index,base in enumerate((0,n0,n0+n1)):
            if not plane_sizes[index]: continue
            shift = pixels_shift*plane_bpp[index]
            for y in range(240):
                start=base+plane_offsets[index]+y*strides[index]
                row=planes[start:start+320*plane_bpp[index]]
                source[start:start+len(row)]=row[shift:]+row[:shift]
        count = writes if frame == 0 and not args.warm else 0
        header = struct.pack('<8I', magic, size, count, relocations, n0, n1, n2, 0)
        data = header + packet[32:32+count*8] + desc + relocs + source
        (payload_dir / f'packet-{frame:03d}.bin').write_bytes(data)
        shift=pixels_shift*source_bpp
        originals.append(b''.join(reference[y*320*source_bpp+shift:(y+1)*320*source_bpp]+
                                 reference[y*320*source_bpp:y*320*source_bpp+shift] for y in range(240)))
    input_path.write_bytes(b''.join(originals))
    archive = out / 'packets.tar.gz'
    with tarfile.open(archive, 'w:gz') as tar:
        for path in sorted(payload_dir.iterdir()):
            tar.add(path, arcname=path.name)
    if args.prepare_only:
        print('PRORES_PACKETS_PREPARED',codec,args.frames,str(archive),flush=True)
        raise SystemExit(0)
    state = native(f'uname -r; cat /proc/device-tree/compatible; echo; cat {DEBUG}/status', 'initial')
    assert '7.1.6-aurora-neo' in state and 'apple,t8140' in state
    assert re.search(r'^failed=0\r?$', state, re.M)
    assert re.search(r'^started=' + ('1' if args.warm else '0') + r'\r?$', state, re.M)
    old_frames = int(re.search(r'^frames=(\d+)', state, re.M)[1])
    assert (old_frames + args.frames) * size <= 128*1024, 'Reboot before exhausting the bounded ring'
    remote = '/tmp/prores-run-' + hashlib.sha256(str(out).encode()).hexdigest()[:10]
    if args.target_packetgen:
        gen_dir=ROOT/'artifacts/prores-packetgen-20260909'
        generator=gen_dir/'prores-packetgen'
        qualified=json.loads((gen_dir/'validation.json').read_text())
        assert qualified['passed'] and qualified['binary_sha256']==hashlib.sha256(generator.read_bytes()).hexdigest()
        (out/'packetgen-validation.json').write_text(json.dumps(qualified,indent=2)+'\n')
        run([PYTHON,ROOT/'push-linux.py',generator,'/tmp/prores-packetgen'],'upload-generator')
        run([PYTHON,ROOT/'push-linux.py',args.packet,'/tmp/prores-template.bin'],'upload-template')
        native(f'mkdir {remote} && chmod 700 /tmp/prores-packetgen && /tmp/prores-packetgen '
            f'/tmp/prores-template.bin {args.frames} {remote} '+('warm' if args.warm else 'cold'),'unpack')
    else:
        run([PYTHON, ROOT / 'push-linux.py', archive, '/tmp/prores-packets.tar.gz'], 'upload')
        native(f'mkdir {remote} && tar -xzf /tmp/prores-packets.tar.gz -C {remote}', 'unpack')
    for frame in range(args.frames):
        name = f'{frame:03d}'
        native(f'dd if={remote}/packet-{name}.bin of={DEBUG}/submit bs=4194304 count=1 && '
               f'cat {DEBUG}/output > {remote}/frame-{name}.sample && cat {DEBUG}/status', f'frame-{name}')
        print(f'NATIVE_PRORES_FRAME {frame + 1}/{args.frames}', flush=True)
    text = native(f'printf "APR_OUTPUT_BEGIN\\n"; (cd {remote} && tar -czf - frame-*.sample) | '
                  'base64; printf "APR_OUTPUT_END\\n"', 'download', timeout=180 if input_format=='ayuv64le' else 60)
    encoded = text.replace('\r', '').split('\nAPR_OUTPUT_BEGIN\n', 1)[1].split('\nAPR_OUTPUT_END\n', 1)[0]
    data = base64.b64decode(''.join(line.strip() for line in encoded.splitlines()), validate=True)
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as tar:
        members = tar.getmembers()
        assert len(members) == args.frames
        assert {m.name for m in members} == {f'frame-{n:03d}.sample' for n in range(args.frames)}
        for member in members:
            assert member.isfile() and 0 < member.size <= 4 * 1024 * 1024
            sample = tar.extractfile(member).read()
            assert struct.unpack_from('>I', sample)[0] == len(sample) and sample[4:8] == b'icpf'
            (out / member.name).write_bytes(sample)
    final = native(f'cat {DEBUG}/status; cat /sys/devices/system/cpu/online; '
                   '/opt/neo/bin/g17p-vulkan-compute; dmesg | tail -15', 'final', timeout=45)
    assert re.search(r'^frames=' + str(old_frames + args.frames) + r'\r?$', final, re.M)
    assert re.search(r'^failed=0\r?$', final, re.M)
    assert re.search(r'^0-5\r?$', final, re.M)
    assert 'g17p-vulkan-compute: PASS' in final
else:
    state=(out/'initial.log').read_text()
    final=(out/'final.log').read_text()
    old_frames=int(re.search(r'^frames=(\d+)',state,re.M)[1])
    assert re.search(r'^frames='+str(old_frames+args.frames)+r'\r?$',final,re.M)
    assert re.search(r'^failed=0\r?$',final,re.M) and re.search(r'^0-5\r?$',final,re.M)
    assert 'g17p-vulkan-compute: PASS' in final
    for frame in range(args.frames):
        evidence=(out/f'frame-{frame:03d}.log').read_text()
        assert re.search(r'^failed=0\r?$',evidence,re.M)
        assert re.search(r'^frames='+str(old_frames+frame+1)+r'\r?$',evidence,re.M)
run(['ffmpeg', '-v', 'error', '-f', 'image2', '-framerate', 30, '-c:v', 'prores',
     '-i', out / 'frame-%03d.sample', '-c:v', 'copy', '-tag:v', codec, out / 'video.mov'], 'mux')
probe=json.loads(run(['ffprobe','-v','error','-show_streams','-of','json',out/'video.mov'],'probe'))['streams'][0]
assert probe['codec_name']=='prores' and probe['profile']==profiles[codec]
assert probe['width']==320 and probe['height']==240 and int(probe['nb_frames'])==args.frames
decode_format = 'yuva444p12le' if alpha else 'yuv444p12le' if subsampling==3 else 'uyvy422'
decoded_path=out/('decoded.uyvy' if decode_format=='uyvy422' else 'decoded.raw')
run(['ffmpeg', '-v', 'error', '-i', out / 'video.mov', '-f', 'rawvideo', '-pix_fmt', decode_format, decoded_path], 'decode')
decoded = decoded_path.read_bytes()
decoded_frame_bytes = 320*240*(8 if alpha else 6 if subsampling==3 else 2)
assert len(decoded) == args.frames * decoded_frame_bytes
alpha_precision = None
if input_format == 'ayuv64le':
    source_words = array('H', input_path.read_bytes())
    decoded_words = array('H', decoded)
    count = 320*240
    for frame in range(args.frames):
        actual = decoded_words[frame*count*4+3*count:(frame+1)*count*4]
        expected = array('H', (source_words[(frame*count+i)*4] >> 4 for i in range(count)))
        assert actual == expected, 'Alpha differs from the captured 12-bit decode precision'
        assert len(set(actual)) > 256, 'Alpha precision collapsed to 8 bits'
        if frame:
            previous = decoded_words[(frame-1)*count*4+3*count:frame*count*4]
            assert actual != previous, 'Transparency did not change between frames'
    alpha_precision = {'source_color_bits':12,'source_alpha_bits':16,'decoded_alpha_bits':12,
        'alpha_matches_source_top_12_bits':True,'alpha_max_error_at_12_bits':0}
    # FFmpeg's 12-bit ProRes output truncates alpha to its output precision.
    # Independently decode AYUV16 with the host reader to test all alpha bits.
    decoder=ROOT/'artifacts/coverage-probes-alpha16-decode-20260909/neo-vt-decode-probe'
    movie_hash=hashlib.sha256((out/'video.mov').read_bytes()).hexdigest()
    alpha_dir=out/('alpha16-'+movie_hash[:16])
    if not alpha_dir.exists():
        run([decoder,out/'video.mov',alpha_dir,'y416','--allow-host-decoder'],'alpha16-decode')
    alpha_result=json.loads((alpha_dir/'result.json').read_text())
    assert alpha_result['passed'] and alpha_result['format']=='y416' and alpha_result['frames']==args.frames
    full_precision=array('H',(alpha_dir/'decoded.raw').read_bytes())
    assert len(full_precision)==len(source_words) and full_precision[::4]==source_words[::4], '16-bit alpha was not preserved'
    alpha_precision.update(decoded_alpha_bits=16,alpha_matches_all_source_bits=True,
        alpha_max_error_at_16_bits=0,independent_ffmpeg_alpha_bits=12,
        host_decoder_model=alpha_result['model'],movie_sha256=movie_hash,
        decoder_sha256=hashlib.sha256(decoder.read_bytes()).hexdigest())
stats = out / 'psnr.txt'
quality_format = 'yuva444p12le' if alpha else 'yuv444p12le' if subsampling==3 else 'yuv422p'
run(['ffmpeg', '-v', 'error', '-i', out / 'video.mov', '-f', 'rawvideo', '-pixel_format', input_format,
     '-video_size', '320x240', '-framerate', 30, '-i', input_path, '-filter_complex',
     f'[0:v]format={quality_format}[a];[1:v]format={quality_format}[b];[a][b]psnr=stats_file={stats}', '-f', 'null', '-'], 'quality')
rows = [dict(word.split(':', 1) for word in line.split()) for line in stats.read_text().splitlines()]
assert len(rows) == args.frames
minimum = min(float(row['psnr_avg']) for row in rows)
assert minimum >= 25
if args.frames > 1:
    assert all(decoded[i*decoded_frame_bytes:(i+1)*decoded_frame_bytes] != decoded[(i+1)*decoded_frame_bytes:(i+2)*decoded_frame_bytes] for i in range(args.frames-1))
reference_movie=args.reference/'video.mov'
if not reference_movie.exists():
    reference_movie=out/'reference-mux.mov'
    run(['ffmpeg','-v','error','-f','image2','-c:v','prores','-i',args.reference/'frame-%03d.sample',
         '-c:v','copy','-tag:v',codec,reference_movie],'reference-mux')
run(['ffmpeg','-v','error','-i',reference_movie,'-frames:v',1,'-f','rawvideo',
     '-pix_fmt',decode_format,out/'reference-decoded.raw'],'reference-decode')
matches = decoded[:decoded_frame_bytes] == (out / 'reference-decoded.raw').read_bytes()
assert matches, 'Native first frame differs from decoded hardware reference'
native_sample=(out/'frame-000.sample').read_bytes()
reference_sample=(args.reference/'frame-000.sample').read_bytes()
padding_bytes=len(reference_sample)-len(native_sample)
# 25G76 pads ProRes samples to 16 bytes and updates the frame-size field.
# The old Standard frame needed 2 bytes; HQ needs 14. Compare every payload
# byte, and permit only the exact zero-filled alignment tail.
bitstream_match = (native_sample[4:]==reference_sample[4:]) or (
    len(reference_sample)==(len(native_sample)+15)&~15 and 0<=padding_bytes<=15 and
    native_sample[4:]==reference_sample[4:len(native_sample)] and
    reference_sample[len(native_sample):]==bytes(padding_bytes))
assert bitstream_match, 'Native compressed payload differs beyond macOS 16-byte sample alignment'
result = {'passed': True, 'codec': 'ProRes '+profiles[codec], 'fourcc':codec, 'scope': 'native Neo Linux hardware',
          'input_pixel_format':input_format, 'decoded_pixel_format':decode_format, 'alpha':alpha,
          'precision_validation':alpha_precision,
          'kernel': '7.1.6-aurora-neo', 'frames': args.frames, 'width': 320, 'height': 240,
          'fps': 30, 'minimum_psnr_db': minimum, 'first_frame_matches_macos_reference': matches,
          'first_bitstream_matches_macos_except_padding':bitstream_match,
          'macos_alignment_padding_bytes':padding_bytes,
          'packet_sha256': hashlib.sha256(packet).hexdigest(),
          'module_sha256': hashlib.sha256((ROOT / 'artifacts/linux-prores/apple-prores-test.ko').read_bytes()).hexdigest()}
(out / 'validation.json').write_text(json.dumps(result, indent=2) + '\n')
print('NATIVE_PRORES_VALIDATED', json.dumps(result), flush=True)
