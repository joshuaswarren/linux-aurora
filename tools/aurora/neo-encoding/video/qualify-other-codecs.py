#!/usr/bin/env python3
"""Freeze the additional-codec macOS reference results, clearly scoped."""
import hashlib
import json
from pathlib import Path
import shutil

root = Path(__file__).resolve().parents[1]
destination = root / 'artifacts/other-codecs-reference-qualified-20260909'
assert not destination.exists()
source = root / 'artifacts/other-codecs-20260909/guest-copy'
results = json.loads((source / 'validation-summary.json').read_text())
passed = [r for r in results if r['encoded']]
assert {r['codec'] for r in passed} == {'apcn', 'apch', 'apcs', 'apco', 'ap4h', 'ap4x', 'jpeg'}
assert all(r['decoded_frames'] == 3 and r['minimum_psnr_db'] >= 25 for r in passed)
destination.mkdir()
shutil.copytree(source, destination / 'results')
shutil.copytree(root / 'artifacts/other-codecs-20260909/prores-native-reference', destination / 'prores-native-reference')
for name in ['neo-vt-codec-probe.m', 'validate-other-codecs.py', 'trace-j700-media.py',
             'make-prores-packet.py', 'extract-ave2.py', 'demangle-media-symbols.py']:
    target = destination / 'source' / name
    target.parent.mkdir(exist_ok=True)
    shutil.copyfile(root / 'video' / name, target)
for run in ['20260909-024939Z-kis-90524', '20260909-030705Z-kis-94037', '20260909-031749Z-kis-96031']:
    capture = root.parent / 'neo-update/logs' / run / 'artifacts/hv'
    target = destination / 'captures' / run
    target.mkdir(parents=True)
    for path in capture.iterdir():
        if path.name in ('media-topology.json', 'media-mmio.jsonl') or path.name.startswith('apr-'):
            shutil.copyfile(path, target / path.name)
scope = {'scope': 'Neo macOS 25G76 hardware reference, not native Linux qualification',
         'hardware_encoders_with_decoded_output': [r['codec'] for r in passed],
         'kernelcache_sha256': 'db55e48ac9e9c0f0db8e0e6e4d17efbfe8c077676f9b5eac5dd0c5f7a5f035fd',
         'trace_source_note': 'Source contains subsequent ring-size, exact-length and previous-head fixes. Only apr-01 from the final capture is used for the native packet.',
         'jpeg_hardware_evidence': 'Explicit hardware EncoderID, require-hardware specification, and correlated JPEG0 register activity',
         'native_status': 'See live OTHER_CODECS.md; this archive does not claim native ProRes or JPEG success'}
(destination / 'SCOPE.json').write_text(json.dumps(scope, indent=2) + '\n')
files = {str(p.relative_to(destination)): {'bytes': p.stat().st_size,
         'sha256': hashlib.sha256(p.read_bytes()).hexdigest()}
         for p in sorted(destination.rglob('*')) if p.is_file()}
(destination / 'manifest.json').write_text(json.dumps({'files': files}, indent=2) + '\n')
for name, item in files.items():
    data = (destination / name).read_bytes()
    assert len(data) == item['bytes'] and hashlib.sha256(data).hexdigest() == item['sha256']
print('OTHER_CODECS_REFERENCE_FROZEN', len(files), 'files', sum(i['bytes'] for i in files.values()), 'bytes')
