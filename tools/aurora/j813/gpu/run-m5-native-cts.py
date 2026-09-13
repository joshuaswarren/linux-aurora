#!/usr/bin/env python3
"""Run one bounded native CTS selection, retaining logs and mapped driver proof."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('name')
p.add_argument('--case', required=True)
p.add_argument('--binary', choices=['glcts', 'deqp-gles3'], default='glcts')
p.add_argument('--seconds', type=int, default=60)
p.add_argument('--platform', choices=['surfaceless', 'x11'], default='surfaceless')
p.add_argument('--config', default='rgba8888d24s8ms0',
               help='CTS EGL configuration name; recorded verbatim in the report')
a = p.parse_args()
assert b'apple,j813\0' in Path('/proc/device-tree/compatible').read_bytes()
out = Path('/run/m5-native-cts') / a.name
out.mkdir(parents=True, exist_ok=False)
cwd = Path('/opt/m5-cts') / a.platform
driver = Path('/opt/m5-native-gallium/lib/libgallium-26.1.4.so')
command = ['/opt/m5-native-gallium/native-env', '/usr/bin/env',
           'EGL_PLATFORM=' + ('surfaceless' if a.platform == 'surfaceless' else 'x11'),
           str(cwd / a.binary), '--deqp-case=' + a.case,
           '--deqp-surface-type=pbuffer', '--deqp-surface-width=64',
           '--deqp-surface-height=64', '--deqp-gl-config-name=' + a.config,
           '--deqp-log-filename=' + str(out / 'results.qpa'),
           '--deqp-log-images=enable', '--deqp-watchdog=enable']
report = {'command': command, 'timeout_seconds': a.seconds,
          'driver_sha256': hashlib.sha256(driver.read_bytes()).hexdigest(),
          'provenance_verified': False, 'timed_out': False}
start = time.monotonic()
paths = set()
with (out / 'stdout.log').open('w') as log:
    child = subprocess.Popen(command, cwd=cwd, stdout=log, stderr=subprocess.STDOUT,
                             start_new_session=True)
    while child.poll() is None:
        try:
            maps = Path(f'/proc/{child.pid}/maps').read_text()
            paths.update(line.split()[-1] for line in maps.splitlines() if '/' in line)
            if str(driver) in paths:
                report['provenance_verified'] = True
            if any('libvulkan_asahi' in s or 'libasahi_noop' in s for s in paths):
                os.killpg(child.pid, signal.SIGKILL)
                report['provenance_verified'] = False
                report['provenance_error'] = 'Vulkan or no-op driver present'
                break
        except (FileNotFoundError, ProcessLookupError):
            pass
        if time.monotonic() - start > a.seconds:
            report['timed_out'] = True
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
            break
        time.sleep(0.01)
    report['returncode'] = child.wait()
report['elapsed_seconds'] = round(time.monotonic() - start, 3)
report['mapped_libraries'] = sorted(s for s in paths if '.so' in s)
qpa = (out / 'results.qpa').read_text(errors='replace') if (out / 'results.qpa').exists() else ''
report['cases_started'] = len(re.findall(r'^#beginTestCaseResult ', qpa, re.M))
report['results'] = dict(Counter(re.findall(r'<Result StatusCode="([^"]+)"', qpa)))
report['cases_completed'] = sum(report['results'].values())
report['renderer_strings'] = re.findall(r'^#sessionInfo renderer "([^"]+)"', qpa, re.M)
report['passed'] = (report['returncode'] == 0 and report['provenance_verified']
                    and not report['timed_out'] and report['cases_started'] > 0
                    and report['cases_started'] == report['cases_completed']
                    and report['results'] == {'Pass': report['cases_completed']}
                    and any('Apple M5 (G17G' in s for s in report['renderer_strings']))
(out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
(out / 'kernel.log').write_bytes(subprocess.check_output(['dmesg']))
print(json.dumps({k: v for k, v in report.items() if k != 'mapped_libraries'}, indent=2))
sys.exit(0 if report['passed'] else 1)
