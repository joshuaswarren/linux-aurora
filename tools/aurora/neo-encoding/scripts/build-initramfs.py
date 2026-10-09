#!/usr/bin/env python3
"""Build a small RAM root with the exact kernel module and Vulkan artifacts."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

if subprocess.check_output(['hostname'], text=True).strip() != 'lima-neo-builder':
    raise SystemExit('Run inside neo-builder')
parser = argparse.ArgumentParser()
parser.add_argument('--staging-tag', default='')
args = parser.parse_args()
if args.staging_tag and not re.fullmatch(r'[A-Za-z0-9_-]+', args.staging_tag):
    raise SystemExit('Invalid staging tag')
shared = Path(__file__).resolve().parents[1]
build = Path.home() / 'neo/build'
release = (build / 'linux/include/config/kernel.release').read_text().strip()
root = build / ('initramfs-' + release + ('-' + args.staging_tag if args.staging_tag else ''))
if root.exists():
    raise SystemExit(f'Preserving existing root: {root}; use a new staging name for a new bundle')
for directory in ['bin','sbin','dev','proc','sys','tmp','run','root','etc','usr/bin','usr/sbin','opt/neo/bin']:
    (root / directory).mkdir(parents=True, exist_ok=True)

def copy(src, dest):
    target = root / str(dest).lstrip('/')
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, target, follow_symlinks=True)
    return target

def executable(src, dest, strip=False):
    target = copy(src, dest)
    if strip:
        subprocess.run(['strip','--strip-debug',str(target)], check=True)
    result = subprocess.run(['ldd',str(src)], capture_output=True, text=True)
    if 'not found' in result.stdout:
        raise SystemExit(f'Missing dependency for {src}: {result.stdout}')
    if result.returncode and 'not a dynamic executable' not in result.stderr:
        raise SystemExit(f'ldd failed for {src}: {result.stderr}')
    for line in result.stdout.splitlines():
        match = re.search(r'(?:=>\s+)?(/\S+)\s+\(',line)
        if match:
            dependency = match[1]
            copy(dependency, dependency)
    return target

executable('/bin/busybox','/bin/busybox')
for applet in subprocess.check_output(['/bin/busybox','--list'],text=True).splitlines():
    if applet != 'busybox':
        (root / 'bin' / applet).symlink_to('busybox')
executable('/usr/bin/kmod','/sbin/kmod')
(root/'sbin/modprobe').symlink_to('kmod')
(root/'sbin/modinfo').symlink_to('kmod')
# Use full kmod ahead of BusyBox's module applets.
(root/'bin/modprobe').unlink()
(root/'bin/modprobe').symlink_to('/sbin/modprobe')
(root/'bin/modinfo').unlink(missing_ok=True)
(root/'bin/modinfo').symlink_to('/sbin/modinfo')
executable('/usr/bin/vulkaninfo','/usr/bin/vulkaninfo')
# These tools load Vulkan with dlopen, so ldd alone does not discover it.
executable('/usr/lib/aarch64-linux-gnu/libvulkan.so.1',
           '/usr/lib/aarch64-linux-gnu/libvulkan.so.1')

module_root = build / 'modules'
module_dir = module_root / 'lib/modules' / release
dependencies = subprocess.check_output(['modprobe','--show-depends','-d',str(module_root),'-S',release,'asahi'],text=True)
modules = []
for line in dependencies.splitlines():
    if line.startswith('insmod '):
        source = Path(line.split()[1])
        relative = source.relative_to(module_root)
        copy(source, relative)
        modules.append(str(source.relative_to(module_dir)))
for name in ['modules.builtin','modules.builtin.modinfo','modules.builtin.ranges']:
    source = module_dir / name
    if source.exists():
        copy(source, Path('lib/modules')/release/name)
(root/'lib/modules'/release/'modules.order').write_text('\n'.join(modules)+'\n')
subprocess.run(['depmod','-b',str(root),release],check=True)

mesa_install = build/'mesa-install/opt/neo-mesa'
executable(mesa_install/'lib/libvulkan_asahi.so','/opt/neo-mesa/lib/libvulkan_asahi.so',strip=True)
copy(mesa_install/'share/vulkan/icd.d/asahi_icd.aarch64.json','/opt/neo-mesa/share/vulkan/icd.d/asahi_icd.aarch64.json')
for name in ['asahi-vulkan-g17p-enumerate','asahi-submit-g17p-add3','asahi-submit-g17p-render','g17p-offscreen-triangle',
             'g17p-vulkan-compute','g17p-vulkan-compute-count']:
    executable(build/'mesa/src/asahi/vulkan'/name,'/opt/neo/bin/'+name,strip=True)
copy(shared/'init','/init').chmod(0o755)
copy(shared/'artifacts/source-manifest.json','/etc/neo-source-manifest.json')
(root/'etc/os-release').write_text('NAME="Aurora Neo RAM bring-up"\nID=aurora-neo-ram\n')

archive = shared/'artifacts/linux/initramfs.cpio.gz'
env = os.environ.copy()
env['LC_ALL']='C'
with archive.open('wb') as output:
    subprocess.run(['bash','-o','pipefail','-c',"find . -print0 | sort -z | cpio --null -o --format=newc --owner=0:0 | gzip -9n"], cwd=root,env=env,stdout=output,check=True)
copy_symbols = shared/'artifacts/linux/vmlinux'
shutil.copy2(build/'linux/vmlinux', copy_symbols)
manifest = {'kernel_release': release, 'files': {}}
for path in sorted((shared/'artifacts/linux').iterdir()):
    if path.is_file() and path.name != 'manifest.json':
        digest=hashlib.sha256()
        with path.open('rb') as stream:
            for chunk in iter(lambda:stream.read(1024*1024),b''):
                digest.update(chunk)
        manifest['files'][path.name]={'bytes':path.stat().st_size,'sha256':digest.hexdigest()}
(shared/'artifacts/linux/manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
print('NEO_INITRAMFS_BUILD_COMPLETE', release, 'archive_bytes='+str(archive.stat().st_size))
