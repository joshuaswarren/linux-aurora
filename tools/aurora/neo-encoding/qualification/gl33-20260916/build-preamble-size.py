#!/usr/bin/env python3
"""CPU-only compiler metadata validation; restore the shared builder afterward."""
import argparse
import fcntl
import hashlib
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--prefix', default='preamble-size')
args = parser.parse_args()
if not args.prefix or any(c not in 'abcdefghijklmnopqrstuvwxyz0123456789-' for c in args.prefix):
    parser.error('--prefix must contain lowercase letters, digits, or hyphens')

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
BASE = ROOT / 'mesa-j700-gs-cache-20260915'
CANDIDATE = ROOT / 'mesa-j700-preamble-20260915'
VM_SOURCE = '/home/mcruz.guest/neo/sources/mesa-native-gl2-20260913'
VM_BUILD = '/home/mcruz.guest/neo/build/mesa-native-gl2-20260913'
FILES = (
    'src/asahi/compiler/agx_compile.c',
    'src/asahi/compiler/agx_compile.h',
    'src/asahi/compiler/test/test-agx3-compile-fail-closed.c',
)
TARGETS = (
    'src/asahi/compiler/agx3_compile_fail_closed_test',
    'src/asahi/lib/asahi-g17p-precompiled-fill',
)
MESA = VM_BUILD + '/src/gallium/targets/dri/libgallium-26.1.4.so'


def vm(*args, **kwargs):
    return subprocess.run(['limactl', 'shell', 'neo-builder', *args],
                          check=True, **kwargs)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def vm_sha(path):
    return vm('sha256sum', path, capture_output=True, text=True).stdout.split()[0]


def copy(tree):
    for rel in FILES:
        subprocess.run(['limactl', 'copy', str(tree / rel),
                        'neo-builder:' + VM_SOURCE + '/' + rel], check=True)
        vm('touch', VM_SOURCE + '/' + rel)


def build(label):
    with (HERE / (label + '-build.log')).open('wb') as output:
        vm('ninja', '-j8', '-C', VM_BUILD, *TARGETS,
           stdout=output, stderr=subprocess.STDOUT)


def run(label):
    for target in TARGETS:
        with (HERE / (label + '-' + Path(target).name + '.log')).open('wb') as out:
            vm(VM_BUILD + '/' + target, stdout=out, stderr=subprocess.STDOUT)


with (HERE / 'preamble-builder.lock').open('a') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    for rel in FILES:
        assert vm_sha(VM_SOURCE + '/' + rel) == sha(BASE / rel), rel
    mesa_sha = vm_sha(MESA)
    # Establish a reproducible current-source baseline before mutation.
    vm('touch', *(VM_SOURCE + '/' + rel for rel in FILES))
    build(args.prefix + '-baseline')
    run(args.prefix + '-baseline')
    baseline = {target: vm_sha(VM_BUILD + '/' + target) for target in TARGETS}
    print('CPU_BASELINE_PASSED', baseline, flush=True)
    try:
        copy(CANDIDATE)
        build(args.prefix + '-candidate')
        run(args.prefix + '-candidate')
        print('CPU_CANDIDATE_PASSED', flush=True)
    finally:
        copy(BASE)
        build(args.prefix + '-restored')
        run(args.prefix + '-restored')
        for rel in FILES:
            assert vm_sha(VM_SOURCE + '/' + rel) == sha(BASE / rel), rel
        for target in TARGETS:
            assert vm_sha(VM_BUILD + '/' + target) == baseline[target], target
        assert vm_sha(MESA) == mesa_sha
        print('BUILDER_SOURCES_AND_TEST_BINARIES_RESTORED', baseline, flush=True)
        print('BUILDER_MESA_UNCHANGED', mesa_sha, flush=True)
