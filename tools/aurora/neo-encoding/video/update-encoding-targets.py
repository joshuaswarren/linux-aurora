#!/usr/bin/env python3
"""Derive the target ledger from saved qualification artifacts."""
import json
from pathlib import Path

r=Path(__file__).resolve().parents[1]
targets=[]
def add(name,status,evidence,note=''):
    assert status in ['candidate','hardware advertised','reference validated','Linux validated']
    targets.append(dict(target=name,status=status,evidence=evidence,note=note))

for name in ['H.264','HEVC Main']:
    add(name,'Linux validated',['artifacts/ave-video-qualified-20260908'])
prores=r/'artifacts/prores-all-profiles-qualified-20260909/SCOPE.json'
assert json.loads(prores.read_text())['status']=='Linux validated'
for name in ['422','HQ','LT','Proxy','4444','4444 XQ']:
    add('ProRes '+name,'Linux validated',['artifacts/prores-all-profiles-qualified-20260909'],
        '8-bit packed source; 4444/XQ alpha opaque. High-precision variable alpha is separate.')
for name in ['JPEG','Motion JPEG']:
    path='artifacts/jpeg-mjpeg-qualified-20260909'
    assert json.loads((r/path/'SCOPE.json').read_text())['status']=='Linux validated'
    add(name,'Linux validated',[path],'320x240 Baseline, 8-bit 4:2:0; one frame and 60 changing frames.')
precision='artifacts/hevc-precision-qualified-20260909'
for name in ['Main10','Main42210']:
    qualified=(r/precision/'manifest.json').exists()
    add('HEVC '+name,'Linux validated' if qualified else 'reference validated',
        [precision if qualified else 'artifacts/ave-hevc-'+name.lower()+'-capture-20260909'],
        'Real 10-bit source. Linux sequence forces keyframes; inter-frame scheduling is not qualified.' if qualified else 'Native qualification in progress.')
for name,codec in [('ProRes 4444 alpha / 12-bit input','ap4h'),('ProRes 4444 XQ alpha / 12-bit input','ap4x')]:
    reference='artifacts/'+codec+'-alpha12-reference-validation-20260909'
    assert json.loads((r/reference/'validation.json').read_text())['passed']
    native='artifacts/prores-alpha-precision-qualified-20260909'
    qualified=(r/native/'manifest.json').exists()
    add(name,'Linux validated' if qualified else 'reference validated',[native if qualified else reference],
        'Real 12-bit color; all 16 alpha bits preserved through independent decode, one frame and 60 changing frames.' if qualified else
        'Hardware reference encoded and independently decoded. Native DMA input capture pending.')
add('HEVC alpha','hardware advertised',['artifacts/coverage-reference-20260909/guest/hevc-alpha'],
    'PreserveAlphaChannel rejected. Corrected EncodesAlpha/readback probe is prepared but blocked by macOS reference boot failures; no native qualification.')
for name in ['depth','disparity']:
    reference='artifacts/hevc-'+name+'-host-reference-decode-20260909'
    assert json.loads((r/reference/'validation.json').read_text())['passed']
    add('HEVC '+name,'reference validated',[reference],
        'Hardware-required Neo encode independently decoded to float32 on the host from 10-bit monochrome HEVC. Lossy reference; missing native input mapping is blocked by macOS reference boot failures.')
add('MV-HEVC','reference validated',['artifacts/mvhevc-reference-validation-20260909'],
    'Two independently decoded, distinct views. Larger native AVE pool passed baseline regressions, but complete multi-layer command/input capture is blocked by macOS reference boot failures.')
audio='artifacts/audio-hardware-explicit-20260909'
inventory=json.loads((r/audio/'inventory.json').read_text())
assert inventory['status']==0 and inventory['model']=='Mac17,5' and inventory['hardware_encoder_count']==0
for record in inventory['formats']:
    code=record['format']
    name={'aac ':'AAC-LC','apac':'APAC'}.get(code,code)
    add('Audio '+name,'candidate',[audio,'artifacts/auxiliary-reference-20260909/guest/audio'],
        'No hardware-advertised implementation in the Neo inventory. AAC hardware selection also accepted a nonexistent manufacturer, so conversion is not hardware proof.')
for name in ['ProRes RAW','ProRes RAW HQ','AV1','VP8','VP9']:
    add(name,'candidate',['A18_ENCODING_TARGET_AUDIT.md'],
        'Outside implementation commitments until new hardware evidence appears.')
(r/'ENCODING_TARGETS.json').write_text(json.dumps(dict(target='Neo T8140',
    evidence_date='2026-09-09',targets=targets),indent=2)+'\n')
print('ENCODING_TARGET_LEDGER',len(targets),'targets')
