#!/usr/bin/env python3
"""Run one official desktop OpenGL 3.0 CTS session on the native M5 driver.

The session argument vectors are exactly the ones `cts-runner --type=gl30`
generates for a surfaceless platform (one default configuration): the
CTS-Configs metadata session, the no-context KHR list, the 878-case main list
at 64x64/seed1 and 113x47/seed2, and the two rgba8888d24s8 FBO variants
(seed3). Only the log filename is added. Nothing filters the case lists.

One session is one glcts process. The current M5 kernel path retains a single
render VM per boot, so a fresh boot is needed between sessions until that
limitation is lifted. Thermal sampling, kernel capture and driver provenance
checks match run-m5-native-cts.py. Pass/fail counts are reported as data; the
exit status only says whether the session ran to completion and was captured
without driver errors, thermal aborts or missing cases.
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
from m5_native_cts_validation import driver_errors, invalid_cases
from m5_kernel_capture import KernelCapture

MAIN = 'gl_cts/data/mustpass/gl/khronos_mustpass/main/gl30-main.txt'
KHR = 'gl_cts/data/mustpass/gl/khronos_mustpass_noctx/main/gl30-khr-main.txt'
COMMON = ['--deqp-screen-rotation=unspecified']
SESSIONS = {
    'configs': ['--deqp-case=CTS-Configs.*', '--deqp-log-images=disable',
                '--deqp-log-shader-sources=disable'],
    'khr-main': ['--deqp-caselist-file=' + KHR] + COMMON + [
        '--deqp-surface-width=64', '--deqp-surface-height=64',
        '--deqp-watchdog=disable', '--deqp-base-seed=1'],
    'main-64': ['--deqp-caselist-file=' + MAIN] + COMMON + [
        '--deqp-surface-width=64', '--deqp-surface-height=64',
        '--deqp-watchdog=disable', '--deqp-base-seed=1'],
    'main-113x47': ['--deqp-caselist-file=' + MAIN] + COMMON + [
        '--deqp-surface-width=113', '--deqp-surface-height=47',
        '--deqp-watchdog=disable', '--deqp-base-seed=2'],
    'fbo-w64': ['--deqp-caselist-file=' + MAIN] + COMMON + [
        '--deqp-surface-width=64', '--deqp-surface-height=-1',
        '--deqp-watchdog=disable', '--deqp-base-seed=3',
        '--deqp-gl-config-name=rgba8888d24s8', '--deqp-surface-type=fbo'],
    'fbo-h64': ['--deqp-caselist-file=' + MAIN] + COMMON + [
        '--deqp-surface-width=-1', '--deqp-surface-height=64',
        '--deqp-watchdog=disable', '--deqp-base-seed=3',
        '--deqp-gl-config-name=rgba8888d24s8', '--deqp-surface-type=fbo'],
}

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('name')
p.add_argument('--session', choices=sorted(SESSIONS), required=True)
p.add_argument('--seconds', type=int, default=3600)
p.add_argument('--platform', choices=['surfaceless', 'x11'], default='surfaceless')
p.add_argument('--max-temperature-celsius', type=int, default=70,
               help='Stop the run at this measured SMC temperature (50..75 C)')
p.add_argument('--config-name', default=None,
               help='Deviation from the official window sessions: pin the CTS '
                    'configuration (e.g. rgba8888d0s0ms0) instead of the platform '
                    'default; recorded verbatim in the report')
a = p.parse_args()
if not 50 <= a.max_temperature_celsius <= 75:
    p.error('The bring-up temperature stop must be between 50 and 75 C')
assert b'apple,j813\0' in Path('/proc/device-tree/compatible').read_bytes()
out = Path('/run/m5-native-cts') / a.name
out.mkdir(parents=True, exist_ok=False)
cwd = Path('/opt/m5-cts') / a.platform
driver = Path('/opt/m5-native-gallium/lib/libgallium-26.1.4.so')
session_args = list(SESSIONS[a.session])
if a.platform == 'surfaceless' and not any(s.startswith('--deqp-surface-type=') for s in session_args):
    # The runner's window sessions cannot run on the surfaceless platform
    # ("does not support --deqp-surface-type=window"); a pbuffer of the same
    # size is the headless equivalent. FBO sessions keep their own type.
    session_args.append('--deqp-surface-type=pbuffer')
if a.config_name:
    if any(s.startswith('--deqp-gl-config-name=') for s in session_args):
        p.error('This session already pins its configuration')
    session_args.append('--deqp-gl-config-name=' + a.config_name)
expected_cases = []
for arg in session_args:
    if arg.startswith('--deqp-caselist-file='):
        listing = cwd / arg.split('=', 1)[1]
        expected_cases = [c for c in listing.read_text().splitlines() if c.strip()]
        (out / 'caselist.sha256').write_text(
            hashlib.sha256(listing.read_bytes()).hexdigest() + '  ' + str(listing) + '\n')
command = ['/opt/m5-native-gallium/native-env', '/usr/bin/env',
           'EGL_PLATFORM=' + ('surfaceless' if a.platform == 'surfaceless' else 'x11'),
           str(cwd / 'glcts')] + session_args + [
           '--deqp-log-filename=' + str(out / 'results.qpa')]
report = {'command': command, 'session': a.session, 'timeout_seconds': a.seconds,
          'expected_case_count': len(expected_cases),
          'driver_sha256': hashlib.sha256(driver.read_bytes()).hexdigest(),
          'glcts_sha256': hashlib.sha256((cwd / 'glcts').read_bytes()).hexdigest(),
          'kernel_release': os.uname().release,
          'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
          'provenance_verified': False, 'thermal_aborted': False, 'timed_out': False,
          'thermal_samples': []}

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
    report.update(thermal_aborted=True, thermal_error=str(error), complete=False)
    (out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    raise SystemExit(str(error))
next_temperature_sample = time.monotonic() + 1
paths = set()
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
started_names = re.findall(r'^#beginTestCaseResult (\S+)', qpa, re.M)
report['cases_started'] = len(started_names)
report['missing_cases'] = sorted(set(expected_cases) - set(started_names))
report['unexpected_cases'] = (sorted(set(started_names) - set(expected_cases))
                              if expected_cases else [])
report['results'] = dict(Counter(re.findall(r'<Result StatusCode="([^"]+)"', qpa)))
report['cases_completed'] = sum(report['results'].values())
# The case that began but never logged a result (a crash or kill ends the log).
ended_count = len(re.findall(r'^#endTestCaseResult', qpa, re.M))
report['incomplete_case'] = (started_names[-1]
                             if started_names and ended_count < len(started_names) else None)
report['renderer_strings'] = re.findall(r'^#sessionInfo renderer "([^"]+)"', qpa, re.M)
report['version_strings'] = re.findall(r'^#sessionInfo version "([^"]+)"', qpa, re.M)
report['driver_errors'] = driver_errors((out / 'stdout.log').read_text(errors='replace'))
report['invalid_cases'] = sorted(invalid_cases(report['driver_errors'], started_names))
if not report['kernel_capture']['complete']:
    report['invalid_cases'] = sorted(set(started_names))
report['complete'] = (report['provenance_verified'] and not report['driver_errors']
                      and report['kernel_capture']['complete']
                      and not report['timed_out'] and not report['thermal_aborted']
                      and report['cases_started'] > 0
                      and report['cases_started'] == report['cases_completed']
                      and len(started_names) == len(set(started_names))
                      and not report['missing_cases'] and not report['unexpected_cases']
                      and any('Apple M5 (G17G' in s for s in report['renderer_strings']))
(out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
(out / 'kernel.log').write_bytes(subprocess.check_output(['dmesg']))
print(json.dumps({k: v for k, v in report.items()
                  if k not in ('mapped_libraries', 'thermal_samples')}, indent=2))
sys.exit(0 if report['complete'] else 1)
