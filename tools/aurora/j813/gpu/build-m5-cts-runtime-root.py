#!/usr/bin/env python3
"""Derive a smaller headless CTS runtime from the verified desktop archive.

Retain Python, SSH, command-line diagnostics and system shared libraries.
Mesa and unchanged CTS remain separate, hash-verified payloads. The source
desktop root is never modified; this archive is for surfaceless testing.
"""
import argparse
import gzip
import hashlib
from pathlib import Path
import tarfile

SOURCE_SHA256 = 'ada4e031949b4985bc8ee9abeac64faf3a28178a59f9ba7a1e0b624f72e202bc'
COMMANDS = set('bash cat chmod chown chroot cp date df dmesg du env false find grep '
               'free gzip head id install kill ln ls mkdir mount mv nice pgrep '
               'pkill ps python3 python3.14 readlink rm rmdir scp sed sha256sum '
               'sh dash sleep sort ssh ssh-keygen stat tail tar tee timeout touch '
               'tr true umount uname uniq wc which xargs'.split())


def keep(name, member):
    name = name.removeprefix('./').rstrip('/')
    if member.isdir() or name in ('bin', 'sbin', 'lib', 'lib64'):
        return True
    if name.startswith('etc/'):
        return True
    if name.startswith(('usr/bin/', 'usr/sbin/')):
        return name.rsplit('/', 1)[-1] in COMMANDS | {'sshd', 'unix_chkpwd'}
    if name.startswith(('usr/lib/python3.14/', 'usr/lib/locale/',
                        'usr/lib/cargo/bin/coreutils/',
                        'usr/lib/openssh/',
                        'usr/libexec/openssh/')):
        return True
    if name in ('usr/lib/ld-linux-aarch64.so.1', 'lib/ld-linux-aarch64.so.1'):
        return True
    if name.startswith(('usr/lib/aarch64-linux-gnu/', 'lib/aarch64-linux-gnu/')):
        # Native Mesa is installed under /opt with its own dependencies.
        # These are the desktop distribution's unused GPU driver binaries.
        base = name.rsplit('/', 1)[-1]
        if base.startswith(('libLLVM', 'libgallium')) or '/dri/' in name:
            return False
        return '.so' in base
    return name == 'usr/lib/os-release'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    with args.source.open('rb') as f:
        if hashlib.file_digest(f, 'sha256').hexdigest() != SOURCE_SHA256:
            parser.error('Desktop source hash mismatch')
    if args.output.exists():
        parser.error('Output already exists; preserve the existing runtime')
    count = size = 0
    # Pin the gzip header so this verified source produces a stable digest.
    with (args.output.open('xb') as output,
          gzip.GzipFile(filename='', mode='wb', fileobj=output, mtime=0) as compressed,
          tarfile.open(args.source, 'r|gz') as src,
          tarfile.open(fileobj=compressed, mode='w|') as dst):
        for member in src:
            if keep(member.name, member):
                dst.addfile(member, src.extractfile(member) if member.isfile() else None)
                count += 1
                size += member.size
    with args.output.open('rb') as f:
        digest = hashlib.file_digest(f, 'sha256').hexdigest()
    print(f'{digest}  {args.output.name}')
    print(f'{count} entries; {size} unpacked bytes; {args.output.stat().st_size} archive bytes')


if __name__ == '__main__':
    main()
