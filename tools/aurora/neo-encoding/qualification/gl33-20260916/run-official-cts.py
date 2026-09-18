#!/usr/bin/env python3
"""Supervise unmodified Khronos CTS binaries and preserve fault evidence."""
import errno
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import signal
import subprocess
import sys
import time
import shlex
from cts_faults import FATAL_GPU_FAULT


def diagnostic_options(session):
    if session is None:
        return ['--deqp-surface-width=64', '--deqp-surface-height=64',
                '--deqp-base-seed=1', '--deqp-screen-rotation=unspecified',
                '--deqp-watchdog=disable', '--deqp-surface-type=window',
                '--deqp-gl-config-id=1', '--deqp-gl-context-type=egl']
    assert session['kind'] == 'test'
    options = shlex.split(session['command'])
    allowed = {'deqp-surface-width', 'deqp-surface-height', 'deqp-base-seed',
               'deqp-screen-rotation', 'deqp-watchdog', 'deqp-surface-type',
               'deqp-gl-config-id', 'deqp-gl-context-type', 'deqp-gl-config-name'}
    result, seen, caselists = [], set(), []
    for option in options:
        assert option.startswith('--') and '=' in option
        key, value = option[2:].split('=', 1)
        assert key not in seen and value
        seen.add(key)
        if key == 'deqp-caselist-file':
            caselists.append(value)
            continue
        assert key in allowed
        result.append(option)
    assert caselists == ['gl_cts/data/mustpass/gl/khronos_mustpass/main/'
                         + profile + '-main.txt']
    assert allowed - {'deqp-gl-config-name'} <= seen
    return result

boot, label, mode = sys.argv[1:4]
platform = sys.argv[4] if len(sys.argv) > 4 else 'surfaceless'
assert platform in ('surfaceless', 'x11')
expected_mesa = sys.argv[5] if len(sys.argv) > 5 else '6b660621245899d0fa0ab916e82201232d32b3774006bff77aa0f98336f3e96e'
assert re.fullmatch(r'[0-9a-f]{64}', expected_mesa)
# Everything past the fixed positions is either a named token or the single
# diagnostic case name, so a token may be passed whatever the mode is.
tail_args = sys.argv[6:]
extra = [x for x in tail_args
         if x == 'trace' or x.startswith(('session=', 'profile=', 'replay=', 'probe=', 'probe-binary=', 'iterations=', 'cache='))]
cases = [x for x in tail_args if x not in extra]
assert len(cases) <= 1
case = cases[0] if cases else None
assert len(extra) <= 4 and len(extra) == len(set(extra))
# The pinned runner selects this profile's main table plus preliminary
# no-context work. It does not walk all older profiles' main tables.
profiles = [x[len('profile='):] for x in extra if x.startswith('profile=')]
assert len(profiles) <= 1
profile = profiles[0] if profiles else 'gl30'
assert profile in ('gl30', 'gl31', 'gl32', 'gl33')
trace = 'trace' in extra
cache_policies = [x[len('cache='):] for x in extra if x.startswith('cache=')]
assert len(cache_policies) <= 1
cache_policy = cache_policies[0] if cache_policies else 'off'
assert cache_policy in ('off', 'isolated')
assert not trace or cache_policy == 'off'
sessions = [json.loads(x[len('session='):]) for x in extra if x.startswith('session=')]
assert len(sessions) <= 1
session = sessions[0] if sessions else None
assert os.geteuid() == 0
assert re.fullmatch(r'[a-z0-9-]+', label)
assert mode in ('summary', 'full', 'diagnostic', 'replay', 'probe', 'sparse-depth')
assert (case is not None) == (mode == 'diagnostic')
assert not trace or mode == 'diagnostic'
assert session is None or mode == 'diagnostic'
replays = [x[len('replay='):] for x in extra if x.startswith('replay=')]
assert len(replays) == (1 if mode == 'replay' else 0)
replay = None
probes = [x[len('probe='):] for x in extra if x.startswith('probe=')]
budgets = [x[len('iterations='):] for x in extra if x.startswith('iterations=')]
assert len(probes) == len(budgets) == (1 if mode == 'probe' else 0)
probe_source_sha, probe_iterations = None, None
binary_digests = [x[len('probe-binary='):] for x in extra if x.startswith('probe-binary=')]
assert len(binary_digests) == (1 if mode == 'sparse-depth' else 0)
probe_binary_sha = binary_digests[0] if binary_digests else None
if mode == 'sparse-depth':
    assert re.fullmatch(r'[0-9a-f]{64}', probe_binary_sha)
    assert platform == 'x11' and profile == 'gl33'
if mode == 'probe':
    probe_source_sha, probe_iterations = probes[0], int(budgets[0])
    assert re.fullmatch(r'[0-9a-f]{64}', probe_source_sha)
    assert 24 <= probe_iterations <= 1000000 and platform == 'x11' and profile == 'gl33'
if replays:
    job_path = Path(replays[0])
    assert job_path == Path('/home/mcruz/gl30-cts-20260913') / ('fast-job-' + label + '.json')
    job = json.loads(job_path.read_text())
    assert job['boot'] == boot and job['label'] == label and job['mesa_sha256'] == expected_mesa
    replay = job['replay']
    assert replay['profile'] == profile and 1 <= replay['repeat'] <= 100
    assert replay['groups'] and platform == 'x11'
    for group in replay['groups']:
        assert group['cases'] and len(set(group['cases'])) == len(group['cases'])
        assert all(re.fullmatch(r'KHR-' + profile.upper() + r'\.[a-zA-Z0-9_.-]+', c)
                   for c in group['cases'])
        diagnostic_options(group['session'])
if case:
    assert platform == 'x11' and re.fullmatch(
        r'KHR-GL3[0-3]\.[a-zA-Z0-9_.*-]+', case)
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert Path('/run/neo-gl30-cts-prepared').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
assert subprocess.run(['systemctl', 'is-active', '--quiet', 'sddm']).returncode != 0
mesa = Path('/opt/neo-mesa-native-gl2-20260913')
mesa_sha = hashlib.sha256((mesa/'lib/libgallium-26.1.4.so').read_bytes()).hexdigest()
assert mesa_sha == expected_mesa
module = Path('/lib/modules')/os.uname().release/'kernel/drivers/gpu/drm/asahi/asahi.ko'
module_sha = hashlib.sha256(module.read_bytes()).hexdigest()
module_note = Path('/sys/module/asahi/notes/.note.gnu.build-id').read_bytes().hex()
cts = Path('/opt/neo-cts-opengl-4.6.8.1'+('-x11' if platform == 'x11' else ''))
assert (cts/'source-commit.txt').read_text().strip() == '067e8832315e79817ede1c4863804e440f5d1c80'
manifest = json.loads((cts/'package-manifest.json').read_text())
for name in ('cts-runner', 'glcts'):
    assert hashlib.sha256((cts/name).read_bytes()).hexdigest() == manifest[name]['sha256']
fault = FATAL_GPU_FAULT
before = subprocess.check_output(['dmesg'])
assert not fault.search(before), 'A fresh boot is required after a GPU fault'
out = Path('/home/mcruz/gl30-cts-20260913')/boot/label
out.mkdir(exist_ok=False)
user = pwd.getpwnam('mcruz')
os.chown(out, user.pw_uid, user.pw_gid)
(out/'kernel-before.log').write_bytes(before)
env = dict(
    HOME='/home/mcruz', USER='mcruz', LANG='C',
    PATH='/opt/neo-piglit-full/waffle/bin:/usr/bin:/bin',
    LD_LIBRARY_PATH=str(mesa)+'/lib:/opt/neo-mesa-native-glx-f4c0b9725bbc/lib/compat:/opt/neo-piglit-full/waffle/lib',
    LIBGL_DRIVERS_PATH=str(mesa)+'/lib/dri',
    __EGL_VENDOR_LIBRARY_FILENAMES=str(mesa)+'/share/glvnd/egl_vendor.d/50_mesa.json',
    EGL_PLATFORM='surfaceless', GALLIUM_DRIVER='asahi',
    MESA_LOADER_DRIVER_OVERRIDE='asahi',
)
from cts_cache import shader_cache_environment
cache_env = shader_cache_environment(cache_policy, mesa_sha, module_sha)
env.update(cache_env)
if cache_policy == 'isolated':
    subprocess.run(['runuser', '-u', 'mcruz', '--', 'mkdir', '-p',
                    cache_env['MESA_SHADER_CACHE_DIR']], check=True)
if platform == 'x11':
    state = json.loads((out.parent/'x11-server/state.json').read_text())
    assert state['boot'] == boot
    assert Path('/proc', str(state['pid']), 'comm').read_text().strip() == 'Xorg'
    assert str(out.parent/'x11-server/Xorg.log').encode() in Path('/proc', str(state['pid']), 'cmdline').read_bytes()
    assert Path(state['authority']).is_file()
    env.update(DISPLAY=state['display'], XAUTHORITY=state['authority'], EGL_PLATFORM='x11')
    # Let Mesa bridge the display-only DCP device to the Asahi render device.
    # Forcing the render driver onto the display FD prevents EGL initialization.
    env.pop('GALLIUM_DRIVER')
    env.pop('MESA_LOADER_DRIVER_OVERRIDE')
launch = ['runuser', '-u', 'mcruz', '--', 'env', '-i']
if mode == 'diagnostic' and trace:
    # Internal programs (geometry copy, transform feedback, blits) are refused
    # as often as API shaders, so dump them too.
    env['AGX_MESA_DEBUG'] = 'shaders,internal,refuse'
    env['ASAHI_MESA_DEBUG'] = 'resource,batch'
    # Full command/shader dumps are opt-in; ordinary focused iterations need
    # only the unchanged CTS QPA, renderer receipt and kernel fault capture.
    # The asynchronous decoder knows G17P compact VDM aliases.
    env['ASAHI_MESA_DEBUG'] += ',asynctrace'
    env['AGXDECODE_DUMP_FILE'] = str(out/'agxdecode')
    env['HK_G17P_LAUNCH_TRACE'] = '1'
launch += [key+'='+value for key, value in env.items()]
probe = subprocess.run(launch+['wflinfo', '--platform', 'x11_egl' if platform == 'x11' else 'surfaceless_egl',
                       '--api', 'gl', '--profile', 'compat', '--verbose'],
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=30)
(out/'native-renderer.log').write_bytes(probe.stdout)
assert probe.returncode == 0 and b'Apple A18 Pro' in probe.stdout
assert not fault.search(subprocess.check_output(['dmesg'])), 'GPU fault during renderer discovery'
command = launch+[str(cts/'cts-runner'), '--type='+profile,
                  '--logdir='+str(out)]
if mode == 'summary':
    command += ['--summary']
elif mode == 'diagnostic':
    command = launch+[str(cts/'glcts'), '--deqp-case='+case] + diagnostic_options(session) + [
        '--deqp-log-filename='+str(out/'diagnostic.qpa')]
elif mode == 'probe':
    source = Path('/home/mcruz/gl30-cts-20260913/compressed-readback-race.c')
    data = source.read_bytes()
    assert hashlib.sha256(data).hexdigest() == probe_source_sha
    (out/source.name).write_bytes(data)
    binary = out/'readback-probe'
    with (out/'probe-build.log').open('wb') as build_log:
        subprocess.run(['runuser', '-u', 'mcruz', '--', 'cc', '-O2', str(out/source.name),
                        '-o', str(binary), '-lEGL', '-lGL'], check=True,
                       stdout=build_log, stderr=subprocess.STDOUT, timeout=30)
    command = launch+['LOOP_N='+str(probe_iterations), 'LOOP_FAIL_FAST=1', str(binary)]
elif mode == 'sparse-depth':
    source = Path('/home/mcruz/gl30-cts-20260913/asahi-g17p-gl-msaa-resolve')
    data = source.read_bytes()
    assert hashlib.sha256(data).hexdigest() == probe_binary_sha
    binary = out/'sparse-depth-probe'
    binary.write_bytes(data)
    binary.chmod(0o755)
    command = launch+[str(binary), '--sparse-depth']
commands = [command]
replay_sessions = []
if replay:
    commands = []
    (out/'replay-plan.json').write_text(json.dumps(replay, indent=2)+'\n')
    for round_index in range(replay['repeat']):
        for group_index, group in enumerate(replay['groups']):
            stem = f'replay-r{round_index+1:03d}-g{group_index+1:03d}'
            casefile = out/(stem+'.txt')
            casefile.write_text('\n'.join(group['cases'])+'\n')
            commands.append(launch+[str(cts/'glcts'), '--deqp-caselist-file='+str(casefile)] +
                            diagnostic_options(group['session']) + [
                                '--deqp-log-filename='+str(out/(stem+'.qpa'))])
            replay_sessions.append(dict(round=round_index+1, group=group_index+1,
                                        qpa=stem+'.qpa', expected=len(group['cases'])))
(out/'command.json').write_text(json.dumps(commands if replay else command, indent=2)+'\n')
fd = os.open('/dev/kmsg', os.O_RDONLY | os.O_NONBLOCK)
os.lseek(fd, 0, os.SEEK_END)
kernel_fault = False
overruns = 0
start = time.monotonic()
exit_code = 0
completed_sessions = []
with (out/'runner.log').open('wb') as log, (out/'kernel-stream.log').open('wb') as kernel:
    for index, command in enumerate(commands):
        session_start = time.monotonic()
        proc = subprocess.Popen(command, cwd=cts, stdout=log,
                                stderr=subprocess.STDOUT, start_new_session=True)
        (out/'process.json').write_text(json.dumps({'pid':proc.pid, 'boot':boot,
                                                   'mode':mode, 'session':index})+'\n')
        print('OFFICIAL_CTS_PID='+str(proc.pid), flush=True)
        while proc.poll() is None:
            while True:
                try:
                    message = os.read(fd, 65536)
                except BlockingIOError:
                    break
                except OSError as error:
                    if error.errno != errno.EPIPE:
                        raise
                    overruns += 1
                    message = subprocess.check_output(['dmesg'])
                if not message:
                    break
                kernel.write(message)
                if fault.search(message):
                    kernel_fault = True
                    break
            # Lost kernel history invalidates hardware qualification even if
            # the remaining snapshot contains no fault. Stop immediately so
            # an hours-long run cannot finish with unverifiable evidence.
            if kernel_fault or overruns:
                os.killpg(proc.pid, signal.SIGTERM)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(proc.pid, signal.SIGKILL)
                break
            time.sleep(0.25)
        try:
            session_exit = proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            session_exit = None
        if session_exit != 0:
            exit_code = session_exit
        if replay:
            completed_sessions.append(dict(replay_sessions[index], exit_code=session_exit,
                                           elapsed_seconds=time.monotonic()-session_start))
            (out/'replay-progress.json').write_text(json.dumps(completed_sessions, indent=2)+'\n')
        # Inspect before starting another command, including faults emitted
        # just as the previous process exited. A failed case alone may proceed.
        kernel_fault |= bool(fault.search(subprocess.check_output(['dmesg'])))
        if kernel_fault or overruns or session_exit is None or session_exit < 0:
            break
os.close(fd)
after = subprocess.check_output(['dmesg'])
(out/'kernel-after.log').write_bytes(after)
kernel_fault |= bool(fault.search(after))
firmware_dumps = None
if kernel_fault:
    # Collect before reboot or driver recovery discards the firmware evidence.
    # Failure to collect must not conceal the original GPU failure/receipt.
    try:
        from collect_asahi_devcoredumps import collect
        firmware_dumps = collect(out)
    except Exception as error:
        firmware_dumps = [{'error': str(error)}]
    (out/'firmware-dumps.json').write_text(json.dumps(firmware_dumps, indent=2)+'\n')
probe_result = None
if mode == 'probe':
    text = (out/'runner.log').read_text(errors='replace')
    (out/'readback.log').write_text(text)
    count = re.search(r'loop: iterations=(\d+) requested=(\d+) fail_fast=1', text)
    result = re.search(r'loop: checked=(\d+) mismatches=(\d+) unstable_read=(\d+) second_read_ok=(\d+)', text)
    probe_result = dict(passed=False, conformance_passed=False, requested=probe_iterations)
    if count and result:
        executed, requested = map(int, count.groups())
        checked, mismatches, unstable, second_ok = map(int, result.groups())
        probe_result.update(iterations=executed, checked=checked, mismatches=mismatches,
                            unstable_read=unstable, second_read_ok=second_ok,
                            passed=exit_code == 0 and not kernel_fault and requested == probe_iterations
                            and executed == requested and checked == requested - 12
                            and mismatches == 0 and unstable == 0 and 'GL error' not in text)
    if not probe_result['passed'] and exit_code == 0:
        exit_code = 1
elif mode == 'sparse-depth':
    from cts_probe import summarize_sparse_depth
    text = (out/'runner.log').read_text(errors='replace')
    (out/'sparse-depth.log').write_text(text)
    probe_result = summarize_sparse_depth(text, exit_code, kernel_fault)
    if not probe_result['passed'] and exit_code == 0:
        exit_code = 1
receipt = dict(boot=boot, mode=mode, profile=profile, platform=platform, exit_code=exit_code,
               diagnostic_case=case,
               diagnostic_session=session,
               replay_sessions=completed_sessions if replay else None,
               command_trace=trace,
               command_trace_mode='asynctrace' if trace else None,
               shader_cache_policy=cache_policy,
               shader_cache_directory=cache_env.get('MESA_SHADER_CACHE_DIR'),
               mesa_sha256=mesa_sha, cts_commit=(cts/'source-commit.txt').read_text().strip(),
               asahi_module_sha256=module_sha, asahi_loaded_build_note=module_note,
               probe_source_sha256=probe_source_sha, probe_binary_sha256=probe_binary_sha,
               probe_result=probe_result,
               cts_runner_sha256=manifest['cts-runner']['sha256'],
               kernel_fault=kernel_fault, kernel_stream_overruns=overruns,
               firmware_dumps=firmware_dumps,
               elapsed_seconds=time.monotonic()-start,
               verdict_note='Interpret the official summary and complete run inventory; process exit alone is not conformance.')
(out/'receipt.json').write_text(json.dumps(receipt, indent=2)+'\n')
print(json.dumps(receipt), flush=True)
sys.exit(1 if kernel_fault or exit_code is None else exit_code)
