# SPDX-License-Identifier: MIT
"""Bounded M5 GPU mailbox/power observation, loaded before macOS starts.

No Neo init-data decoder, no speculative device accesses and no nested proxy
requests from asynchronous callbacks. Raw observations establish the M5 ABI.
"""
import json
import os
from pathlib import Path
import re
import runpy
import sys
import time

from m1n1.hv.types import TraceMode
from m1n1.trace.asc import BaseASCTracer
from m1n1.utils import irange

if int(hv.u.adt['/chosen'].chip_id) != 0x8142 or hv.u.adt.model != 'Mac17,3':
    raise RuntimeError('M5 GPU boot tracer requires J813/T8142')
if hv.started:
    raise RuntimeError('GPU startup trace must be installed before guest entry')
native_fw_trace_mask = int(os.environ.get('M5_GPU_NATIVE_FW_TRACE_MASK', '0'), 0)
if (native_fw_trace_mask < 0 or native_fw_trace_mask & ~0x203
        or native_fw_trace_mask and os.environ.get('M5_GPU_CAPTURE_INIT') != '1'):
    raise ValueError('Native firmware tracing requires init capture and bits within 0x203')

output = Path(os.environ['M5_GPU_TRACE_DIR'])
stream = (output / 'gpu-events.jsonl').open('x', buffering=1)
limit = 100000
sequence = 0
failed = False


def emit(kind, **fields):
    global sequence, failed
    if failed or sequence >= limit:
        return
    try:
        sequence += 1
        stream.write(json.dumps(dict(sequence=sequence, monotonic_ns=time.monotonic_ns(),
                                     kind=kind, **fields), sort_keys=True) + '\n')
        if sequence == limit:
            stream.write(json.dumps(dict(kind='TRACE_LIMIT', limit=limit)) + '\n')
    except Exception:
        failed = True


class M5GpuRoleTracer(BaseASCTracer):
    DEFAULT_MODE = TraceMode.ASYNC

    def log(self, message, show_cpu=True):
        emit('ASC_LOG', role=self.ident, message=message)

    def handle_msg(self, direction, low, high):
        emit('MAILBOX', role=self.ident, direction='tx' if int(direction) else 'rx',
             endpoint=int(high.EP), word0=hex(int(low.value)), word1=hex(int(high.value)))
        return True

    def evt_rw(self, event, regmap=None, prefix=None):
        try:
            emit('ASC_MMIO', role=self.ident, cpu=int(event.flags.CPU), pc=hex(event.pc),
                 write=bool(event.flags.WRITE), width=1 << int(event.flags.WIDTH),
                 offset=hex(int(event.addr) - int(self.dev.get_reg(0)[0])),
                 value=hex(event.data))
            return super().evt_rw(event, regmap=regmap, prefix=prefix)
        except Exception as error:
            emit('TRACE_ERROR', role=self.ident, error=repr(error))


roles = []
for name, base in [('gfx-asc', 0x582600000), ('gfx1-asc', 0x582e00000)]:
    path = '/arm-io/' + name
    if hv.adt[path].get_reg(0) != (base, 0x88000):
        raise RuntimeError('M5 ASC wrapper geometry changed')
    role = M5GpuRoleTracer(hv, path, verbose=0)
    role.start()
    roles.append(role)
    emit('TRACE_ARMED', role=name, base=hex(base))


def power_access(event):
    emit('GPU_POWER', address=hex(event.addr), write=bool(event.flags.WRITE),
         width=1 << int(event.flags.WIDTH), value=hex(event.data),
         cpu=int(event.flags.CPU), pc=hex(event.pc))


for name, base in [('GPX', 0x580e80000), ('GFX', 0x580e80110)]:
    hv.add_tracer(irange(base, 4), 'M5_GPU_' + name, TraceMode.ASYNC,
                  read=power_access, write=power_access)
    emit('POWER_TRACE_ARMED', domain=name, base=hex(base))


def vector_access(event):
    emit('ASC_VBAR', address=hex(event.addr), write=bool(event.flags.WRITE),
         width=1 << int(event.flags.WIDTH), value=hex(event.data),
         cpu=int(event.flags.CPU), pc=hex(event.pc))


for name, base in [('gfx-asc', 0x582050000), ('gfx1-asc', 0x582850000)]:
    if hv.adt['/arm-io/' + name].get_reg(1) != (base, 8):
        raise RuntimeError('M5 ASC vector geometry changed')
    hv.add_tracer(irange(base, 8), 'M5_GPU_VBAR_' + name, TraceMode.ASYNC,
                  read=vector_access, write=vector_access)
    emit('VECTOR_TRACE_ARMED', role=name, base=hex(base))


host_gpu_write_tracers = []
host_gpu_write_count = 0
if os.environ.get('M5_GPU_TRACE_HOST_WRITES') == '1':
    # Observe writes issued by the native AP driver, not GPU firmware CPU
    # accesses. The existing ASC observers own the two mailbox apertures.
    # No callback issues a proxy request or changes the hardware write.
    if hv.adt['/arm-io/sgx'].get_reg(0) != (0x580000000, 0x4000000):
        raise RuntimeError('M5 GPU aperture differs from the measured target')

    def host_gpu_write(event):
        global host_gpu_write_count
        if host_gpu_write_count >= 20000:
            return
        host_gpu_write_count += 1
        emit('HOST_GPU_WRITE', address=hex(event.addr),
             width=1 << int(event.flags.WIDTH), value=hex(event.data),
             cpu=int(event.flags.CPU), pc=hex(event.pc))
        if host_gpu_write_count == 20000:
            emit('HOST_GPU_WRITE_LIMIT', limit=20000)

    for ordinal, (start, end) in enumerate([
            (0x580000000, 0x582600000),
            (0x582688000, 0x582e00000),
            (0x582e88000, 0x584000000)]):
        name = 'M5_HOST_GPU_WRITE_' + str(ordinal)
        window = irange(start, end - start)
        hv.add_tracer(window, name, TraceMode.ASYNC, write=host_gpu_write)
        host_gpu_write_tracers.append((window, name))
    emit('HOST_GPU_WRITE_TRACE_ARMED', max_records=20000,
         excludes='GFX and GFX1 ASC mailbox apertures')


if os.environ.get('M5_GPU_CAPTURE_INIT') == '1':
    # Only INBOX0 temporarily becomes synchronous. Its payload write occurs
    # before the INBOX1 publication, so the firmware cannot consume this
    # initialization descriptor while it is being captured. General mailbox
    # observation remains asynchronous throughout the boot.
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from m5_gpu_capture import M5GPUCapture
    recorded_roots = re.findall(r'\buat_global_root_pa=(0x[0-9a-f]+)',
                               (output / 'console.log').read_text())
    if len(set(recorded_roots)) != 1:
        raise RuntimeError('M5 init capture requires the current SPTM seed receipt')
    init_capture = M5GPUCapture(hv.iface.readmem,
        (int(hv.u.adt['/chosen'].dram_base), int(hv.u.adt['/chosen'].dram_size)),
        int(recorded_roots[0], 16))
    init_seen = set()
    startup_capture = None
    startup_init_word = None
    startup_hits = 0
    if os.environ.get('M5_GPU_CAPTURE_STARTUP') == '1':
        from m5_startup_capture import M5StartupCapture
        sgx = hv.u.adt['/arm-io/sgx']
        startup_capture = M5StartupCapture(hv.iface.readmem,
            (int(hv.u.adt['/chosen'].dram_base), int(hv.u.adt['/chosen'].dram_size)),
            int(recorded_roots[0], 16), (int(sgx.gfx_data_base), int(sgx.gfx_data_size)))

    def init_callback(role, ident):
        def record(event):
            global startup_init_word, startup_hits
            word = int(event.data)
            if role == 'gfx-asc' and startup_capture is not None and role in init_seen:
                startup_hits += 1
                try:
                    if (word >> 48) & 0xff in (0x83, 0x84):
                        if hv.ctx is None:
                            raise RuntimeError('Startup capture must execute synchronously')
                        manifest = startup_capture.capture_startup(startup_init_word, word,
                            output / 'startup' / ('publication-%02d' % startup_capture.events))
                        emit('STARTUP_CAPTURE', ordinal=manifest['ordinal'],
                             state=manifest.get('firmware_state'), error=manifest.get('error'))
                except Exception as error:
                    emit('STARTUP_CAPTURE_FAILED', error=repr(error))
                    hv.clear_tracers(ident)
                finally:
                    if startup_hits >= 512 or startup_capture.events >= startup_capture.MAX_EVENTS:
                        hv.clear_tracers(ident)
                return
            if (word >> 48) & 0xff != 0x81 or role in init_seen:
                return
            init_seen.add(role)
            try:
                if hv.ctx is None:
                    raise RuntimeError('Initialization capture must execute synchronously')
                emit('INIT_CAPTURE_BEGIN', role=role, word0=hex(word))
                manifest = init_capture.capture(role, word, output / 'preinit' / role,
                                                'before-INBOX1-publication')
                if role == 'gfx-asc':
                    startup_init_word = word
                    if native_fw_trace_mask:
                        # INBOX1 has not published this root to firmware yet.
                        # Use the same guarded host-input writer and restoration
                        # receipt as the paused-owner diagnostic. No ring entry,
                        # firmware DATA, or mailbox payload is changed.
                        runpy.run_path(str(Path(__file__).with_name('trace-m5-native-fw.py')),
                            init_globals=dict(hv=hv, m5_native_trace_action='enable',
                                              m5_native_trace_mask=native_fw_trace_mask))
                        emit('NATIVE_FW_TRACE_INPUT', role=role,
                             mask=hex(native_fw_trace_mask),
                             phase='before-INBOX1-publication',
                             firmware_adoption_verified=False)
                emit('INIT_CAPTURE_COMPLETE', role=role, bytes=manifest['total_bytes'],
                     objects=len(manifest['objects']))
            except Exception as error:
                emit('INIT_CAPTURE_FAILED', role=role, error=repr(error))
            finally:
                # This is a synchronous handler: removing this override is
                # safe. The existing asynchronous ASC tracer stays installed.
                if role != 'gfx-asc' or startup_capture is None or startup_init_word is None:
                    hv.clear_tracers(ident)
        return record

    for role, base in [('gfx-asc', 0x582600000), ('gfx1-asc', 0x582e00000)]:
        ident = 'M5_GPU_INIT_' + role
        hv.add_tracer(irange(base + 0x8800, 8), ident, TraceMode.SYNC,
                      write=init_callback(role, ident))
        emit('INIT_CAPTURE_ARMED', role=role)

hv.m5_gpu_roles = roles
print('M5_GPU_TRACE_ARMED: both ASC roles, VBARs and GPX/GFX; raw events, no firmware ABI override')
