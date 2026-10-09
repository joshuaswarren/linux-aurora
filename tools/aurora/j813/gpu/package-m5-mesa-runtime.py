#!/usr/bin/env python3
"""Package the isolated M5 Mesa build and Vulkan tools for its RAM root."""
from pathlib import Path
import argparse
import hashlib
import json
import os
import re
import shutil
import socket
import subprocess
import tarfile

assert socket.gethostname() == 'lima-neo-builder'
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--output', type=Path, required=True,
                    help='new package directory; existing artifacts are never overwritten')
args = parser.parse_args()
build = Path('/home/mcruz.guest/j813-mesa-runtime')
output = args.output
lib = output / 'opt/m5-mesa/lib'
bin_dir = output / 'opt/m5-mesa/bin'
sources = {
    'libvulkan_asahi.so': build / 'src/asahi/vulkan/libvulkan_asahi.so',
    'libgallium-26.1.4.so': build / 'src/gallium/targets/dri/libgallium-26.1.4.so',
    'libGLX_mesa.so.0': build / 'src/glx/libGLX_mesa.so.0.0.0',
    'libEGL_mesa.so.0': build / 'src/egl/libEGL_mesa.so.0.0.0',
    'libgbm.so.1': build / 'src/gbm/libgbm.so.1.0.0',
    'gbm/dri_gbm.so': build / 'src/gbm/backends/dri/dri_gbm.so',
    'dri/libdril_dri.so': build / 'src/gallium/targets/dril/libdril_dri.so',
}
tools = [Path('/usr/bin/vulkaninfo'), Path('/usr/bin/vkcube')]
dependencies = {}
env = dict(os.environ, LD_LIBRARY_PATH=':'.join(sorted({str(p.parent) for p in sources.values()})))
for path in [*sources.values(), *tools]:
    text = subprocess.check_output(['ldd', str(path)], text=True, env=env)
    if 'not found' in text:
        raise RuntimeError('Unresolved runtime dependency')
    for name, resolved in re.findall(r'^\s*(\S+) => (/\S+)', text, re.M):
        resolved = Path(resolved)
        if name in dependencies and dependencies[name] != resolved:
            raise RuntimeError('Conflicting runtime dependency: ' + name)
        dependencies[name] = resolved
output.mkdir(exist_ok=False)
lib.mkdir(parents=True)
bin_dir.mkdir(parents=True)
for name, path in dependencies.items():
    if name not in sources:
        shutil.copy2(path, lib / name)
for name, path in sources.items():
    (lib / name).parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(path, lib / name)
for path in tools:
    shutil.copy2(path, bin_dir / path.name)
# Strip only these deployment copies; preserve the qualified build's symbols.
for path in [p for p in lib.rglob('*') if p.is_file()] + list(bin_dir.iterdir()):
    subprocess.run(['strip', '--strip-debug', str(path)], check=True)
dri = lib / 'dri'
dri.mkdir(exist_ok=True)
# Mesa's DRI ABI is exported by DRIL, not by libgallium. Xorg also queries
# the swrast loader entry point while constructing its GLX visual list;
# GALLIUM_DRIVER=zink still selects the hardware Vulkan renderer.
for name in ['zink_dri.so', 'asahi_dri.so', 'apple_dri.so', 'swrast_dri.so']:
    (dri / name).symlink_to('libdril_dri.so')
(output / 'opt/m5-mesa/asahi_icd.json').write_text(json.dumps({
    'file_format_version': '1.0.0',
    'ICD': {'library_path': '/opt/m5-mesa/lib/libvulkan_asahi.so',
            'api_version': '1.4.0'}}, indent=2) + '\n')
manifest = {str(p.relative_to(output)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in output.rglob('*') if p.is_file() and not p.is_symlink()}
(output / 'opt/m5-mesa/SHA256SUMS').write_text(''.join(
    digest + '  /' + name + '\n' for name, digest in sorted(manifest.items())))
archive = output.with_suffix('.tar.gz')
with tarfile.open(archive, 'x:gz') as tar:
    tar.add(output / 'opt', arcname='opt')
print('M5_MESA_RUNTIME_PACKAGED', archive, archive.stat().st_size,
      hashlib.sha256(archive.read_bytes()).hexdigest(), 'files=' + str(len(manifest)))
