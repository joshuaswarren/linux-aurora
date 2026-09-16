#!/usr/bin/env python3
"""Offline ABI differential. Run inside neo-builder; never alters Mesa inputs."""
import hashlib
import json
from pathlib import Path
import shlex
import subprocess

HERE = Path(__file__).resolve().parent
BUILD = Path('/home/mcruz.guest/neo/build/mesa-native-gl2-20260913')
SOURCE = Path('/home/mcruz.guest/neo/sources/mesa-native-gl2-20260913')
OUT = Path('/home/mcruz.guest/neo/build/msaa-fetch-abi-staging-fields-20260913')
OUT.mkdir(exist_ok=False)
commands = json.loads((BUILD / 'compile_commands.json').read_text())
entry = next(e for e in commands if e['file'].endswith('/test-agx3-tex-core.c'))
target = entry['output'].split('.p/')[0]
link = shlex.split(subprocess.check_output(
    ['ninja', '-C', str(BUILD), '-t', 'commands', target], text=True).splitlines()[-1])
core = SOURCE / 'src/asahi/compiler/agx3_tex_core.c'
archive = BUILD / 'src/asahi/compiler/libasahi_compiler.a'
digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
before = {str(p): digest(p) for p in [core, archive, BUILD / target]}

def compile_file(source, output):
    args = shlex.split(entry['command'])
    args[args.index('-c') + 1] = str(source)
    for flag, value in [('-o', str(output)), ('-MQ', str(output)),
                        ('-MF', str(output) + '.d')]:
        args[args.index(flag) + 1] = value
    subprocess.run(args, cwd=BUILD, check=True)

main_object = OUT / 'check.o'
compile_file(HERE / 'check.c', main_object)
results = {}
for name in ['driver', 'apple-abi-control', 'apple-abi-sampler1-control']:
    args = link.copy()
    args[args.index(entry['output'])] = str(main_object)
    exe = OUT / name
    args[args.index('-o') + 1] = str(exe)
    if name != 'driver':
        text = core.read_text()
        old = 'base[nb++] = (struct agx3_field_kv)KV(112, 4);'
        assert text.count(old) == 1
        text = text.replace(old,
            'base[nb++] = (struct agx3_field_kv)KV(83, 4);\n'
            '   base[nb++] = (struct agx3_field_kv)KV(112, 8);')
        if name == 'apple-abi-sampler1-control':
            old_sampler = '      d->sampler_value);'
            assert text.count(old_sampler) >= 1
            text = text.replace(old_sampler, '      1);', 1)
        alternate = OUT / (name + '.c')
        alternate.write_text(text)
        obj = OUT / (name + '.o')
        compile_file(alternate, obj)
        args.insert(args.index(str(main_object)) + 1, str(obj))
    subprocess.run(args, cwd=BUILD, check=True)
    results[name] = dict(line.split() for line in
        subprocess.check_output([str(exe)], text=True).splitlines())

native = '25800ca090008014000100001200'
results['native'] = native
results['apple_control_matching_texture_wait'] = [w for w, b in
    results['apple-abi-control'].items() if b == native]
results['apple_sampler1_matching_texture_wait'] = [w for w, b in
    results['apple-abi-sampler1-control'].items() if b == native]
results['driver_differing_bytes'] = [i for i, (a, b) in enumerate(zip(
    bytes.fromhex(results['driver']['0:1']), bytes.fromhex(native))) if a != b]
after = {str(p): digest(p) for p in [core, archive, BUILD / target]}
assert before == after
results['production_files_unchanged'] = before
(HERE / 'staging-fields-receipt.json').write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps(results, indent=2))
