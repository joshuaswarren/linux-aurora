#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Validate saved evidence for the two-boot J700 application-runtime campaign."""
import argparse
import ast
import hashlib
import json
from pathlib import Path
import re
import sentencepiece as spm


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def command_pass(text):
    require(re.findall(r'^NEO_CMD_[0-9a-f]+=(-?\d+)$', text, re.M) == ['0'],
            'missing successful serial command receipt')


def events(text, jobs):
    actual = re.findall(r'ANE work token=(\d+) (began|profiled|completed)', text)
    require(actual == [(str(i), phase) for i in range(1, jobs + 1)
                       for phase in ('began', 'profiled', 'completed')],
            'missing, extra or reordered work events')
    for opcode, length in [('203', 16), ('201', 12), ('1', 12)]:
        require(len(re.findall(f'ANE IO opcode=0x{opcode} reply={length} status=0', text)) == 1,
                'missing or repeated close/shutdown reply')


def final_state(text, jobs, hashes):
    command_pass(text)
    require('7.1.6-aurora-neo-gpu1' in text and '\n0-5\n' in text,
            'unexpected Linux runtime')
    require(not any(line.startswith('/dev/') for line in text.splitlines()),
            'unexpected disk filesystem mount')
    require('state=runtime-closed-retained error=0 started=1 allocations=5 '
            'shared_bytes=2457600 samples_passed=0 shutdown_reply=1' in text,
            'unsuccessful final state')
    for path, digest in hashes.items():
        require(digest + '  ' + path in text, 'live artifact hash mismatch: ' + path)
    events(text, jobs)
    require(f'runtime closed: jobs={jobs} events={jobs * 3}; DMA/power retained until Neo reboot' in text,
            'missing retained shutdown state')
    kernel = re.findall(r'^\[\s*([\d.]+)\] (.+)$', text, re.M)
    start = [float(t) for t, msg in kernel if msg.startswith('apple_ane_bringup: loading out-of-tree')]
    require(len(start) == 1, 'missing module-load boundary')
    diagnostics = [msg for t, msg in kernel if float(t) >= start[0] and
                   re.search(r'\bfault\b|reject|\berror\b|warning|BUG:|mismatch|timeout|timed out|Oops|panic', msg, re.I)]
    require(not diagnostics, 'new kernel diagnostics: ' + repr(diagnostics))
    ids = re.findall(r'^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$', text, re.M)
    require(len(ids) == 1, 'missing Linux boot identity')
    return ids[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', required=True, type=Path)
    parser.add_argument('--native-artifacts', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    root, native = args.artifacts, args.native_artifacts
    build = json.loads((root / 'build-v2/manifest.json').read_text())
    bundle = json.loads((root / 'linux-ram-v2/manifest.json').read_text())
    for name, expected in build['sha256'].items():
        require(sha(root / 'build-v2' / name) == expected, 'build artifact changed: ' + name)
    for name, expected in bundle['files'].items():
        require(sha(root / 'linux-ram-v2' / name) == expected['sha256'], 'boot artifact changed: ' + name)
    client_hash = sha(root / 'ane-client')
    require(client_hash == bundle['runtime_files']['opt/ane/ane-client']['sha256'], 'client changed')
    package_hash = sha(native / 'linux-case/j700-anemll-tiny.case')
    require(package_hash == build['case_sha256'], 'model package changed')
    hashes = {'/opt/ane/apple-ane-fw-test.ko': build['sha256']['apple-ane-fw-test.ko'],
              '/opt/ane/ane-client': client_hash,
              '/lib/firmware/apple/ane/j700-anemll-tiny.case': package_hash}
    qualify = (root / 'linux-qualify-v2.log').read_text()
    close = (root / 'linux-run-close.log').read_text()
    for text in [qualify, close]:
        command_pass(text)
    for marker in ['ANE_OWNER_AND_PERMISSION_PASS',
                   'ANE_INVALID_REQUESTS_PASS jobs=0 events=0',
                   'ANE_COPYBACK_ERRORS_PASS completed=2 events=6 session_usable=1',
                   'ANE_RUNTIME_QUALIFICATION_PASS jobs=256 events=768 native_values=3915776 cpu_values=262144 decoded_tokens=16',
                   'ANE_FINAL_CLOSE_PASS jobs=256 events=768 state=closed reset_required=1']:
        require(marker in qualify, 'missing qualification: ' + marker)
    events(qualify, 256)
    events(close, 1)
    require('ANE_FINAL_CLOSE_PASS jobs=1 events=3 state=closed reset_required=1' in close,
            'automatic final-fd-close failed')
    output_hash = sha(native / 'series-output-0.bin')
    require(output_hash + '  /tmp/ane-raw-output.bin' in close and
            output_hash + '  /opt/ane/runtime-fixtures/output-0.bin' in close,
            'raw application output differs from native ANE')
    boots = []
    for suffix, jobs in [('v2', 256), ('close', 1)]:
        boot = (root / f'linux-boot-{suffix}.log').read_text()
        require('TARGET_CONFIRMED J700 T8140 Mac17,5' in boot and
                'NEO_NATIVE_LINUX_SHELL_VERIFIED' in boot, 'missing target/boot evidence')
        boots.append(final_state((root / f'linux-final-{suffix}.log').read_text(), jobs, hashes))
    require(len(set(boots)) == 2, 'close test did not use a fresh Linux boot')
    tokenizer = spm.SentencePieceProcessor(model_file=str(native / 'tinyllamas/tok512.model'))
    prompts = []
    for name in ['one-day', 'little-girl']:
        reference = json.loads((root / 'prompts' / name / 'manifest.json').read_text())
        rows = re.findall(r'ANE_USER_DECODE prompt=' + name +
                          r' step=(\d+) context=(\d+) token=(\d+) expected=(\d+) max_cpu_error=([\d.]+)', qualify)
        require(len(rows) == 8, 'missing prompt jobs')
        for i, row in enumerate(rows):
            expected = reference['samples'][i]
            require(tuple(map(int, row[:4])) == (i, expected['context_length'],
                    expected['next_token'], expected['next_token']), 'prompt token/context mismatch')
            require(0 <= float(row[4]) <= 0.26, 'CPU reference error exceeds qualification bound')
        tokens = [int(row[2]) for row in rows]
        decoded = tokenizer.decode(reference['prompt_tokens'] + tokens)
        require(decoded == reference['text'], 'decoded text mismatch')
        prompts.append(dict(name=name, generated_tokens=tokens, text=decoded,
                            maximum_cpu_absolute_error=max(float(row[4]) for row in rows)))
    resident_text = (root / 'resident-after-runtime.log').read_text()
    resident_rows = re.findall(r'^INSTALLED_RESIDENT_PASS (.+)$', resident_text, re.M)
    require(len(resident_rows) == 1, 'missing final resident verification')
    resident = ast.literal_eval(resident_rows[0])
    require(resident['version'] == 'j700-resident-c1956d5-nb1' and
            resident['verified_bytes'] == 491520 and resident['relocations'] == 252 and
            resident['queries'] == 100, 'resident changed or verification incomplete')
    evidence = sorted(root.glob('*.log')) + [root / 'build-v2/manifest.json',
                root / 'linux-ram-v2/manifest.json'] + sorted(root.glob('prompts/*/manifest.json'))
    result = dict(result='pass', linux_ane_tested=True, model='stories260K through ANEMLL',
                  parameters=260032, runtime_jobs=256, runtime_ordered_events=768,
                  native_exact_values=3915776, cpu_reference_values=262144,
                  total_runtime_values_compared=4177920, native_exact_mismatches=0,
                  decoded_tokens=16, prompts=prompts, copyback_fault_completed_jobs=2,
                  implicit_close_test=dict(jobs=1, ordered_events=3, exact_native_values=16384,
                                           output_sha256=output_hash, final_state='closed'),
                  linux_boot_ids=boots, live_artifact_hashes=hashes, new_kernel_diagnostics=[],
                  resident=resident, cleanup='Neo-only SoC reset; resident verified unchanged',
                  limitations=['private ABI, one owner, fixed model and tensor shapes',
                               '256 jobs per boot, 32 positions, stateless context recomputation',
                               'macOS model compilation and native resources still required',
                               'DMA/power retained until reset; no safe unload, suspend or automatic recovery',
                               'not a throughput benchmark or general ANEMLL Linux runtime'],
                  files={str(p.relative_to(root)): sha(p) for p in evidence})
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print('ANE_RUNTIME_RESULT_PASS 256-job qualification, 1-job implicit close, 16 decoded tokens')


if __name__ == '__main__':
    main()
