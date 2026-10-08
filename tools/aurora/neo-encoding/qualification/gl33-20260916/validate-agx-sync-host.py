#!/usr/bin/env python3
"""Build and run the merged standalone encoder tests under host UBSan."""
from pathlib import Path
import argparse
import hashlib
import json
import re
import subprocess

HERE = Path(__file__).resolve().parent
SOURCE = HERE.parents[2] / 'mesa-j700-agx-sync-20260916'
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--alu-asan', action='store_true')
args = parser.parse_args()
OUT = HERE / ('agx-sync-20260916-host-asan' if args.alu_asan else 'agx-sync-20260916-host')
OUT.mkdir(exist_ok=True)
meson = (SOURCE / 'src/asahi/compiler/meson.build').read_text()
targets = ('agx3_ioto_load_packer_test', 'agx3_ioto_uniform_program_test',
           'agx3_slot_core_test', 'agx3_subgroup_core_test')
if args.alu_asan:
    targets = ('agx3_alu_core_test',)
results = []
for target in targets:
    match = re.search(r"'" + target + r"',\s*files\((.*?)\)", meson, re.S)
    assert match, target
    sources = [SOURCE / 'src/asahi/compiler' / name
               for name in re.findall(r"'([^']+\.c)'", match[1])]
    assert sources and all(p.is_file() for p in sources)
    binary = OUT / target
    command = ['clang', '-std=c11', '-O1', '-g', '-Wall', '-Wextra', '-Werror',
               '-Wno-missing-field-initializers',
               '-Wno-error=sign-compare',
               '-fsanitize=' + ('address,undefined' if args.alu_asan else 'undefined'),
               '-fno-sanitize-recover=all',
               '-I' + str(SOURCE / 'include'), '-I' + str(SOURCE / 'src'),
               '-I' + str(SOURCE / 'src/asahi/compiler'),
               *map(str, sources), '-o', str(binary)]
    with (OUT / (target + '-build.log')).open('w') as log:
        subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True)
    with (OUT / (target + '-run.log')).open('w') as log:
        result = subprocess.run([str(binary)], stdout=log, stderr=subprocess.STDOUT)
    results.append({'target': target, 'command': command,
                    'sources': {str(p.relative_to(SOURCE)): hashlib.sha256(p.read_bytes()).hexdigest()
                                for p in sources}, 'exit_code': result.returncode})
    (OUT / 'receipt.json').write_text(json.dumps(results, indent=2) + '\n')
    print(target, 'exit', result.returncode, flush=True)
    result.check_returncode()
