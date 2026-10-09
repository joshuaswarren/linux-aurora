#!/usr/bin/env python3
"""Build a logging-only cache probe, then restore the exact normal builder."""
import difflib
import hashlib
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
SOURCE = HERE.parents[2] / 'mesa-j700/src/gallium/drivers/asahi'
VM_SOURCE = '/home/mcruz.guest/neo/sources/mesa-native-gl2-20260913/src/gallium/drivers/asahi'
VM_BUILD = '/home/mcruz.guest/neo/build/mesa-native-gl2-20260913'
TARGET = 'src/gallium/targets/dri/libgallium-26.1.4.so'
NORMAL = '2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2'
NAMES = ['agx_state.c', 'agx_disk_cache.c']

def vm(*args, **kwargs):
    return subprocess.run(['limactl', 'shell', 'neo-builder', *args], check=True, **kwargs)

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def vm_sha(path):
    return vm('sha256sum', path, capture_output=True, text=True).stdout.split()[0]

def replace(text, old, new):
    assert text.count(old) == 1, (old, text.count(old))
    return text.replace(old, new)

def copy(path, name):
    subprocess.run(['limactl', 'copy', str(path), 'neo-builder:' + VM_SOURCE + '/' + name], check=True)
    vm('touch', VM_SOURCE + '/' + name)

def build(log):
    with (HERE / log).open('wb') as output:
        vm('ninja', '-C', VM_BUILD, TARGET, stdout=output, stderr=subprocess.STDOUT)

for name in NAMES:
    assert vm_sha(VM_SOURCE + '/' + name) == sha(SOURCE / name), name
assert vm_sha(VM_BUILD + '/' + TARGET) == NORMAL
overlay = HERE / 'probes/cache-metadata-overlay'
overlay.mkdir(exist_ok=False)
candidate = HERE / 'candidates/libgallium-cache-metadata-probe.so'
assert not candidate.exists()

state = (SOURCE / 'agx_state.c').read_text()
state = replace(state, '      if (resource_pair) {\n         if (!cs->g17p_texture_resources ||',
    '      if (resource_pair) {\n'
    '         fprintf(stderr, "CACHE_META dispatch stage=%u texflag=%u storage=%u tex=%u img=%u base=%llx samplers=%llx borders=%u size=%u\\n",\n'
    '                 cs->stage, cs->g17p_texture_resources, cs->g17p_storage_write,\n'
    '                 cs->so->info.nr_bindful_textures, cs->so->info.nr_bindful_images,\n'
    '                 (unsigned long long)batch->stage_uniforms[stage].texture_base,\n'
    '                 (unsigned long long)batch->samplers[stage],\n'
    '                 batch->ctx->stage[stage].custom_borders, cs->b.info.binary_size);\n'
    '         if (!cs->g17p_texture_resources ||')
cache = (SOURCE / 'agx_disk_cache.c').read_text()
fmt = '   fprintf(stderr, "CACHE_META %s stage=%u texflag=%u storage=%u size=%u push=%u\\n", "%s", binary->stage, binary->g17p_texture_resources, binary->g17p_storage_write, binary->b.info.binary_size, binary->push_range_count);\n'
cache = replace(cache, '   blob_write_uint8(blob, binary->g17p_texture_resources);',
    fmt.replace('"%s",', '"write",') + '   blob_write_uint8(blob, binary->g17p_texture_resources);')
cache = replace(cache, '   if (is_root && uncompiled->type == MESA_SHADER_GEOMETRY) {',
    fmt.replace('"%s",', '"read",') + '   if (is_root && uncompiled->type == MESA_SHADER_GEOMETRY) {')
for name, text in zip(NAMES, [state, cache]):
    (overlay / name).write_text(text)
    (overlay / (name + '.diff')).write_text(''.join(difflib.unified_diff(
        (SOURCE / name).read_text().splitlines(True), text.splitlines(True),
        fromfile='normal/' + name, tofile='diagnostic/' + name)))
try:
    for name in NAMES:
        copy(overlay / name, name)
    build('cache-metadata-probe-build.log')
    subprocess.run(['limactl', 'copy', 'neo-builder:' + VM_BUILD + '/' + TARGET, str(candidate)], check=True)
    print('PROBE_BUILT', sha(candidate), flush=True)
finally:
    for name in NAMES:
        copy(SOURCE / name, name)
    build('cache-metadata-probe-restore-build.log')
    for name in NAMES:
        assert vm_sha(VM_SOURCE + '/' + name) == sha(SOURCE / name)
    assert vm_sha(VM_BUILD + '/' + TARGET) == NORMAL
    print('NORMAL_BUILDER_RESTORED', NORMAL, flush=True)
