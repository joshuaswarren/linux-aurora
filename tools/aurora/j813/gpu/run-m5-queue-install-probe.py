#!/usr/bin/env python3
"""Run a retained mode-3..7 probe after boot-linux.py releases the M5 console."""
from pathlib import Path
import os
import sys
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / "m1n1/proxyclient"))
from m1n1.proxy import Serial

mode = int(sys.argv[1]) if len(sys.argv) == 2 else 3
if len(sys.argv) > 2 or mode not in (3, 4, 5, 6, 7):
    raise ValueError("Expected mode 3 (queue), 4 (scheduler), 5 (context), 6 (compute) or 7 (setup without kick)")
trace_mask = int(os.environ.get('M5_FW_TRACE', '0'), 0)
if trace_mask < 0 or trace_mask & ~0x203:
    raise ValueError('M5_FW_TRACE accepts only power, submission and recovery bits (0x203)')
command = (
    "echo M5_QUEUE_PROBE_BEGIN; "
    "if grep -q apple,j813 /proc/device-tree/compatible "
    "&& [ $(uname -r) = 7.1.6-aurora-j813-gpu-v9 ] "
    "&& [ ! -d /sys/module/asahi ]; then "
    f"modprobe asahi m5_initdata_test={mode} m5_fw_trace={trace_mask}; echo M5_QUEUE_PROBE_RETURN=$?; "
    "dmesg | tail -180; "
    "else echo M5_QUEUE_PROBE_WRONG_TARGET_OR_ALREADY_LOADED; fi; "
    "echo M5_QUEUE_PROBE_DONE\n"
)
captured = bytearray()
with Serial("/dev/cu.kis-01100000-ch-0", 115200, timeout=0.2) as port:
    port.write(command.encode())
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        chunk = port.read(65536)
        if not chunk:
            continue
        captured.extend(chunk)
        sys.stdout.buffer.write(chunk)
        sys.stdout.flush()
        if b"\nM5_QUEUE_PROBE_DONE\r\n" in captured:
            break
    else:
        raise RuntimeError("M5 probe did not return within 60 seconds; retain its owners")
required = [b"M5_QUEUE installed qid11", b"\nM5_QUEUE_PROBE_RETURN=0\r\n",
            b"M5_INIT transaction result=Ok(())", b"M5_INIT stop requests=Ok(())"]
if mode >= 4:
    required.append(b"M5_QUEUE scheduler bound Some(")
if mode >= 5:
    required.append(b"M5_CONTEXT prepared id3 roots=")
if mode == 6:
    required.append(b"M5_COMPUTE verified 1024 exact outputs, inputs unchanged, queues retired;")
if mode == 7:
    required.append(b"M5_COMPUTE setup retired outer=[2, 2, 2] inner=[3, 3, 3]; 1024 untouched outputs, SKSM kick unpublished")
    if b'M5_COMPUTE first SKSM kick submitted' in captured:
        raise RuntimeError('The no-kick setup unexpectedly submitted compute; retain its owners')
if any(marker not in captured for marker in required):
    if b"M5_COMPUTE firmware requested GPU restart" in captured:
        raise RuntimeError("M5 compute triggered a GPU restart; inspect the retained reports")
    raise RuntimeError(f"M5 mode-{mode} probe was not verified; inspect the retained log")
print("M5_COMPUTE_SETUP_HARDWARE_VERIFIED" if mode == 7
      else "M5_QUEUE_INSTALL_HARDWARE_VERIFIED")
