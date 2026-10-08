#!/usr/bin/env python3
"""Build the isolated cache identity fix and restore the qualified builder."""
import hashlib
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
NORMAL_SOURCE = ROOT / 'mesa-j700/src/gallium/drivers/asahi/agx_disk_cache.c'
FIX_SOURCE = ROOT / 'mesa-j700-gs-cache-20260915/src/gallium/drivers/asahi/agx_disk_cache.c'
VM_SOURCE = '/home/mcruz.guest/neo/sources/mesa-native-gl2-20260913/src/gallium/drivers/asahi/agx_disk_cache.c'
VM_BUILD = '/home/mcruz.guest/neo/build/mesa-native-gl2-20260913'
TARGET = 'src/gallium/targets/dri/libgallium-26.1.4.so'
NORMAL = '2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2'

def vm(*args, **kwargs):
    return subprocess.run(['limactl', 'shell', 'neo-builder', *args], check=True, **kwargs)

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def vm_sha(path):
    return vm('sha256sum', path, capture_output=True, text=True).stdout.split()[0]

def copy(path):
    subprocess.run(['limactl', 'copy', str(path), 'neo-builder:' + VM_SOURCE], check=True)
    vm('touch', VM_SOURCE)

def build(log):
    with (HERE / log).open('wb') as output:
        vm('ninja', '-C', VM_BUILD, TARGET, stdout=output, stderr=subprocess.STDOUT)

assert vm_sha(VM_SOURCE) == sha(NORMAL_SOURCE)
assert vm_sha(VM_BUILD + '/' + TARGET) == NORMAL
candidate = HERE / 'candidates/libgallium-cache-identity-fix.so'
assert not candidate.exists()
try:
    copy(FIX_SOURCE)
    build('cache-identity-fix-build.log')
    subprocess.run(['limactl', 'copy', 'neo-builder:' + VM_BUILD + '/' + TARGET, str(candidate)], check=True)
    print('FIX_BUILT', sha(candidate), flush=True)
finally:
    copy(NORMAL_SOURCE)
    build('cache-identity-fix-restore-build.log')
    assert vm_sha(VM_SOURCE) == sha(NORMAL_SOURCE)
    assert vm_sha(VM_BUILD + '/' + TARGET) == NORMAL
    print('NORMAL_BUILDER_RESTORED', NORMAL, flush=True)
