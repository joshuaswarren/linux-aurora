#!/usr/bin/env python3
"""Record declaration-level compatibility; does not claim runtime qualification."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
MESA = ROOT / 'mesa-j700-agx-sync-20260916'
KERNEL = ROOT / 'neo-encoder-linux-j700-readback-20260915'
LIVE_REF = 'b79a17bbe78'


def git(repo, *args):
    return subprocess.check_output(['git', *args], cwd=repo)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def declarations(data):
    text = re.sub(r'/\*.*?\*/', '', data.decode(), flags=re.S)
    return '\n'.join(re.sub(r'\s+', ' ', line).strip()
                     for line in text.splitlines() if line.strip()).encode()


candidate = (MESA / 'include/drm-uapi/asahi_drm.h').read_bytes()
kernel = (KERNEL / 'include/uapi/drm/asahi_drm.h').read_bytes()
live = git(MESA, 'show', LIVE_REF + ':include/drm-uapi/asahi_drm.h')
report = {
    'candidate_head': git(MESA, 'rev-parse', 'HEAD').decode().strip(),
    'kernel_head': git(KERNEL, 'rev-parse', 'HEAD').decode().strip(),
    'live_mesa_reference': git(MESA, 'rev-parse', LIVE_REF).decode().strip(),
    'candidate_header_sha256': sha(candidate),
    'kernel_header_sha256': sha(kernel),
    'live_header_sha256': sha(live),
    'candidate_matches_live_header': candidate == live,
    'candidate_matches_kernel_declarations': declarations(candidate) == declarations(kernel),
    'normalized_declarations_sha256': sha(declarations(candidate)),
    'hardware_execution': False,
    'runtime_compatibility_verified': False,
    'scope': 'Shared UAPI declarations only; GPU firmware commands and runtime behaviour still need hardware qualification.',
}
(HERE / 'agx-kernel-interface-audit.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))
assert report['candidate_matches_live_header']
assert report['candidate_matches_kernel_declarations']
