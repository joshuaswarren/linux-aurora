#!/usr/bin/env python3
"""One fresh-boot hardware diagnostic, never a CTS conformance run."""
import errno
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import threading
import time

BASE = Path('/home/mcruz/gl30-cts-20260913')
sys.path.insert(0, str(BASE))
from cts_faults import FATAL_GPU_FAULT

MESA = '362a359bb06f3a1432720bab738f60dd2095f6188cced7d27bcfd8a2f2c0cc56'
MODULE = 'fc3c7367555041557df6b593e5f3ed98bcbd309c09aaa5472cab9f9b32279d51'
BOOT = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
OUT = BASE / 'event-wrap-qualification-20260916'
done = threading.Event()
fault = threading.Event()
overrun = threading.Event()
stream_error = []

def capture_kernel(fd):
    try:
        with (OUT / 'kernel-stream.log').open('xb') as log:
            while not done.is_set():
                try:
                    data = os.read(fd, 1024 * 1024)
                except BlockingIOError:
                    done.wait(0.02)
                    continue
                except OSError as error:
                    if error.errno == errno.EPIPE:
                        overrun.set()
                        continue
                    raise
                log.write(data)
                log.flush()
                if FATAL_GPU_FAULT.search(data):
                    fault.set()
    except BaseException as error:
        stream_error.append(str(error))
        fault.set()
    finally:
        os.close(fd)

def step(name, command, seconds=180):
    print('STEP ' + name, flush=True)
    # The existing Wi-Fi script may print network identifiers. Do not retain
    # its output in qualification artifacts.
    output_path = Path(os.devnull) if name == 'wifi' else OUT / (name + '.log')
    with output_path.open('wb' if name == 'wifi' else 'xb') as log:
        proc = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                stdin=subprocess.DEVNULL, start_new_session=True)
        deadline = time.monotonic() + seconds
        while proc.poll() is None:
            if fault.is_set() or overrun.is_set() or time.monotonic() >= deadline:
                os.killpg(proc.pid, signal.SIGTERM)
                try: proc.wait(5)
                except subprocess.TimeoutExpired:
                    os.killpg(proc.pid, signal.SIGKILL)
                    proc.wait()
                raise RuntimeError(name + ': kernel fault, log loss, or deadline exceeded')
            time.sleep(0.1)
        if proc.returncode:
            raise RuntimeError(name + ': exit ' + str(proc.returncode))

receipt = dict(boot=BOOT, production_commit='378b14ac9d89',
               diagnostic_commit='cd6595e2a555', module_sha256=MODULE,
               mesa_sha256=MESA, cts_conformance=False, hardware_test_pass=False)
thread = None
try:
    assert os.geteuid() == 0
    assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
    pending = OUT / 'pending-boot'
    assert pending.read_text().strip() != BOOT, 'requires a fresh qualified boot'
    pending.rename(OUT / 'attempted-from-boot')
    # Consume this one-shot registration before testing. It cannot repeat on
    # another boot, regardless of the result of this attempt.
    subprocess.run(['systemctl', 'disable', 'neo-event-wrap-qualification.service'],
                   check=True, stdout=subprocess.DEVNULL)
    thread = threading.Thread(target=capture_kernel,
        args=(os.open('/dev/kmsg', os.O_RDONLY | os.O_NONBLOCK),), daemon=True)
    thread.start()
    step('wifi', ['bash', '/opt/neo/plasma-wifi-20260911/bringup-wifi-first.sh'])
    step('prepare', ['python3', str(BASE / 'prepare-clean-boot.py'), BOOT, MESA, MODULE])
    step('dcp', ['bash', str(BASE / 'restore-dcp-cts.sh'), BOOT,
                 'gl30-cts-event-wrap-20260916'], 360)
    step('xorg', ['python3', str(BASE / 'start-x11-hardware.py'), BOOT])
    mesa = '/opt/neo-mesa-native-gl2-20260913'
    step('pixels', ['runuser', '-u', 'mcruz', '--', 'env', '-i',
        'PATH=/usr/bin:/bin', 'HOME=/home/mcruz',
        'LD_LIBRARY_PATH='+mesa+'/lib:/opt/neo-mesa-native-glx-f4c0b9725bbc/lib/compat',
        'LIBGL_DRIVERS_PATH='+mesa+'/lib/dri',
        '__EGL_VENDOR_LIBRARY_FILENAMES='+mesa+'/share/glvnd/egl_vendor.d/50_mesa.json',
        'EGL_PLATFORM=x11', 'DISPLAY=:0', 'XAUTHORITY=/run/neo-gl30-cts.Xauthority',
        str(BASE / 'probes/render-event-wrap')], 90)
    assert 'PIXELS_PASS draws=768 pixels=4078848 first_read_only=1' in (OUT / 'pixels.log').read_text()
    # Drain pending kernel records, then require exact completion witnesses on
    # both sides of zero for one physical queue. A successful pixel process
    # alone does not establish that the test crossed the intended boundary.
    time.sleep(0.2)
    data = (OUT / 'kernel-stream.log').read_bytes()
    matches = re.findall(rb'G17P test event-wrap verified qid=(\d+) ordinal=(\d+) stamp=(0x[0-9a-f]+)', data)
    by_queue = {}
    for qid, ordinal, stamp in matches:
        by_queue.setdefault(int(qid), {})[int(ordinal)] = int(stamp, 16)
    receipt['completion_witnesses'] = by_queue
    assert any(all(values.get(i) == (((i + 1 + 0xffff00) << 8) & 0xffffffff)
                   for i in range(252, 259)) for values in by_queue.values()), 'missing wrap witnesses'
    assert not fault.is_set() and not overrun.is_set()
    assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == BOOT
    receipt['hardware_test_pass'] = True
except BaseException as error:
    receipt['error'] = repr(error)
finally:
    done.set()
    if thread:
        thread.join(5)
        if thread.is_alive(): stream_error.append('kernel reader did not stop')
    after = subprocess.check_output(['dmesg', '--raw'])
    (OUT / 'kernel-after.log').write_bytes(after)
    receipt['kernel_fault'] = fault.is_set() or bool(FATAL_GPU_FAULT.search(after))
    receipt['kernel_log_overrun'] = overrun.is_set()
    receipt['kernel_stream_errors'] = stream_error
    if receipt['kernel_fault'] or receipt['kernel_log_overrun'] or stream_error:
        receipt['hardware_test_pass'] = False
    (OUT / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt), flush=True)
sys.exit(0 if receipt['hardware_test_pass'] else 1)
