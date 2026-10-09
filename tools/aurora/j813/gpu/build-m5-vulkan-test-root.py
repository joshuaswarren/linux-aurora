#!/usr/bin/env python3
"""Package a small Vulkan test root from the qualified M5 userspace artifacts.

The inputs must use the same qualified AArch64 userspace ABI. The result excludes
XFCE, Zink/Gallium and LLVM; its target-side installer checks the dynamic loaders
and records a Vulkan draw before reporting readiness.
"""
import argparse
import gzip
import hashlib
from pathlib import Path
import shutil
import tarfile
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['mesa-runtime', 'desktop-root', 'driver', 'readback', 'render', 'output']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    for path in [args.mesa_runtime, args.desktop_root, args.driver, args.readback, args.render]:
        if not path.is_file():
            parser.error(f'missing input {path}')
    if args.output.exists():
        parser.error(f'refusing to overwrite {args.output}')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='m5-vulkan-root-', dir=args.output.parent) as temporary:
        stage = Path(temporary)
        with tarfile.open(args.mesa_runtime) as source:
            members = []
            for member in source:
                name = member.name.removeprefix('./')
                if not name.startswith('opt/m5-mesa/'):
                    continue
                if any(part in name for part in ['/lib/dri/', 'libLLVM', 'libgallium',
                                                  '/bin/', '/reference-']):
                    continue
                members.append(member)
            source.extractall(stage, members=members, filter='data')
        library = stage / 'opt/m5-mesa/lib/libvulkan_asahi.so'
        opener = gzip.open if args.driver.suffix == '.gz' else open
        with opener(args.driver, 'rb') as src, library.open('wb') as dst:
            shutil.copyfileobj(src, dst)
        bindir = stage / 'opt/m5-mesa/bin'
        bindir.mkdir(parents=True, exist_ok=True)
        for path, name in [(args.readback, 'm5-vulkan-readback-render'),
                           (args.render, 'm5-vulkan-render-debug')]:
            shutil.copyfile(path, bindir / name)
            (bindir / name).chmod(0o755)
        # The fixture dlopens libvulkan; ldd on the executable cannot discover it.
        with tarfile.open(args.desktop_root) as source:
            vulkan = [member for member in source if member.isfile() and
                      member.name.removeprefix('./').startswith(
                          'usr/lib/aarch64-linux-gnu/libvulkan.so.')]
            if len(vulkan) != 1:
                raise RuntimeError('Expected one qualified Vulkan loader')
            with source.extractfile(vulkan[0]) as src, (library.parent / 'libvulkan.so.1').open('wb') as dst:
                shutil.copyfileobj(src, dst)
            loader = stage / 'lib/ld-linux-aarch64.so.1'
            loader.parent.mkdir(parents=True, exist_ok=True)
            with source.extractfile('./usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1') as src, loader.open('wb') as dst:
                shutil.copyfileobj(src, dst)
            loader.chmod(0o755)
        (stage / 'lib/aarch64-linux-gnu').symlink_to('/opt/m5-mesa/lib')
        for directory in ['dev', 'proc', 'sys', 'run', 'tmp']:
            (stage / directory).mkdir(exist_ok=True)
        with args.output.open('xb') as raw, gzip.GzipFile(fileobj=raw, mode='wb', mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w|') as output:
                output.add(stage, arcname='.', recursive=True)
    print(f'{hashlib.sha256(args.output.read_bytes()).hexdigest()}  {args.output.name}')
    print(f'M5_VULKAN_FAST_RUNTIME_BYTES={args.output.stat().st_size}')


if __name__ == '__main__':
    main()
