#!/usr/bin/env python3
"""Offline causal experiment; never edits the production source or build."""
import hashlib
import json
import pathlib
import shlex
import subprocess
import time

V = pathlib.Path('/home/mcruz.guest/neo/build/mesa-native-gl2-20260913')
P = pathlib.Path(__file__).resolve().parent
OUT = pathlib.Path('/home/mcruz.guest/neo/build/robust-index-audit-20260913')
OUT.mkdir(exist_ok=False)
commands = json.loads((V / 'compile_commands.json').read_text())
receipt = {'experiment': 'Reverse only 0b99903d325 W8 load/store payload hunks in two isolated compiler objects', 'commands': []}

def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()

protected = [V / s for s in ['src/asahi/clc/asahi_clc', 'src/asahi/compiler/libasahi_compiler.a', 'src/asahi/libagx/libagx.spv', 'src/asahi/lib/libagx_shaders.c']]
receipt['production_before'] = {str(p): sha(p) for p in protected}

def run(argv, label, allowed=(0,)):
    start = time.monotonic()
    with (P / (label + '.log')).open('w') as log:
        result = subprocess.run(argv, cwd=V, stdout=log, stderr=subprocess.STDOUT)
    receipt['commands'].append({'label': label, 'argv': list(map(str, argv)), 'returncode': result.returncode, 'seconds': time.monotonic()-start})
    (P / 'receipt.json').write_text(json.dumps(receipt, indent=2)+'\n')
    print(label, result.returncode, flush=True)
    if result.returncode not in allowed:
        raise RuntimeError(label)

def compile_copy(entry, source, output, label):
    argv = shlex.split(entry['command'])
    assert argv.count(entry['file']) == 1
    argv[argv.index(entry['file'])] = str(source)
    argv[argv.index('-o')+1] = str(output)
    if '-MF' in argv:
        argv[argv.index('-MF')+1] = str(output)+'.d'
    run(argv, label)

def link_command(target):
    commands = subprocess.check_output(['ninja', '-t', 'commands', target], cwd=V, text=True)
    argv = shlex.split(commands.splitlines()[-1])
    assert argv[argv.index('-o')+1] == target
    return argv

run([str(V / 'src/asahi/lib/asahi-g17p-precompiled-fill')], 'current-golden-red', (1,))
replacements = {}
for name in ['agx_compile.c', 'agx3_pack_binary.c']:
    entry = next(x for x in commands if x['file'].endswith('/'+name))
    obj = OUT / (name+'.o')
    compile_copy(entry, P/name, obj, 'compile-'+name)
    replacements[entry['output']] = str(obj)
    receipt.setdefault('alternate_sources', {})[name] = sha(P/name)

archive = 'src/asahi/compiler/libasahi_compiler.a'
members = subprocess.check_output(['ar', 't', archive], cwd=V, text=True).splitlines()
assert set(replacements).issubset(members)
run(['ar', 'rcs', str(OUT/'libasahi_compiler.a')] + [replacements.get(x, str(V/x)) for x in members], 'isolated-archive')
argv = link_command('src/asahi/clc/asahi_clc')
argv[argv.index('-o')+1] = str(OUT/'asahi_clc')
assert argv.count(archive) == 1
argv[argv.index(archive)] = str(OUT/'libasahi_compiler.a')
run(argv, 'isolated-clc-link')
run([str(OUT/'asahi_clc'), str(V/'src/asahi/libagx/libagx.spv'), str(OUT/'libagx_shaders.h'), str(OUT/'libagx_shaders.c')], 'alternate-precompile')
entry = next(x for x in commands if x['file'] == 'src/asahi/lib/libagx_shaders.c' and 'precompiled-fill' in x['output'])
compile_copy(entry, OUT/'libagx_shaders.c', OUT/'libagx_shaders.c.o', 'alternate-table-compile')
argv = link_command('src/asahi/lib/asahi-g17p-precompiled-fill')
argv[argv.index('-o')+1] = str(OUT/'precompiled-fill')
assert argv.count(entry['output']) == 1
argv[argv.index(entry['output'])] = str(OUT/'libagx_shaders.c.o')
run(argv, 'alternate-test-link')
run([str(OUT/'precompiled-fill')], 'alternate-golden-test', (0, 1))
receipt['production_after'] = {str(p): sha(p) for p in protected}
receipt['production_unchanged'] = receipt['production_before'] == receipt['production_after']
assert receipt['production_unchanged']
receipt['alternate_table_sha256'] = sha(OUT/'libagx_shaders.c')
receipt['tables_identical'] = (OUT/'libagx_shaders.c').read_bytes() == (V/'src/asahi/lib/libagx_shaders.c').read_bytes()
receipt['w8_payload_change_explains_golden_mismatch'] = not receipt['tables_identical']
(P/'receipt.json').write_text(json.dumps(receipt, indent=2)+'\n')
print('Experiment complete; inspect the receipt for the causal verdict', flush=True)
