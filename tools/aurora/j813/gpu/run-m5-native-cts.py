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
from m5_native_cts_validation import driver_errors, invalid_cases
from m5_kernel_capture import KernelCapture

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('name')
selection = p.add_mutually_exclusive_group(required=True)
selection.add_argument('--case')
selection.add_argument('--caselist', type=Path,
                       help='Plain list of exact CTS case names; require every result')
p.add_argument('--binary', choices=['glcts', 'deqp-gles3'], default='glcts')
p.add_argument('--seconds', type=int, default=60)
p.add_argument('--platform', choices=['surfaceless', 'x11'], default='surfaceless')
p.add_argument('--config', default='rgba8888d24s8ms0',
               help='CTS EGL configuration name; recorded verbatim in the report')
p.add_argument('--base-seed', type=int, default=0)
p.add_argument('--max-temperature-celsius', type=int, default=70,
               help='Stop the run at this measured SMC temperature (50..75 C)')
a = p.parse_args()
if not 50 <= a.max_temperature_celsius <= 75:
    p.error('The bring-up temperature stop must be between 50 and 75 C')
expected_cases = []
if a.caselist:
    expected_cases = a.caselist.read_text().splitlines()
    if (not expected_cases or len(set(expected_cases)) != len(expected_cases)
            or any(not re.fullmatch(r'(?:dEQP-GLES3|KHR-GLES3)\.[A-Za-z0-9_.-]+', c)
                   for c in expected_cases)):
        p.error('--caselist requires unique, nonempty, exact GLES3 case names')
assert b'apple,j813\0' in Path('/proc/device-tree/compatible').read_bytes()
out = Path('/run/m5-native-cts') / a.name
out.mkdir(parents=True, exist_ok=False)
cwd = Path('/opt/m5-cts') / a.platform
driver = Path('/opt/m5-native-gallium/lib/libgallium-26.1.4.so')
if expected_cases:
    (out / 'caselist.txt').write_text('\n'.join(expected_cases) + '\n')
    case_option = '--deqp-caselist-file=' + str(out / 'caselist.txt')
else:
    case_option = '--deqp-case=' + a.case
command = ['/opt/m5-native-gallium/native-env', '/usr/bin/env',
           'EGL_PLATFORM=' + ('surfaceless' if a.platform == 'surfaceless' else 'x11'),
           str(cwd / a.binary), case_option,
           '--deqp-base-seed=' + str(a.base_seed),
           '--deqp-surface-type=pbuffer', '--deqp-surface-width=64',
           '--deqp-surface-height=64', '--deqp-gl-config-name=' + a.config,
           '--deqp-log-filename=' + str(out / 'results.qpa'),
           '--deqp-log-images=enable', '--deqp-watchdog=enable']
report = {'command': command, 'timeout_seconds': a.seconds,
          'expected_cases': expected_cases,
          'driver_sha256': hashlib.sha256(driver.read_bytes()).hexdigest(),
          'provenance_verified': False, 'timed_out': False}
report['thermal_stop_celsius'] = a.max_temperature_celsius
report['thermal_samples'] = []
report['thermal_aborted'] = False

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
    report.update(thermal_aborted=True, thermal_error=str(error), passed=False)
    (out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    raise SystemExit(str(error))
next_temperature_sample = time.monotonic() + 1
paths = set()
# Open before the child can submit work; preserve record sequence numbers so
# a wrapping printk ring cannot silently produce incomplete qualification.
try:
    kernel_capture = KernelCapture(out / 'kernel-stream.log')
except OSError as error:
    report.update(kernel_capture={'complete': False, 'errors': [str(error)]}, passed=False)
    (out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    raise SystemExit(str(error))
with (out / 'stdout.log').open('w') as log:
    child = subprocess.Popen(command, cwd=cwd, stdout=log, stderr=subprocess.STDOUT,
                             start_new_session=True)
    while child.poll() is None:
        kernel_capture.drain()
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
if not report['thermal_aborted']:
    try:
        temperature_sample()
    except (OSError, ValueError, RuntimeError) as error:
        report['thermal_aborted'] = True
        report['thermal_error'] = str(error)
report['elapsed_seconds'] = round(time.monotonic() - start, 3)
report['mapped_libraries'] = sorted(s for s in paths if '.so' in s)
qpa = (out / 'results.qpa').read_text(errors='replace') if (out / 'results.qpa').exists() else ''
report['cases_started'] = len(re.findall(r'^#beginTestCaseResult ', qpa, re.M))
started_names = re.findall(r'^#beginTestCaseResult (\S+)', qpa, re.M)
report['missing_cases'] = sorted(set(expected_cases) - set(started_names))
report['unexpected_cases'] = (sorted(set(started_names) - set(expected_cases))
                              if expected_cases else [])
report['results'] = dict(Counter(re.findall(r'<Result StatusCode="([^"]+)"', qpa)))
report['cases_completed'] = sum(report['results'].values())
report['renderer_strings'] = re.findall(r'^#sessionInfo renderer "([^"]+)"', qpa, re.M)
report['driver_errors'] = driver_errors((out / 'stdout.log').read_text(errors='replace'))
report['invalid_cases'] = sorted(invalid_cases(report['driver_errors'], started_names))
if not report['kernel_capture']['complete']:
    report['invalid_cases'] = sorted(set(started_names))
report['passed'] = (report['returncode'] == 0 and report['provenance_verified']
                    and not report['driver_errors']
                    and report['kernel_capture']['complete']
                    and not report['timed_out'] and not report['thermal_aborted']
                    and report['cases_started'] > 0
                    and report['cases_started'] == report['cases_completed']
                    and len(started_names) == len(set(started_names))
                    and not report['missing_cases'] and not report['unexpected_cases']
                    and report['results'] == {'Pass': report['cases_completed']}
                    and any('Apple M5 (G17G' in s for s in report['renderer_strings']))
(out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
(out / 'kernel.log').write_bytes(subprocess.check_output(['dmesg']))
print(json.dumps({k: v for k, v in report.items() if k != 'mapped_libraries'}, indent=2))
sys.exit(0 if report['passed'] else 1)
