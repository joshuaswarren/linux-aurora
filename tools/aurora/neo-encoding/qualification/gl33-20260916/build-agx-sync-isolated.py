#!/usr/bin/env python3
"""Build the AGX merge in a fresh builder tree; never install on the Neo."""
from pathlib import Path
import argparse
import hashlib
import json
import subprocess
import tarfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--resume', action='store_true', help='refresh the isolated tree after a stopped build')
parser.add_argument('--upstream', action='store_true', help='build the unmodified pinned upstream control')
args = parser.parse_args()
HERE = Path(__file__).resolve().parent
LABEL = 'agx-upstream-20260916' if args.upstream else 'agx-sync-20260916'
NAME = 'mesa-j700-' + LABEL
SOURCE = HERE.parents[2] / NAME
OUT = HERE / (LABEL + '-linux')
VM_SOURCE = '/home/mcruz.guest/neo/sources/' + NAME
VM_BUILD = '/home/mcruz.guest/neo/build/' + NAME
BASE_MESA = '/home/mcruz.guest/neo/build/mesa-native-gl2-20260913/src/gallium/targets/dri/libgallium-26.1.4.so'

def vm(*args, **kwargs):
    return subprocess.run(['limactl', 'shell', 'neo-builder', *args], check=True, **kwargs)

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

OUT.mkdir(exist_ok=True)
baseline = vm('sha256sum', BASE_MESA, capture_output=True, text=True).stdout.split()[0]
files = sorted(set(subprocess.check_output(['git', '-C', str(SOURCE), 'ls-files', '-z']).decode().split('\0')) - {''})
assert all((SOURCE / f).is_file() or (SOURCE / f).is_symlink() for f in files)
manifest = {'head': subprocess.check_output(['git', '-C', str(SOURCE), 'rev-parse', 'HEAD'], text=True).strip(),
            'merge_head': subprocess.run(['git', '-C', str(SOURCE), 'rev-parse', '--verify', 'MERGE_HEAD'],
                                         capture_output=True, text=True).stdout.strip(),
            'source_files': {f: ({'symlink': str((SOURCE / f).readlink())}
                                if (SOURCE / f).is_symlink() else digest(SOURCE / f))
                             for f in files},
            'baseline_mesa_sha256': baseline, 'vm_source': VM_SOURCE, 'vm_build': VM_BUILD}
(OUT / 'source-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
archive = OUT / 'source.tar.gz'
with tarfile.open(archive, 'w:gz', compresslevel=1) as tar:
    for f in files:
        tar.add(SOURCE / f, arcname=f, recursive=False)
print('Source snapshot', len(files), 'files', digest(archive), flush=True)
# mkdir without -p rejects an existing source/build directory.
if not args.resume:
    vm('mkdir', VM_SOURCE, VM_BUILD)
else:
    vm('test', '-f', VM_BUILD + '/meson-private/coredata.dat')
subprocess.run(['limactl', 'copy', str(archive), 'neo-builder:' + VM_SOURCE + '/source.tar.gz'], check=True)
vm('tar', 'xzf', VM_SOURCE + '/source.tar.gz', '-C', VM_SOURCE)
options = ['-Dvulkan-drivers=asahi', '-Dgallium-drivers=asahi,softpipe',
           '-Dplatforms=x11,wayland', '-Dglx=dri', '-Degl=enabled', '-Dgbm=enabled',
           '-Dopengl=true', '-Dgles1=disabled', '-Dgles2=enabled', '-Dllvm=enabled',
           '-Dgallium-rusticl=false', '-Dbuild-tests=true', '-Dtools=asahi,drm-shim',
           '-Dvideo-codecs=all', '--prefix=/opt/neo-mesa-' + LABEL,
           '--libdir=lib', '--buildtype=debugoptimized']
with (OUT / 'configure.log').open('w') as log:
    vm('meson', 'setup', *(['--reconfigure'] if args.resume else []),
       VM_BUILD, VM_SOURCE, *options, stdout=log, stderr=subprocess.STDOUT)
print('Isolated Meson configuration complete', flush=True)
tests = json.loads(vm('cat', VM_BUILD + '/meson-info/intro-tests.json', capture_output=True, text=True).stdout)
selected = [t for t in tests if any(s.endswith(':asahi') or s == 'asahi' for s in t['suite'])]
targets = sorted({str(Path(t['cmd'][0]).relative_to(VM_BUILD)) for t in selected
                  if t['cmd'][0].startswith(VM_BUILD + '/')})
assert targets
targets.append('src/gallium/targets/dri/libgallium-26.1.4.so')
(OUT / 'test-selection.json').write_text(json.dumps(selected, indent=2) + '\n')
with (OUT / 'build.log').open('w') as log:
    # Build the driver and default libraries first. Meson test below also
    # resolves build_by_default:false dependencies of its Python wrappers.
    vm('ninja', '-j8', '-C', VM_BUILD, 'all', stdout=log, stderr=subprocess.STDOUT)
print('Mesa and',len(selected),'Asahi test executables built', flush=True)
with (OUT / 'tests.log').open('w') as log:
    result = subprocess.run(['limactl', 'shell', 'neo-builder', 'meson', 'test', '-C', VM_BUILD,
                             '--suite', 'asahi', '--print-errorlogs', '-j4'],
                            stdout=log, stderr=subprocess.STDOUT)
print('Asahi test runner exit', result.returncode, flush=True)
restored = vm('sha256sum', BASE_MESA, capture_output=True, text=True).stdout.split()[0]
assert restored == baseline, 'Original baseline Mesa changed during isolated build'
candidate = VM_BUILD + '/src/gallium/targets/dri/libgallium-26.1.4.so'
candidate_sha = vm('sha256sum', candidate, capture_output=True, text=True).stdout.split()[0]
subprocess.run(['limactl', 'copy', 'neo-builder:' + candidate, str(OUT / 'libgallium-candidate.so')], check=True)
(OUT / 'receipt.json').write_text(json.dumps({'source_archive_sha256': digest(archive),
    'candidate_sha256': candidate_sha, 'baseline_sha256_unchanged': restored,
    'test_exit_code': result.returncode, 'test_count': len(selected), 'installed': False}, indent=2) + '\n')
result.check_returncode()
