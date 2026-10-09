#!/usr/bin/env python3
"""Read-only Neo CTS observer; cache only completed, unchanged QPA files locally.

This status counter is not the final strict XML accounting performed by
analyze-run.py. A failed observation never restarts or stops the campaign.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import subprocess
import time

HERE = Path(__file__).resolve().parent
BOOT = '9f1434bf-725b-4ff1-ae51-babd4df25cb4'
CAMPAIGN = 'cache-identity-full-gl33-20260915'
CONFIG = HERE.parent / 'j700-plasma-keyboard-install-20260913/linux-ssh-config'
CACHE = HERE / 'full-gl33-progress-cache.json'
SNAPSHOT = HERE / 'full-gl33-progress-latest.json'

REMOTE = r'''
from pathlib import Path
import collections, json, re, subprocess

def inspect(boot, campaign, cache):
    current_boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    if current_boot != boot:
        raise RuntimeError('Target boot changed; this is not the active campaign boot')
    p = Path('/home/mcruz/gl30-cts-20260913') / boot / campaign
    out = {'boot': boot, 'receipt_exists': (p/'receipt.json').exists()}
    main = collections.Counter()
    other = collections.Counter()
    files = []
    failures = []
    next_cache = {}
    reused = scanned = read_bytes = 0
    def identity(q):
        s = q.stat()
        # Strings preserve nanoseconds if a consumer round-trips through JS.
        return [str(x) for x in (s.st_dev, s.st_ino, s.st_size,
                                s.st_mtime_ns, s.st_ctime_ns)]
    for q in sorted(p.glob('*.qpa')):
        before = identity(q)
        saved = cache.get(q.name)
        if saved and saved['identity'] == before and saved['result']['complete']:
            result = saved['result']
            reused += 1
        else:
            data = q.read_bytes()
            read_bytes += len(data)
            scanned += 1
            s = data.decode(errors='replace')
            counts = collections.Counter(re.findall(r'StatusCode="([^"\n]+)"', s))
            bad = []
            if any(status not in ('Pass', 'NotSupported') for status in counts):
                for block in re.split(r'^#beginTestCaseResult ', s, flags=re.M)[1:]:
                    m = re.search(r'StatusCode="([^"\n]+)"', block)
                    if m and m[1] not in ('Pass', 'NotSupported'):
                        bad.append([block.splitlines()[0], m[1]])
            result = {'name': q.name, 'counts': dict(counts),
                      'complete': '#endSession' in s, 'failures': bad}
        after = identity(q)
        if result['complete'] and before == after:
            next_cache[q.name] = {'identity': after, 'result': result}
        (main if q.name.startswith('config-gl33-main-') else other).update(result['counts'])
        failures.extend(result['failures'])
        files.append({k: v for k, v in result.items() if k != 'failures'})
    process = json.loads((p/'process.json').read_text())
    out.update(main=dict(main), other=dict(other), files=files, failures=failures,
               process=process, cache=next_cache,
               observer={'reused_files': reused, 'scanned_files': scanned,
                         'qpa_bytes_read': read_bytes})
    out['live_process'] = subprocess.run(
        ['ps', '-p', str(process['pid']), '-o', 'pid,etime,stat,args'],
        capture_output=True, text=True).stdout
    kernel = p/'kernel-stream.log'
    out['kernel_fault_audit'] = scan_kernel_faults([kernel])
    out['fault_lines'] = [sample['message'] for sample in out['kernel_fault_audit']['samples']]
    if out['receipt_exists']:
        out['receipt'] = json.loads((p/'receipt.json').read_text())
    print(json.dumps(out))
'''


def write_json(path, value):
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--boot', default=BOOT)
    parser.add_argument('--campaign', default=CAMPAIGN)
    parser.add_argument('--no-cache', action='store_true',
                        help='rescan every QPA, then refresh the local cache')
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9a-f-]{36}', args.boot):
        parser.error('boot must be a boot UUID')
    if not re.fullmatch(r'[a-z0-9-]+', args.campaign):
        parser.error('campaign must contain only lowercase letters, digits and hyphens')
    cache_path = CACHE
    snapshot_path = SNAPSHOT
    if (args.boot, args.campaign) != (BOOT, CAMPAIGN):
        prefix = HERE / (args.campaign + '-' + args.boot)
        cache_path = prefix.with_name(prefix.name + '-progress-cache.json')
        snapshot_path = prefix.with_name(prefix.name + '-progress-latest.json')
    cache = {}
    if cache_path.exists() and not args.no_cache:
        saved = json.loads(cache_path.read_text())
        if saved.get('schema') == 1 and saved.get('boot') == args.boot and saved.get('campaign') == args.campaign:
            cache = saved['files']
    started = time.monotonic()
    detector_source = (HERE/'cts_faults.py').read_text()
    result = subprocess.run([
        'ssh', '-F', str(CONFIG), '-o', 'ControlMaster=no',
        '-o', 'ControlPath=none', '-o', 'ConnectTimeout=5',
        'neo-plasma', 'python3', '-'],
        input=detector_source + '\n' + REMOTE + '\ninspect(' + repr(args.boot) + ', ' + repr(args.campaign) + ', ' + repr(cache) + ')\n',
        capture_output=True, text=True, timeout=45, check=True)
    out = json.loads(result.stdout)
    out['observed_at'] = datetime.now(timezone.utc).isoformat()
    out['kernel_fault_audit']['detector_sha256'] = hashlib.sha256(detector_source.encode()).hexdigest()
    out['observer']['wall_seconds'] = round(time.monotonic() - started, 3)
    write_json(cache_path, {'schema': 1, 'boot': args.boot, 'campaign': args.campaign,
                        'files': out.pop('cache')})
    write_json(snapshot_path, out)
    completed = sum(f['complete'] for f in out['files']
                    if f['name'].startswith('config-gl33-main-'))
    print(json.dumps({
        'observed_at': out['observed_at'],
        'main': out['main'], 'other': out['other'], 'completed': completed,
        'active': [f for f in out['files'] if not f['complete']],
        'scope': out['process'].get('mode'),
        'completion_percent': (round(100 * sum(out['main'].values()) / 494300, 3)
                               if out['process'].get('mode') == 'full' else None),
        'receipt_exists': out['receipt_exists'],
        'live_process': '\n'.join(' '.join(line.split()[:3]) for line in out['live_process'].splitlines()),
        'failures': out['failures'], 'fault_lines': out['fault_lines'],
        'kernel_fault_audit': out['kernel_fault_audit'],
        'observer': out['observer']}))


if __name__ == '__main__':
    main()
