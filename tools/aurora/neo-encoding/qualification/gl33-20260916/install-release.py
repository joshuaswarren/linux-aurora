#!/usr/bin/env python3
"""Install the verified tagged CTS package into its isolated prefix."""
import hashlib
import json
import os
from pathlib import Path
import sys
import tarfile

boot = sys.argv[1]
platform = sys.argv[2] if len(sys.argv) > 2 else 'surfaceless'
assert platform in ('surfaceless', 'x11')
suffix = '-x11' if platform == 'x11' else ''
assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
archive = Path('/home/mcruz/gl30-cts-20260913/cts-opengl-4.6.8.1'+suffix+'.tar.gz')
expected_sha = {
    'surfaceless': 'f6d7a6971372ea0b60644650e0be173b703ab80b32e04d6a8cfbcbc00f8a38b8',
    'x11': 'eb082f8382f0a4e47f6a95ae6f4c77cc9fbe311df61639a0555678c3e7bf666a',
}[platform]
assert hashlib.sha256(archive.read_bytes()).hexdigest() == expected_sha
prefix = Path('/opt/neo-cts-opengl-4.6.8.1'+suffix)
assert not prefix.exists()
with tarfile.open(archive) as package:
    members = package.getmembers()
    for entry in members:
        assert entry.name == 'opt' or entry.name == str(prefix)[1:] or entry.name.startswith(str(prefix)[1:]+'/')
        assert '..' not in Path(entry.name).parts and (entry.isfile() or entry.isdir())
    package.extractall('/', members=[m for m in members if m.name != 'opt'], filter='data')
assert (prefix/'source-commit.txt').read_text().strip() == '067e8832315e79817ede1c4863804e440f5d1c80'
manifest = json.loads((prefix/'package-manifest.json').read_text())
for name, expected in manifest.items():
    path = prefix/name
    assert path.stat().st_size == expected['size'], name
    assert hashlib.sha256(path.read_bytes()).hexdigest() == expected['sha256'], name
print('OFFICIAL_CTS_INSTALLED_AND_VERIFIED files='+str(len(manifest)))
