#!/usr/bin/env python3
"""Validate the isolated preamble composer without changing the Neo."""
import argparse
import fcntl
import hashlib
from pathlib import Path
import re
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--prefix', default='preamble-composition')
parser.add_argument('--gallium', action='store_true',
                    help='also build/save the complete candidate Mesa library')
args = parser.parse_args()
if not args.prefix or any(c not in 'abcdefghijklmnopqrstuvwxyz0123456789-' for c in args.prefix):
    parser.error('--prefix must contain lowercase letters, digits, or hyphens')

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
BASE = ROOT / 'mesa-j700-gs-cache-20260915'
if args.gallium:
    # The shared builder uses the original pre-cache-fix source/library.
    BASE = ROOT / 'mesa-j700'
CANDIDATE = ROOT / 'mesa-j700-preamble-20260915'
VM_SOURCE = '/home/mcruz.guest/neo/sources/mesa-native-gl2-20260913'
VM_BUILD = '/home/mcruz.guest/neo/build/mesa-native-gl2-20260913'
FILES = (
    'src/asahi/lib/agx_ioto_load_core.c',
    'src/asahi/lib/agx_ioto_load_core.h',
    'src/asahi/lib/tests/test-agx-ioto-load-core.c',
    'src/asahi/compiler/agx3_ioto_load_packer.c',
    'src/asahi/compiler/agx3_ioto_load_packer.h',
    'src/asahi/compiler/test/test-agx3-ioto-load-packer.c',
    'src/asahi/compiler/agx3_ioto_uniform_program.c',
    'src/asahi/compiler/agx3_ioto_uniform_program.h',
    'src/asahi/compiler/test/test-agx3-ioto-uniform-program.c',
    'src/asahi/compiler/agx3_slot_core.c',
    'src/asahi/compiler/agx3_slot_core.h',
    'src/asahi/compiler/test/test-agx3-slot-core.c',
    'src/asahi/compiler/meson.build',
    'src/asahi/vulkan/meson.build',
    'src/asahi/compiler/agx_compile.c',
    'src/asahi/compiler/agx_compile.h',
    'src/asahi/compiler/test/test-agx3-compile-fail-closed.c',
    'src/asahi/compiler/agx3_compute_program.c',
    'src/asahi/compiler/agx3_compute_program.h',
    'src/asahi/compiler/agx3_graphics_program.c',
    'src/asahi/compiler/agx3_graphics_program.h',
    'src/asahi/compiler/test/test-agx3-graphics-program.c',
    'src/asahi/lib/agx_ioto_program_core.c',
    'src/asahi/lib/agx_ioto_program_core.h',
    'src/asahi/lib/agx_ioto_preshader_core.c',
    'src/asahi/lib/agx_ioto_preshader_core.h',
    'src/asahi/lib/agx_vdm_state_update_core.h',
    'src/asahi/lib/agx_vdm_state_update_core.c',
    'src/asahi/lib/tests/test-vdm-state-update-core.c',
    'src/asahi/lib/tests/test-agx-ioto-preshader-core.c',
    'src/asahi/vulkan/tests/test-hk-agx3-ioto-program-core.c',
)
TARGETS = (
    'src/asahi/lib/asahi-agx-ioto-load-core',
    'src/asahi/compiler/agx3_ioto_load_packer_test',
    'src/asahi/compiler/agx3_ioto_uniform_program_test',
    'src/asahi/compiler/agx3_slot_core_test',
    'src/asahi/compiler/agx3_compile_fail_closed_test',
    'src/asahi/compiler/agx3_graphics_program_test',
    'src/asahi/lib/asahi-agx-ioto-preshader-core',
    'src/asahi/lib/asahi-agx-ioto-program-core',
    'src/asahi/lib/asahi-g17p-precompiled-fill',
    'src/asahi/lib/asahi-vdm-state-update-core',
    'src/asahi/vulkan/hk_agx3_ioto_program_core_test',
)
MESA = VM_BUILD + '/src/gallium/targets/dri/libgallium-26.1.4.so'
if args.gallium:
    FILES += (
        'src/gallium/drivers/asahi/agx_state.c',
        'src/gallium/drivers/asahi/agx_disk_cache.c',
    )
    TARGETS += ('src/gallium/targets/dri/libgallium-26.1.4.so',)


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
    with (HERE / (args.prefix + '-' + label + '-build.log')).open('wb') as out:
        vm('ninja', '-j8', '-C', VM_BUILD, *TARGETS,
           stdout=out, stderr=subprocess.STDOUT)


def run(label):
    for target in TARGETS:
        if target.endswith('.so'):
            continue
        name = args.prefix + '-' + label + '-' + Path(target).name + '.log'
        with (HERE / name).open('wb') as out:
            result = subprocess.run(
                ['limactl', 'shell', 'neo-builder', VM_BUILD + '/' + target],
                stdout=out, stderr=subprocess.STDOUT)
        if target.endswith('/hk_agx3_ioto_program_core_test'):
            # This exact pre-existing failure was reproduced before copying
            # any candidate source. The new compute test precedes it in main;
            # all code after the baseline failure remains unexecuted.
            log = (HERE / name).read_text()
            failures = re.findall(r'^FAIL line \d+: (.*)$', log, re.M)
            assert result.returncode == 1 and failures == [
                'hk_agx3_g17p_rt0_tile_layout_supported(&tib)'
            ], (label, result.returncode, failures)
            if label == 'candidate':
                assert 'G17P compute preamble composition cases passed' in log
                print('candidate compute preamble composition PASS', flush=True)
            print(label, Path(target).name,
                  'KNOWN BASELINE FAILURE (packed-integer tile layout)', flush=True)
            continue
        result.check_returncode()
        print(label, Path(target).name, 'PASS', flush=True)


with (HERE / 'preamble-builder.lock').open('a') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    for rel in FILES:
        assert vm_sha(VM_SOURCE + '/' + rel) == sha(BASE / rel), rel
    mesa_sha = vm_sha(MESA)
    if args.gallium:
        saved_library = HERE / 'candidates' / (args.prefix + '-baseline.so')
        candidate_library = HERE / 'candidates' / (args.prefix + '-candidate.so')
        assert not saved_library.exists() and not candidate_library.exists()
        saved_library.parent.mkdir(exist_ok=True)
        subprocess.run(['limactl', 'copy', 'neo-builder:' + MESA,
                        str(saved_library)], check=True)
        assert sha(saved_library) == mesa_sha
    vm('touch', *(VM_SOURCE + '/' + rel for rel in FILES))
    build('baseline')
    run('baseline')
    if args.gallium and vm_sha(MESA) != mesa_sha:
        subprocess.run(['limactl', 'copy', str(saved_library),
                        'neo-builder:' + MESA], check=True)
        raise RuntimeError('Baseline Mesa rebuild was not reproducible; original library restored')
    baseline = {target: vm_sha(VM_BUILD + '/' + target) for target in TARGETS}
    print('CPU_BASELINE_RECORDED_WITH_KNOWN_HK_FAILURE', baseline, flush=True)
    try:
        copy(CANDIDATE)
        build('candidate')
        run('candidate')
        if args.gallium:
            subprocess.run(['limactl', 'copy', 'neo-builder:' + MESA,
                            str(candidate_library)], check=True)
            print('GALLIUM_CANDIDATE_BUILT_NOT_INSTALLED', candidate_library,
                  sha(candidate_library), flush=True)
        print('CPU_CANDIDATE_CHECKS_MATCH_EXPECTED_BASELINE', flush=True)
    finally:
        copy(BASE)
        build('restored')
        run('restored')
        for rel in FILES:
            assert vm_sha(VM_SOURCE + '/' + rel) == sha(BASE / rel), rel
        for target in TARGETS:
            if target.endswith('.so') and vm_sha(VM_BUILD + '/' + target) != baseline[target]:
                subprocess.run(['limactl', 'copy', str(saved_library),
                                'neo-builder:' + MESA], check=True)
                raise RuntimeError('Restored Mesa rebuild differed; original library restored')
            assert vm_sha(VM_BUILD + '/' + target) == baseline[target], target
        assert vm_sha(MESA) == mesa_sha
        print('BUILDER_SOURCES_AND_TEST_BINARIES_RESTORED', baseline, flush=True)
        print('BUILDER_MESA_UNCHANGED', mesa_sha, flush=True)
