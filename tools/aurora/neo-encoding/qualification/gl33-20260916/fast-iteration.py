#!/usr/bin/env python3
"""Deploy and run one CTS iteration over the verified Neo's existing boot."""
import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import re
import shlex
import subprocess
import tarfile
import time
import xml.etree.ElementTree as ET
from cts_replay import build_replay, summarize_replay
from cts_cache import resolve_cache_policy

BASE = Path(__file__).resolve().parent
ROOT = BASE.parents[2]
REMOTE = '/home/mcruz/gl30-cts-20260913'
CONFIG = BASE.parent/'j700-plasma-keyboard-install-20260913/linux-ssh-config'
SSH = ['ssh', '-F', str(CONFIG), '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', 'neo-plasma']
SCP = ['scp', '-q', '-C', '-F', str(CONFIG), '-o', 'BatchMode=yes']
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('label')
choice = parser.add_mutually_exclusive_group(required=True)
choice.add_argument('--case')
choice.add_argument('--full', action='store_true')
choice.add_argument('--replay-failures', type=Path,
                    help='Batch failures and interrupted cases from original main-run QPAs')
choice.add_argument('--readback-probe', action='store_true',
                    help='Reject on first readback error; success requires the entire probe budget')
choice.add_argument('--sparse-depth-probe', action='store_true',
                    help='Run the staged desktop GL sample-mask/depth diagnostic')
parser.add_argument('--probe-iterations', type=int, default=12000)
parser.add_argument('--repeat', type=int, default=1,
                    help='Replay rounds with unchanged settings (diagnostic only)')
parser.add_argument('--collect-full-log', action='store_true',
                    help='Also download verbose runner stdout; it is always retained on the Neo')
parser.add_argument('--candidate', type=Path)
parser.add_argument('--trace', action='store_true')
parser.add_argument('--session', type=Path,
                    help='Recorded session-ledger entry to reproduce with --case')
parser.add_argument('--profile', default='gl30',
                    choices=['gl30', 'gl31', 'gl32', 'gl33'],
                    help='Official main profile selected by the pinned CTS runner')
parser.add_argument('--take-desktop', action='store_true')
parser.add_argument('--console-timeout', type=int, default=3600)
parser.add_argument('--shader-cache', choices=['auto', 'off', 'isolated'], default='auto',
                    help='Auto uses a build-isolated cache; shader tracing disables it')
args = parser.parse_args()
try:
    cache_policy = resolve_cache_policy(args.shader_cache, args.trace)
except ValueError as error:
    parser.error(str(error))
assert re.fullmatch(r'[a-z0-9-]+', args.label)
assert not args.trace or args.case
assert not args.trace or cache_policy == 'off', 'Shader tracing requires --shader-cache off'
assert not args.session or args.case
assert 1 <= args.repeat <= 100 and (args.replay_failures or args.repeat == 1)
assert 24 <= args.probe_iterations <= 1000000
assert not args.readback_probe or args.profile == 'gl33', 'Readback probe requests a GL 3.3 context; use --profile gl33'
assert not args.sparse_depth_probe or args.profile == 'gl33', 'Sparse-depth probe requires --profile gl33'
replay = build_replay(args.replay_failures, args.profile, args.repeat) if args.replay_failures else None
if args.case:
    # A dEQP '*' pattern runs a matching group in one session, which is how a
    # failure family is retested without one console round trip per case.
    assert re.fullmatch(r'KHR-GL3[0-3]\.[a-zA-Z0-9_.*-]+', args.case)
if args.candidate:
    assert args.candidate.is_file()
    assert re.fullmatch(r'libgallium-[a-z0-9-]+\.so', args.candidate.name)

inventory = None
if args.full:
    inventories = {'gl30': 'x11-inventory-auto', 'gl33': 'x11-gl33-inventory-auto'}
    if args.profile not in inventories:
        parser.error('No audited full-run inventory for ' + args.profile)
    inventory = BASE / 'hardware-inventory' / inventories[args.profile]
    summary = inventory / 'cts-run-summary.xml'
    if not summary.is_file() or ET.parse(summary).getroot().get('Type') != args.profile:
        parser.error('Missing or wrong-profile official inventory: ' + str(summary))

guard = open(BASE/'.fast-iteration-host.lock', 'w')
fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
# Do NOT discover this by globbing for "the only" KIS console: a J813 from
# the other track enumerates the same way, and on 2026-09-14 it took over
# /dev/cu.kis-01100000-ch-0 while the J700's own console was gone entirely.
# Driving that port would have run this campaign's commands on the wrong
# machine. The J700 is this id; if it is absent the link is down, and the
# answer is to restore it, not to use whichever console happens to exist.
device = '/dev/cu.kis-00100000-ch-0'
assert Path(device).exists(), (
    'J700 console is gone; a missing console silently skipped the candidate '
    'install on 2026-09-14 and ran the CTS against the previously installed '
    'library. Restore the link before iterating.')
owners = subprocess.run(['lsof', '-t', device], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
assert owners.returncode in (0, 1) and not owners.stdout and not owners.stderr, 'Neo serial owner exists or ownership could not be checked'

start = time.monotonic()
local = BASE/'fast-iterations'/args.label
local.mkdir(parents=True, exist_ok=False)
timing = {'steps': [], 'label': args.label}


def run(name, command, **kwargs):
    before = time.monotonic()
    result = subprocess.run(command, **kwargs)
    timing['steps'].append({'name': name, 'seconds': time.monotonic()-before,
                            'exit_code': result.returncode})
    return result


probe = """import hashlib,json
from pathlib import Path
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(bytes([0]))
print(json.dumps({'boot':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
 'mesa_sha256':hashlib.sha256(Path('/opt/neo-mesa-native-gl2-20260913/lib/libgallium-26.1.4.so').read_bytes()).hexdigest()}))
"""
result = run('identity', SSH+[shlex.join(['python3', '-c', probe])], capture_output=True, text=True, check=True)
identity = json.loads(result.stdout)
job = dict(boot=identity['boot'], label=args.label, old_sha256=identity['mesa_sha256'],
           mesa_sha256=identity['mesa_sha256'], mode='full' if args.full else 'replay' if replay else 'probe' if args.readback_probe else 'sparse-depth' if args.sparse_depth_probe else 'diagnostic',
           case=args.case, trace=args.trace, take_desktop=args.take_desktop,
           profile=args.profile, candidate=None)
job['shader_cache'] = cache_policy
if replay:
    job['replay'] = replay
if args.session:
    session = json.loads(args.session.read_text())
    assert session['kind'] == 'test' and isinstance(session['command'], str)
    job['session'] = session
files = [BASE/name for name in ('fast-iteration-worker.py', 'stop-desktop-for-cts.py',
         'install-mesa-candidate.py', 'start-x11-hardware.py', 'stop-x11-hardware.py',
         'run-official-cts.py', 'collect_asahi_devcoredumps.py', 'cts_cache.py', 'cts_faults.py', 'cts_probe.py')]
if args.readback_probe:
    source = BASE/'probes/compressed-readback-race.c'
    job.update(probe_sha256=hashlib.sha256(source.read_bytes()).hexdigest(),
               probe_iterations=args.probe_iterations)
    files.append(source)
if args.sparse_depth_probe:
    binary = BASE/'agx-sync-20260916-linux/asahi-g17p-gl-msaa-resolve'
    job['probe_binary_sha256'] = hashlib.sha256(binary.read_bytes()).hexdigest()
    files.append(binary)
if args.candidate:
    job.update(candidate=args.candidate.name,
               mesa_sha256=hashlib.sha256(args.candidate.read_bytes()).hexdigest())
    if job['mesa_sha256'] != job['old_sha256']:
        files.append(args.candidate.resolve())
jobfile = local/('fast-job-'+args.label+'.json')
jobfile.write_text(json.dumps(job, indent=2)+'\n')
files.append(jobfile)
run('upload', SCP+[str(p) for p in files]+['neo-plasma:'+REMOTE+'/'], check=True)
command = shlex.join(['python3', REMOTE+'/fast-iteration-worker.py', REMOTE+'/'+jobfile.name])
with (local/'console.log').open('wb') as log:
    console = run('hardware', [str(ROOT/'neo-update/.venv/bin/python'), str(ROOT/'neo-linux/console.py'),
                  '--device', device, '--timeout', str(args.console_timeout), command],
                  stdout=log, stderr=subprocess.STDOUT)

# A transport timeout never authorizes restarting a possibly running test.
# Fetch its terminal receipt; if absent, leave the owner/process for inspection.
remote_report = REMOTE+'/'+job['boot']+'/fast-iteration-'+args.label+'.json'
result = run('terminal_receipt', SSH+[shlex.join(['cat', remote_report])], capture_output=True, text=True)
if result.returncode:
    timing.update(terminal=False, console_exit_code=console.returncode,
                  seconds=time.monotonic()-start,
                  next_action='inspect_existing_process_do_not_restart')
    (local/'timing.json').write_text(json.dumps(timing, indent=2)+'\n')
    raise SystemExit('No terminal receipt. Inspect the existing remote process; do not restart.')
report = json.loads(result.stdout)
(local/'remote-report.json').write_text(json.dumps(report, indent=2)+'\n')
if report['complete']:
    archive = local/'results.tar.gz'
    with archive.open('wb') as data:
        excludes = [] if args.collect_full_log else ['--exclude=runner.log']
        run('collect', SSH+[shlex.join(['tar', '-C', REMOTE+'/'+job['boot'], *excludes, '-czf', '-', args.label])],
            stdout=data, check=True)
    target = local/'results'
    target.mkdir()
    with tarfile.open(archive) as tar:
        for entry in tar.getmembers():
            path = (target/entry.name).resolve()
            assert path.is_relative_to(target.resolve()) and (entry.isfile() or entry.isdir())
        tar.extractall(target)
    (local/'collection.json').write_text(json.dumps(dict(
        verbose_stdout_collected=args.collect_full_log,
        remote_full_evidence=REMOTE+'/'+job['boot']+'/'+args.label), indent=2)+'\n')
    if replay:
        receipt = json.loads((target/args.label/'receipt.json').read_text())
        analysis = summarize_replay(target/args.label, replay, receipt['replay_sessions'])
        (target/args.label/'replay-analysis.json').write_text(json.dumps(analysis, indent=2)+'\n')
        report['diagnostic_passed'] = analysis['diagnostic_passed']
        report['statuses'] = analysis['statuses']
    if args.readback_probe or args.sparse_depth_probe:
        receipt = json.loads((target/args.label/'receipt.json').read_text())
        report['diagnostic_passed'] = receipt['probe_result']['passed']
        report['probe_result'] = receipt['probe_result']
    if args.full:
        run('analyze', ['python3', str(BASE/'analyze-run.py'), str(target/args.label),
                        str(inventory)], check=True,
                        stdout=subprocess.DEVNULL)
        analysis = json.loads((target/args.label/'analysis.json').read_text())
        report['fully_accounted'] = (analysis['fully_accounted'] and
                                     analysis['complete_sessions'] == analysis['total_sessions'])
        report['statuses'] = analysis['counts']
timing.update(terminal=True, seconds=time.monotonic()-start, hardware=report)
(local/'timing.json').write_text(json.dumps(timing, indent=2)+'\n')
print(json.dumps({'artifact':str(local), 'seconds':timing['seconds'],
                 'cts_seconds':report.get('cts_seconds'), 'xorg_reused':report.get('xorg_reused'),
                 'probe_seconds':report.get('probe_seconds'), 'probe_result':report.get('probe_result'),
                 'xorg_pid':report.get('xorg_pid'), 'kernel_fault':report.get('kernel_fault'),
                 'diagnostic_passed':report.get('diagnostic_passed'), 'statuses':report.get('statuses'),
                 'fully_accounted':report.get('fully_accounted'),
                 'complete':report['complete'], 'next_action':report.get('next_action')}), flush=True)
raise SystemExit(0 if report['complete'] and not report['kernel_fault']
                 and report.get('cts_exit_code') == 0
                 and (not args.full or report.get('fully_accounted'))
                 and (not (replay or args.readback_probe or args.sparse_depth_probe) or report.get('diagnostic_passed')) else 1)
