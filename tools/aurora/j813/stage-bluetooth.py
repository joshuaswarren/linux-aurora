#!/usr/bin/env python3
"""Stage only Bluetooth modules and public BlueZ tools for a RAM-only test."""
from pathlib import Path
import re
import shutil
import subprocess
import tarfile

root = Path('/Users/mcruz/Developer/aurora-silicon/j813-linux')
output = root / 'artifacts/bluetooth'
stage = output / 'ram-test'
for directory in ('bin', 'lib', 'modules'):
    (stage / directory).mkdir(parents=True, exist_ok=True)

tool_names = ('btmgmt', 'btmon', 'l2ping', 'sdptool')
for binary in tool_names:
    source = Path.home() / 'j813-bluez-tools/root/usr/bin' / binary
    shutil.copy2(source, stage / 'bin' / binary)
    listing = subprocess.check_output(['ldd', str(source)], text=True)
    if 'not found' in listing:
        raise RuntimeError('Unresolved BlueZ dependency')
    for library in re.findall(r'(/\S+)', listing):
        source_library = Path(library)
        shutil.copy2(source_library, stage / 'lib' / source_library.name)

modules = output / 'stack/root/lib/modules/7.1.6-aurora-j813'
dependencies = {}
for line in (modules / 'modules.dep').read_text().splitlines():
    name, deps = line.split(':', 1)
    dependencies[name] = deps.split()
ordered = []

def visit(name):
    if name in ordered:
        return
    for dependency in dependencies[name]:
        visit(dependency)
    ordered.append(name)

for name in sorted(dependencies):
    visit(name)
for module in ordered:
    shutil.copy2(modules / module, stage / 'modules' / Path(module).name)
shutil.copy2(output / 'btcentauri.ko', stage / 'modules/btcentauri.ko')
(stage / 'load-core.sh').write_text('#!/bin/sh\nset -eu\n' + ''.join(
    'insmod /run/bt/modules/' + Path(module).name + '\n' for module in ordered))
for binary in tool_names:
    (stage / binary).write_text('#!/bin/sh\nexec /run/bt/lib/ld-linux-aarch64.so.1 '
        f'--library-path /run/bt/lib /run/bt/bin/{binary} "$@"\n')
for name in ('load-core.sh', *tool_names):
    (stage / name).chmod(0o755)
export = output / 'transfer'
export.mkdir(exist_ok=True)
archive = export / 'bluetooth-ram-test.tgz'
with tarfile.open(archive, 'w:gz') as stream:
    for child in stage.iterdir():
        stream.add(child, arcname=child.name)
print('Staged Bluetooth dependencies:', ', '.join(Path(name).name for name in ordered))
print('Bluetooth RAM package bytes:', archive.stat().st_size)
