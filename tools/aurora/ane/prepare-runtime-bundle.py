#!/usr/bin/env python3
"""Add the userspace client and checked fixtures to an existing ANE RAM bundle."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import runpy
import shutil
import stat


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--bundle-helper', type=Path, required=True)
    ap.add_argument('--base', type=Path, required=True)
    ap.add_argument('--client', type=Path, required=True)
    ap.add_argument('--native-reference', type=Path, required=True)
    ap.add_argument('--prompts', type=Path, required=True)
    ap.add_argument('--output', type=Path, required=True)
    args = ap.parse_args()
    helper = runpy.run_path(str(args.bundle_helper))
    helper['verify'](args.base)
    manifest = json.loads((args.base / 'manifest.json').read_text())
    case = json.loads((args.native_reference / 'linux-case/manifest.json').read_text())
    native_result = json.loads((args.native_reference / 'hardware-result.json').read_text())
    if native_result['result'] != 'pass' or not native_result['linux_ane_tested'] or case['sha256'] != native_result['case_sha256']:
        raise ValueError('native baseline is not hardware-qualified')
    client = args.client.read_bytes()
    if client[:4] != b'\x7fELF' or client[4:6] != b'\x02\x01' or int.from_bytes(client[18:20], 'little') != 183:
        raise ValueError('client must be a little-endian AArch64 ELF')
    files = {'opt/ane/ane-client': (0o755, client)}
    prefix = 'opt/ane/runtime-fixtures/'
    for i in range(3):
        inp = (args.native_reference / f'series/{i}.bin').read_bytes()
        out = (args.native_reference / f'series-output-{i}.bin').read_bytes()
        if len(inp) != 4096 or len(out) != 32768 or hashlib.sha256(out).hexdigest() != case['samples'][i]['native_output_sha256']:
            raise ValueError('native numerical fixture changed')
        files[prefix + f'input-{i}.bin'] = (0o600, inp)
        files[prefix + f'output-{i}.bin'] = (0o600, out)
    embeddings = (args.native_reference / 'series/embeddings.bin').read_bytes()
    if len(embeddings) != 65536:
        raise ValueError('wrong embedding table')
    files[prefix + 'embeddings.bin'] = (0o600, embeddings)
    prompt_records = {}
    for name in ['one-day', 'little-girl']:
        folder = args.prompts / name
        data = json.loads((folder / 'manifest.json').read_text())
        if len(data['samples']) != 8 or data['linux_ane_tested']:
            raise ValueError('unexpected CPU-only prompt reference')
        if (folder / 'embeddings.bin').read_bytes() != embeddings:
            raise ValueError('prompt used a different embedding table')
        prompt_records[name] = data
        expected_names = ['prompt-tokens.bin', 'next-tokens.bin']
        expected_names += [f'{i}{suffix}.bin' for i in range(8) for suffix in ['', '-cpu-logits']]
        for basename in expected_names:
            content = (folder / basename).read_bytes()
            expected_size = 65536 if '-cpu-logits' in basename else 32 if basename == 'next-tokens.bin' else len(data['prompt_tokens']) * 4 if basename == 'prompt-tokens.bin' else 4096
            if len(content) != expected_size:
                raise ValueError('bad prompt file size: ' + basename)
            files[prefix + name + '/' + basename] = (0o600, content)
    items = list(helper['entries'](gzip.decompress((args.base / 'initramfs.cpio.gz').read_bytes())))
    names = {name for name, _, _ in items}
    for name, (mode, content) in sorted(files.items()):
        if name in names:
            raise ValueError('would overwrite a base archive file: ' + name)
        for parent in reversed(Path(name).parents):
            directory = str(parent)
            if directory != '.' and directory not in names:
                items.append((directory, stat.S_IFDIR | 0o755, b''))
                names.add(directory)
        items.append((name, stat.S_IFREG | mode, content))
        names.add(name)
    args.output.mkdir(parents=True, exist_ok=False)
    for name in ['Image', 't8140-j700.dtb', 'config']:
        shutil.copy2(args.base / name, args.output / name)
    (args.output / 'initramfs.cpio.gz').write_bytes(gzip.compress(helper['archive'](items), compresslevel=6, mtime=0))
    manifest['purpose'] = 'Private ANE application runtime; 256 jobs and userspace error qualification'
    manifest['hardware_validated'] = False
    manifest['files'] = {name: dict(bytes=(args.output / name).stat().st_size, sha256=sha(args.output / name))
                         for name in ['Image', 't8140-j700.dtb', 'config', 'initramfs.cpio.gz']}
    manifest['runtime_files'] = {name: dict(bytes=len(content), sha256=hashlib.sha256(content).hexdigest()) for name, (_, content) in files.items()}
    manifest['prompt_references'] = prompt_records
    (args.output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print('ANE_RUNTIME_RAM_BUNDLE_PREPARED', args.output, 'client bytes', len(client))


if __name__ == '__main__':
    main()
