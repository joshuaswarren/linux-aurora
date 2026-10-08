#!/usr/bin/env python3
"""Archive and verify the Mesa source delta used for the DJ AGX3 retest."""
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile

BASE = Path(__file__).resolve().parent
ROOT = BASE.parents[2] / 'mesa-j700'
OLD = '1cf27f9be3d78735a857f06e2256b2c2e34fd1cc'
STASH = 'b095d5fce923e99c778185024e5acd9b1f99c8ab'
SOURCE = '/home/mcruz.guest/neo/sources/mesa-native-gl2-20260913'

def git(*args):
    return subprocess.check_output(['git', '-C', str(ROOT), *args], text=True).strip()

assert git('branch', '--show-current') == 'j700'
assert not git('diff', '--name-only', '--diff-filter=U')
subprocess.run(['git', '-C', str(ROOT), 'diff', '--check'], check=True)
files = set(git('diff', '--name-only', OLD).splitlines())
files.update(git('ls-files', '--others', '--exclude-standard').splitlines())
assert files and all((ROOT / f).is_file() for f in files)
manifest = {
    'old_head': OLD, 'head': git('rev-parse', 'HEAD'), 'saved_work': STASH,
    'agx3': git('rev-parse', 'origin/agx3'),
    'agx3_gallium': git('rev-parse', 'origin/agx3-gallium'),
    'files': {f: hashlib.sha256((ROOT / f).read_bytes()).hexdigest()
              for f in sorted(files)},
}
archive = BASE / 'dj-agx3-source-overlay.tar.gz'
with tarfile.open(archive, 'w:gz') as out:
    for f in sorted(files):
        out.add(ROOT / f, arcname=f)
(BASE / 'dj-agx3-source-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
subprocess.run(['limactl', 'shell', 'neo-builder', 'tar', '-xzf', str(archive),
                '-C', SOURCE], check=True)
check = """
import hashlib,json
from pathlib import Path
m=json.loads(Path(__import__('sys').argv[1]).read_text())
r=Path(__import__('sys').argv[2])
bad=[f for f,h in m['files'].items() if hashlib.sha256((r/f).read_bytes()).hexdigest()!=h]
assert not bad,bad
print('Source overlay verified:',len(m['files']),'files')
"""
subprocess.run(['limactl', 'shell', 'neo-builder', 'python3', '-c', check,
                str(BASE / 'dj-agx3-source-manifest.json'), SOURCE], check=True)
