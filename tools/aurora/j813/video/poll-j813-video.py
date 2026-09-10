"""Service host phase/arm files through the single active M5 HV controller."""
import json
import signal
import time
from m1n1.proxy import START, EXC_RET
from m1n1.hv.types import HV_EVENT

if not getattr(hv, 'j813_kernelcache_verified', False):
    raise RuntimeError('Video poller requires the verified M5 guest')
_video_original_shell = hv.run_shell
_video_original_start = hv.start
_video_requested = False
_video_poll_log = (_ave_dir / 'ave-control-events.jsonl').open('x', buffering=1)


def _video_tick(signum, frame):
    global _video_requested
    if hv._in_handler or hv.in_shell or _video_requested:
        return
    _video_requested = True
    hv._handle_sigint(signum, frame)


def _video_shell(*args, **kwargs):
    global _video_requested
    requested, _video_requested = _video_requested, False
    if not (requested and hv.exc_reason == START.HV and
            hv.exc_code == HV_EVENT.USER_INTERRUPT):
        return _video_original_shell(*args, **kwargs)
    timeout = iface.dev.timeout
    iface.dev.timeout = 30
    try:
        status = hv.j813_video_control()
        _video_poll_log.write(json.dumps(dict(
            kind='poll', host_monotonic_ns=time.monotonic_ns(),
            **status)) + '\n')
    except Exception:
        # Keep real guest faults and observer failures visible in the HV shell.
        import traceback
        traceback.print_exc()
        signal.setitimer(signal.ITIMER_REAL, 0)
        return _video_original_shell(*args, **kwargs)
    finally:
        iface.dev.timeout = timeout
    return EXC_RET.HANDLED


def _video_start(*args, **kwargs):
    signal.signal(signal.SIGALRM, _video_tick)
    signal.setitimer(signal.ITIMER_REAL, 5, 20)
    return _video_original_start(*args, **kwargs)


hv.run_shell = _video_shell
hv.start = _video_start
print('M5_VIDEO_CONTROL_POLLER_ARMED first=5 interval=20', flush=True)
