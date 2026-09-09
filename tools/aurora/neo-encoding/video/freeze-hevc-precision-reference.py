#!/usr/bin/env python3
"""Freeze complete 10-bit HEVC phases that precede the later MV capture limit."""
import hashlib
import json
from pathlib import Path
import shutil

r=Path(__file__).resolve().parents[1]
for profile in ['main10','main42210']:
    proto=json.loads((r/f'artifacts/hevc-{profile}-protocol-20260909.json').read_text())
    assert proto['transport_validation_passed'] and proto['phase_session_closed']
    out=r/f'artifacts/ave-hevc-{profile}-capture-20260909';assert not out.exists()
    out.mkdir();(out/'ave-command-buffers').mkdir()
    for item in proto['pairs']:
        path=Path(proto['run'])/'ave-command-buffers'/item['buffer']
        assert hashlib.sha256(path.read_bytes()).hexdigest()==item['sha256']
        shutil.copyfile(path,out/'ave-command-buffers'/path.name)
    (out/'protocol.json').write_text(json.dumps(proto,indent=2)+'\n')
    shutil.copytree(r/f'artifacts/coverage-reference-20260909/guest/hevc-{profile}',out/'reference')
    files={str(p.relative_to(out)):dict(bytes=p.stat().st_size,sha256=hashlib.sha256(p.read_bytes()).hexdigest())
           for p in sorted(out.rglob('*')) if p.is_file()}
    (out/'manifest.json').write_text(json.dumps(dict(profile=profile,scope='hardware reference; Linux untested',files=files),indent=2)+'\n')
    print('HEVC_PRECISION_REFERENCE_FROZEN',profile,len(files))
