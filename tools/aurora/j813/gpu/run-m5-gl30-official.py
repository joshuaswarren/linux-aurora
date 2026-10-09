#!/usr/bin/env python3
"""Run the unmodified official `cts-runner --type=gl30` on the native M5 driver.

The runner spawns one glcts process per session (six on a surfaceless
platform: CTS-Configs, the no-context list, the 878-case main list at two
sizes/seeds and two rgba8888d24s8 FBO variants). Every session after the
first is a new GPU client, so this only completes on a kernel that hands the
retained M5 render context from one VM to the next. Thermal sampling, kernel
capture and driver provenance checks match run-m5-native-cts.py; per-session
QPA logs and the runner's cts-run-summary.xml are retained verbatim.
"""
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
from m5_native_cts_validation import driver_errors
from m5_kernel_capture import KernelCapture

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('name')
p.add_argument('--seconds', type=int, default=7200)
p.add_argument('--platform', choices=['surfaceless', 'x11'], default='surfaceless')
p.add_argument('--max-temperature-celsius', type=int, default=70)
a = p.parse_args()
if not 50 <= a.max_temperature_celsius <= 75:
    p.error('The bring-up temperature stop must be between 50 and 75 C')
assert b'apple,j813\0' in Path('/proc/device-tree/compatible').read_bytes()
out = Path('/run/m5-native-cts') / a.name
out.mkdir(parents=True, exist_ok=False)
logdir = out / 'official'
logdir.mkdir()
cwd = Path('/opt/m5-cts') / a.platform
driver = Path('/opt/m5-native-gallium/lib/libgallium-26.1.4.so')
command = ['/opt/m5-native-gallium/native-env', '/usr/bin/env',
           'EGL_PLATFORM=' + ('surfaceless' if a.platform == 'surfaceless' else 'x11'),
           str(cwd / 'cts-runner'), '--type=gl30', '--logdir=' + str(logdir)]
report = {'command': command, 'timeout_seconds': a.seconds,
          'driver_sha256': hashlib.sha256(driver.read_bytes()).hexdigest(),
          'cts_runner_sha256': hashlib.sha256((cwd / 'cts-runner').read_bytes()).hexdigest(),
          'glcts_sha256': hashlib.sha256((cwd / 'glcts').read_bytes()).hexdigest(),
          'kernel_release': os.uname().release,
          'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
          'provenance_verified': False, 'thermal_aborted': False, 'timed_out': False,
          'thermal_samples': [], 'children_seen': []}

def temperature_sample():
    hwmon = Path('/sys/class/hwmon/hwmon0')
    if (hwmon / 'name').read_text().strip() != 'macsmc_hwmon':
        raise RuntimeError('M5 SMC temperature provider is unavailable')
    inputs = sorted(hwmon.glob('temp*_input'))
    if len(inputs) != 43:
        raise RuntimeError('Expected all 43 qualified M5 temperature channels')
    values = {f.stem: int(f.read_text()) for f in inputs}
    if any(not 0 < value < 150000 for value in values.values()):
        raise RuntimeError('Invalid SMC temperature reading')
    sample = {'elapsed_seconds': round(time.monotonic() - start, 3),
              'maximum_millicelsius': max(values.values())}
    report['thermal_samples'].append(sample)
    if sample['maximum_millicelsius'] >= a.max_temperature_celsius * 1000:
        raise RuntimeError('Measured SMC temperature reached the run stop limit')

def children_of(pid):
    try:
        return [int(x) for x in Path(f'/proc/{pid}/task/{pid}/children').read_text().split()]
    except OSError:
        return []

start = time.monotonic()
try:
    if 'idle=nop' in Path('/proc/cmdline').read_text().split():
        raise RuntimeError('Refusing the polling-idle diagnostic kernel')
    zones = list(Path('/sys/class/thermal').glob('thermal_zone*'))
    if len(zones) != 43 or any(not (zone / 'cdev0').exists() for zone in zones):
        raise RuntimeError('M5 temperature zones are not bound to CPU cooling')
    policies = list(Path('/sys/devices/system/cpu/cpufreq').glob('policy*'))
    if not policies:
        raise RuntimeError('CPU frequency policies are unavailable')
    report['cpu_frequency_caps_khz'] = {}
    for policy in policies:
        cap = int((policy / 'scaling_max_freq').read_text())
        if (policy / 'scaling_driver').read_text().strip() != 'apple-cpufreq':
            raise RuntimeError('Unexpected CPU frequency driver')
        if cap != int((policy / 'cpuinfo_min_freq').read_text()):
            raise RuntimeError('Restore the low CPU clock caps before CTS bring-up')
        report['cpu_frequency_caps_khz'][policy.name] = cap
    temperature_sample()
except (OSError, ValueError, RuntimeError) as error:
    report.update(thermal_aborted=True, thermal_error=str(error), complete=False)
    (out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    raise SystemExit(str(error))
next_temperature_sample = time.monotonic() + 1
paths = set()
seen_children = set()
try:
    kernel_capture = KernelCapture(out / 'kernel-stream.log')
except OSError as error:
    report.update(kernel_capture={'complete': False, 'errors': [str(error)]}, complete=False)
    (out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    raise SystemExit(str(error))
with (out / 'stdout.log').open('w') as log:
    child = subprocess.Popen(command, cwd=cwd, stdout=log, stderr=subprocess.STDOUT,
                             start_new_session=True)
    while child.poll() is None:
        kernel_capture.drain()
        # The runner itself never touches the GPU; each glcts child does.
        for grandchild in children_of(child.pid):
            try:
                maps = Path(f'/proc/{grandchild}/maps').read_text()
            except OSError:
                continue
            seen_children.add(grandchild)
            mapped = {line.split()[-1] for line in maps.splitlines() if '/' in line}
            paths.update(mapped)
            if str(driver) in mapped:
                report['provenance_verified'] = True
            if any('libvulkan_asahi' in s or 'libasahi_noop' in s for s in mapped):
                os.killpg(child.pid, signal.SIGKILL)
                report['provenance_verified'] = False
                report['provenance_error'] = 'Vulkan or no-op driver present'
                break
        if time.monotonic() >= next_temperature_sample:
            try:
                temperature_sample()
            except (OSError, ValueError, RuntimeError) as error:
                report['thermal_aborted'] = True
                report['thermal_error'] = str(error)
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                break
            next_temperature_sample = time.monotonic() + 1
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
report['kernel_capture'] = kernel_capture.close()
report['children_seen'] = sorted(seen_children)
if not report['thermal_aborted']:
    try:
        temperature_sample()
    except (OSError, ValueError, RuntimeError) as error:
        report['thermal_aborted'] = True
        report['thermal_error'] = str(error)
report['elapsed_seconds'] = round(time.monotonic() - start, 3)
report['mapped_libraries'] = sorted(s for s in paths if '.so' in s)
sessions = []
totals = Counter()
renderers = set()
for qpa in sorted(logdir.glob('*.qpa')):
    text = qpa.read_text(errors='replace')
    started = re.findall(r'^#beginTestCaseResult (\S+)', text, re.M)
    results = Counter(re.findall(r'<Result StatusCode="([^"]+)"', text))
    totals.update(results)
    renderers.update(re.findall(r'^#sessionInfo renderer "([^"]+)"', text, re.M))
    sessions.append({'log': qpa.name, 'started': len(started),
                     'completed': sum(results.values()), 'results': dict(results),
                     'command_line': (re.findall(r'^#sessionInfo commandLineParameters "([^"]*)"',
                                                 text, re.M) or [''])[0]})
report['sessions'] = sessions
report['results'] = dict(totals)
report['renderer_strings'] = sorted(renderers)
summary = logdir / 'cts-run-summary.xml'
report['official_summary'] = summary.read_text(errors='replace')[:4000] if summary.exists() else None
report['driver_errors'] = driver_errors((out / 'stdout.log').read_text(errors='replace'))
report['complete'] = (report['provenance_verified'] and not report['driver_errors']
                      and report['kernel_capture']['complete']
                      and not report['timed_out'] and not report['thermal_aborted']
                      and summary.exists()
                      and all(s['started'] == s['completed'] for s in sessions)
                      and any('Apple M5 (G17G' in s for s in renderers))
(out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
(out / 'kernel.log').write_bytes(subprocess.check_output(['dmesg']))
print(json.dumps({k: v for k, v in report.items()
                  if k not in ('mapped_libraries', 'thermal_samples', 'official_summary')},
                 indent=2))
sys.exit(0 if report['complete'] else 1)
