#!/usr/bin/env python3
"""Index existing local qualification manifests without copying their large payloads."""
import hashlib
import json
from pathlib import Path

root=Path(__file__).resolve().parents[1]
names=['ave-video-qualified-20260908','other-codecs-reference-qualified-20260909',
    'prores-all-profiles-qualified-20260909','jpeg-mjpeg-qualified-20260909',
    'hevc-precision-qualified-20260909','prores-alpha-precision-qualified-20260909',
    'ave-pool-qualified-20260909','hevc-auxiliary-qualified-20260909']
archives=[]
for name in names:
    path=root/'artifacts'/name/'manifest.json';raw=path.read_bytes();manifest=json.loads(raw)
    files=manifest['files'];assert files
    assert all((path.parent/file).is_file() for file in files)
    archives.append(dict(path='artifacts/'+name,manifest_sha256=hashlib.sha256(raw).hexdigest(),
        file_count=len(files),total_file_bytes=sum(record['bytes'] for record in files.values())))
(root/'QUALIFICATION_ARCHIVES.json').write_text(json.dumps(dict(target='MacBook Neo J700/T8140',
    evidence_date='2026-09-09',payloads_stored_locally_and_gitignored=True,
    note='Index of qualification manifests; each archive contains its exact scope, evidence and dependency hashes.',
    archives=archives),indent=2)+'\n')
print('QUALIFICATION_MANIFESTS_INDEXED',len(archives))
