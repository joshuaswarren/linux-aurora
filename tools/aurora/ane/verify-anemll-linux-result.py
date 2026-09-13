#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Check saved Linux serial evidence against the pinned native ANEMLL case."""
import argparse
import ast
import hashlib
import json
from pathlib import Path
import re
import sentencepiece as spm


def require(value, message):
    if not value:
        raise ValueError(message)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = args.artifacts
    case = json.loads((root / 'linux-case/manifest.json').read_text())
    build = json.loads((root / 'build-v1/manifest.json').read_text())
    preflight = (root / 'linux-preflight-v1.log').read_text()
    final = (root / 'linux-final-v1.log').read_text()
    compute = (root / 'linux-compute-v1.log').read_text()
    boot = (root / 'linux-boot-v1.log').read_text()
    require('TARGET_CONFIRMED J700 T8140 Mac17,5' in boot and
            'NEO_NATIVE_LINUX_SHELL_VERIFIED' in boot, 'missing target/boot evidence')
    require('7.1.6-aurora-neo-gpu1' in final and '\n0-5\n' in final, 'unexpected Linux runtime')
    require(not any(line.startswith('/dev/') and ' / ' in line for line in final.splitlines()), 'unexpected disk root')
    module_hash = sha(root / 'build-v1/apple-ane-fw-test.ko')
    require(module_hash == build['sha256']['apple-ane-fw-test.ko'], 'changed module')
    require(module_hash + '  /opt/ane/apple-ane-fw-test.ko' in preflight and
            module_hash + '  /opt/ane/apple-ane-fw-test.ko' in final, 'runtime module mismatch')
    require(sha(root / 'linux-case' / case['file']) == case['sha256'] == build['case_sha256'], 'changed package')
    require(case['sha256'] + '  /lib/firmware/apple/ane/' + case['file'] in final, 'runtime package mismatch')
    status = 'state=computation-complete-retained error=0 started=1 allocations=5 shared_bytes=2457600 samples_passed=3 shutdown_reply=1'
    require(status in compute and status in final, 'missing final successful state')
    prefix = r'^\[\s*[\d.]+\] apple-ane-fw-test [^:]+: '
    samples = re.findall(prefix + r'ANE sample=(\d+) token=(\d+) values=(\d+) mismatches=(\d+) sha256=([0-9a-f]{64})$', compute, re.M)
    require(len(samples) == 3, 'wrong sample count')
    for i, row in enumerate(samples):
        require(row == (str(i), str(i+1), '16384', '0', case['samples'][i]['native_output_sha256']), 'numerical mismatch')
    events = re.findall(prefix + r'ANE work token=(\d+) (began|profiled|completed)$', compute, re.M)
    require(events == [(str(i+1), phase) for i in range(3) for phase in ('began','profiled','completed')], 'bad event ordering')
    decoded = re.findall(prefix + r'ANE decoded sample=(\d+) context=(\d+) next_token=(\d+)$', compute, re.M)
    require(decoded == [(str(i), str(i+5), str(case['samples'][i]['next_token'])) for i in range(3)], 'bad token feedback')
    for opcode, length in [('203',16), ('201',12), ('1',12)]:
        require(re.search(prefix + f'ANE IO opcode=0x{opcode} reply={length} status=0$', compute, re.M), 'missing close/shutdown')
    diagnostics = []
    for line in final.splitlines():
        match = re.match(r'^\[\s*([\d.]+)\] (.+)$', line)
        if not match or float(match[1]) < 88.88:
            continue
        if re.search(r'fault|reject|error|warning|BUG:|mismatch', match[2], re.I) and 'mismatches=0' not in match[2]:
            diagnostics.append(line)
    require(not diagnostics, 'new kernel diagnostics: ' + str(diagnostics))
    tokenizer = spm.SentencePieceProcessor(model_file=str(root / 'tinyllamas/tok512.model'))
    tokens = case['prompt_tokens'] + [int(row[2]) for row in decoded]
    text = tokenizer.decode(tokens)
    require(text == case['text'], 'tokenizer receipt mismatch')
    resident_log = (root / 'resident-after-linux-v1.log').read_text()
    matches = re.findall(r'^INSTALLED_RESIDENT_PASS (.+)$', resident_log, re.M)
    require(len(matches) == 1, 'missing cleanup resident verification')
    resident = ast.literal_eval(matches[0])
    require(resident['version'] == 'j700-resident-c1956d5-nb1' and
            resident['verified_bytes'] == 491520 and resident['relocations'] == 252 and
            resident['queries'] == 100, 'unexpected resident verification')
    evidence = ['linux-boot-v1.log', 'linux-preflight-v1.log', 'linux-start-v1.log',
                'linux-compute-v1.log', 'linux-final-v1.log', 'linux-case/manifest.json',
                'build-v1/manifest.json', 'tinyllamas/tok512.model',
                'reboot-after-linux-v1.log', 'resident-after-linux-v1.log']
    receipt = dict(linux_ane_tested=True, result='pass', model='Karpathy stories260K through ANEMLL',
                   parameters=260032, transformer_layers=5, fp16_outputs=49152, exact_native_mismatches=0,
                   prompt=tokenizer.decode(case['prompt_tokens']), generated_token_ids=tokens[-3:], text=text,
                   independent_cpu_next_tokens_matched=3, native_cpu_max_absolute_logit_error=max(x['max_absolute_logit_error'] for x in case['samples']),
                   kernel_release='7.1.6-aurora-neo-gpu1', module_sha256=module_hash,
                   case_sha256=case['sha256'], ordered_work_events=len(events), shutdown_reply=True,
                   kernel_diagnostics_after_start=diagnostics,
                   cleanup='Neo-only reset; installed resident verified unchanged', resident=resident,
                   cpu_roles=['tokenization', 'embedding lookup', 'greedy selection'],
                   ane_roles=['five transformer layers', 'output projection'],
                   limitations=['macOS compilation and captured program still required', 'fixed model and prompt',
                                'three decode steps', '32 positions, full context recomputed each step',
                                'reboot-only lifetime', 'not a general ANEMLL runtime or performance benchmark'],
                   files={name: sha(root / name) for name in evidence})
    args.output.write_text(json.dumps(receipt, indent=2) + '\n')
    print('ANEMLL_LINUX_RESULT_PASS', repr(text), '49152 exact values, 9 ordered events, 3 decoded tokens')


if __name__ == '__main__':
    main()
