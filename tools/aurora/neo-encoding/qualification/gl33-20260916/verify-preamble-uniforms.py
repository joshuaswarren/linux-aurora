#!/usr/bin/env python3
"""Build and validate the uniform probe on software GL, without touching the Neo."""
import hashlib
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
SOURCE = HERE / 'probes/preamble-uniforms.c'
source = SOURCE.read_text()
mutations = {
    'stale-direct': (
        'glUniform4uiv(locations[stage], counts[stage], &data[stage][draw][0][0]);',
        'glUniform4uiv(locations[stage], counts[stage], &data[stage][0][0][0]);',
        'vs-short-direct'),
    'stale-ubo': (
        'draw * stride[stage], counts[stage] * 16);',
        '0, counts[stage] * 16);',
        'vs-short-ubo'),
}
for old, _, _ in mutations.values():
    assert source.count(old) == 1, old

remote = r'''
import json, pathlib, subprocess, tempfile

def check(source, mutations):
    environment = {'PATH': '/usr/bin:/bin', 'EGL_PLATFORM': 'surfaceless',
        'LIBGL_ALWAYS_SOFTWARE': '1', 'MESA_LOADER_DRIVER_OVERRIDE': 'llvmpipe',
        'MESA_SHADER_CACHE_DISABLE': 'true', 'LP_NUM_THREADS': '2'}
    with tempfile.TemporaryDirectory(prefix='j700-uniform-probe-') as directory:
        p = pathlib.Path(directory)
        def build(name, code):
            (p/(name+'.c')).write_text(code)
            subprocess.run(['cc', '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror',
                str(p/(name+'.c')), '-o', str(p/name), '-lEGL', '-lGL'], check=True)
            return p/name
        baseline = build('baseline', source)
        result = subprocess.run([str(baseline), 'llvmpipe', 'all', '8'],
            capture_output=True, text=True, env=environment, timeout=60)
        assert result.returncode == 0, (result.returncode, result.stderr)
        assert 'PASS workloads=12 draws=6144 pixel_comparisons=98304' in result.stdout
        print(json.dumps({'variant': 'baseline', 'exit': result.returncode,
                          'stdout': result.stdout, 'stderr': result.stderr}))
        for name, (old, new, workload) in mutations.items():
            binary = build(name, source.replace(old, new))
            result = subprocess.run([str(binary), 'llvmpipe', workload, '1'],
                capture_output=True, text=True, env=environment, timeout=60)
            assert result.returncode == 1, (name, result.returncode, result.stderr)
            assert 'FAIL '+workload+' round=0 draw=1 pixel=4,0' in result.stderr
            print(json.dumps({'variant': name, 'exit': result.returncode,
                              'stdout': result.stdout, 'stderr': result.stderr}))
'''
result = subprocess.run(['limactl', 'shell', 'neo-builder', 'python3', '-'],
    input=remote + '\ncheck(' + repr(source) + ', ' + repr(mutations) + ')\n',
    capture_output=True, text=True)
(HERE/'preamble-uniforms-controls.log').write_text(
    'source_sha256 '+hashlib.sha256(source.encode()).hexdigest()+'\n' +
    result.stdout + result.stderr)
result.check_returncode()
print('Software baseline and both stale-input negative controls passed.')
print('Evidence:', HERE/'preamble-uniforms-controls.log')
