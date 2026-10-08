#!/usr/bin/env python3
"""Build the existing readback observer, then restore and rebuild normal Mesa."""
import hashlib
from pathlib import Path
import subprocess

here = Path(__file__).resolve().parent
root = here.parents[2]
normal = root / 'mesa-j700/src/gallium/drivers/asahi/agx_pipe.c'
observer = here / 'probes/agx_pipe-fence-observe.c'
vm_source = '/home/mcruz.guest/neo/sources/mesa-native-gl2-20260913/src/gallium/drivers/asahi/agx_pipe.c'
vm_build = '/home/mcruz.guest/neo/build/mesa-native-gl2-20260913'
target = 'src/gallium/targets/dri/libgallium-26.1.4.so'
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
assert digest(normal) == '64de985f842329491f2fa092653e3c54b3791c98c311596a6dbcbfcf4a51222f'


def vm_digest(path):
    return subprocess.check_output(['limactl', 'shell', 'neo-builder', 'sha256sum', path], text=True).split()[0]


def install_source(path):
    subprocess.run(['limactl', 'copy', str(path), 'neo-builder:' + vm_source], check=True)
    subprocess.run(['limactl', 'shell', 'neo-builder', 'touch', vm_source], check=True)


def build(log):
    with (here / log).open('wb') as output:
        subprocess.run(['limactl', 'shell', 'neo-builder', 'ninja', '-C', vm_build, target],
                       stdout=output, stderr=subprocess.STDOUT, check=True)


assert vm_digest(vm_source) == digest(normal)
normal_binary_sha = vm_digest(vm_build + '/' + target)
assert normal_binary_sha == '2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2'
candidate = here / 'candidates/libgallium-render-stamp-observe.so'
assert not candidate.exists()
try:
    install_source(observer)
    build('render-stamp-observer-build.log')
    subprocess.run(['limactl', 'copy', 'neo-builder:' + vm_build + '/' + target, str(candidate)], check=True)
    print('OBSERVER_BUILT', digest(candidate), flush=True)
finally:
    install_source(normal)
    build('render-stamp-observer-restore-build.log')
    assert vm_digest(vm_source) == digest(normal)
    assert vm_digest(vm_build + '/' + target) == normal_binary_sha
    print('NORMAL_SOURCE_AND_BINARY_RESTORED', normal_binary_sha, flush=True)
