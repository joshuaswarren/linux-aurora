#!/usr/bin/env python3
"""Create a dedicated, bounded OBS hardware test profile in the RAM desktop."""
from pathlib import Path
import argparse
import os
import re
import subprocess

os.umask(0o077)
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--run-name', default='m5-ave-obs-test')
p.add_argument('--v4l2', action='store_true', help='Use the stock FFmpeg output and a scene-only fixture')
a = p.parse_args()
assert re.fullmatch(r'm5-ave-obs-test[a-z0-9-]{0,32}', a.run_name)
root = Path('/home/aurora/.config') / a.run_name / 'obs-studio'
assert not root.exists(), 'Preserve any existing test profile'
profile = root / 'basic/profiles/M5AVE'
profile.mkdir(parents=True)
(root / 'basic/scenes').mkdir()
plugin_name = 'obs-m5-v4l2-fixture' if a.v4l2 else 'obs-m5-ave'
plugin = root / f'plugins/{plugin_name}/bin/64bit'
plugin.mkdir(parents=True)
recordings = Path('/home/aurora/Videos') / ('M5-V4L2-test' if a.v4l2 else 'M5-AVE-test')
recordings.mkdir(parents=True, exist_ok=True)
os.chown(recordings.parent, 1000, 1000)
(root / 'global.ini').write_text('[General]\nLastVersion=536936448\n')
(root / 'user.ini').write_text('''[General]
FirstRun=true
Language=en-US

[Basic]
Profile=M5 AVE hardware test
ProfileDir=M5AVE
SceneCollection=M5AVE
SceneCollectionFile=M5AVE

[BasicWindow]
WarnBeforeStartingStream=true
WarnBeforeStoppingStream=true
''')
(profile / 'basic.ini').write_text('''[General]
Name=M5 AVE hardware test

[Video]
BaseCX=320
BaseCY=240
OutputCX=320
OutputCY=240
FPSType=1
FPSInt=1
FPSNum=1
FPSDen=1
ScaleType=bilinear
ColorFormat=NV12
ColorSpace=709
ColorRange=Partial

[Audio]
SampleRate=48000
ChannelSetup=Stereo

[Output]
Mode=Advanced
FilenameFormatting=M5-AVE-%CCYY-%MM-%DD-%hh-%mm-%ss

[AdvOut]
RecType=Standard
RecEncoder=m5_ave_h264
Encoder=obs_x264
RecAudioEncoder=ffmpeg_aac
AudioEncoder=ffmpeg_aac
RecFilePath=/home/aurora/Videos/M5-AVE-test
RecFormat2=mkv
RecTracks=1
RecRescale=false
TrackIndex=1
''')
if a.v4l2:
    config = profile / 'basic.ini'
    content = config.read_text().split('[AdvOut]')[0]
    config.write_text(content + f'''[AdvOut]
RecType=FFmpeg
FFOutputToFile=true
FFFilePath={recordings}
FFFormat=mpegts
FFExtension=ts
FFFormatMimeType=video/MP2T
FFVEncoder=h264_v4l2m2m
FFVEncoderId=27
FFVBitrate=2500
FFVGOPSize=1
FFVCustom=bf=0
FFMCustom=muxdelay=0 muxpreload=0
FFAudioMixes=0
FFRescale=false
Encoder=obs_x264
AudioEncoder=ffmpeg_aac
''')
source = 'fixture-v4l2.c' if a.v4l2 else 'obs-m5-ave.c'
subprocess.run(['gcc', '-shared', '-fPIC', '-O2', '-Wall', '-Wextra', '-Werror',
                '-I/usr/include/obs', '/tmp/m5-obs/' + source,
                '-o', str(plugin / (plugin_name + '.so')), '-lobs', '-lobs-frontend-api'], check=True)
for directory in [root.parent, recordings]:
    for path in [directory, *directory.rglob('*')]:
        os.chown(path, 1000, 1000)
print('M5_OBS_PROFILE_AND_PLUGIN_READY')
