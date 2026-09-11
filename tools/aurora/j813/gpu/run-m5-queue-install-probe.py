#!/usr/bin/env python3
"""Run a retained mode-3..12 probe after boot-linux.py releases the M5 console."""
from pathlib import Path
import os
import argparse
import sys
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / "m1n1/proxyclient"))
from m1n1.proxy import Serial

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('mode', type=int, choices=range(3, 13), nargs='?', default=3)
parser.add_argument('--frames', type=int, choices=range(1, 65), default=1,
                    help='Require this many completed mode12 renders with output resets')
parser.add_argument('--context', type=int, choices=range(3, 64), default=3,
                    help='Mode12 application context; verify descriptor and channel IDs follow the lease')
args = parser.parse_args()
mode = args.mode
if args.frames != 1 and mode != 12:
    parser.error('--frames requires mode12')
if args.context != 3 and mode != 12:
    parser.error('--context requires mode12')
trace_mask = int(os.environ.get('M5_FW_TRACE', '0'), 0)
if trace_mask < 0 or trace_mask & ~0x203:
    raise ValueError('M5_FW_TRACE accepts only power, submission and recovery bits (0x203)')
if mode in (8, 9, 10, 11, 12) and trace_mask & 3 != 3:
    raise ValueError('Modes8..12 require trace bits0 and1 for submission evidence')
command = (
    "echo M5_QUEUE_PROBE_BEGIN; "
    "if grep -q apple,j813 /proc/device-tree/compatible "
    "&& [ $(uname -r) = 7.1.6-aurora-j813-gpu-v9 ] "
    "&& [ ! -d /sys/module/asahi ]; then "
    f"modprobe asahi m5_initdata_test={mode} m5_fw_trace={trace_mask} m5_graphics_frames={args.frames} m5_graphics_context={args.context}; echo M5_QUEUE_PROBE_RETURN=$?; "
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
required = [b"\nM5_QUEUE_PROBE_RETURN=0\r\n",
            b"M5_INIT transaction result=Ok(())", b"M5_INIT stop requests=Ok(())"]
if mode == 12:
    required.append(f'M5_GRAPHICS_CONTEXT id={args.context} roots='.encode())
    required += [b"M5_GRAPHICS installed TA/fragment queues; registration=",
                 b"M5_GRAPHICS prepared outer/inner/completion=",
                 b"M5_GRAPHICS verified native-program TA/fragment pixels=",
                 b"both queues retired; completion="]
    if b'M5_GRAPHICS firmware requested GPU restart' in captured:
        raise RuntimeError('M5 graphics triggered GPU recovery; inspect retained reports')
else:
    required.append(b"M5_QUEUE installed qid11")
if 4 <= mode <= 11:
    required.append(b"M5_QUEUE scheduler bound Some(")
if 5 <= mode <= 11:
    required.append(b"M5_CONTEXT prepared id3 roots=")
if mode in (6, 9, 10, 11):
    required.append(b"M5_COMPUTE verified 1024 exact outputs, inputs unchanged, queues retired;")
if mode == 12 and args.frames > 1:
    required.append(f'M5_GRAPHICS_FRAMES_VERIFIED count={args.frames}'.encode())
    for frame in range(1, args.frames + 1):
        required.append(f'; frame={frame}/{args.frames}'.encode())
        if frame > 1:
            required.append(f'M5_GRAPHICS reset completed output before frame={frame} untouched=65536'.encode())
if mode == 11:
    required.append(b"pages=7 triangle=true 1024 untouched outputs, no kicks")
if mode == 7:
    required.append(b"M5_COMPUTE setup retired outer=[2, 2, 2] inner=[3, 3, 3]; 1024 untouched outputs, SKSM kick unpublished")
    if b'M5_COMPUTE first SKSM kick submitted' in captured:
        raise RuntimeError('The no-kick setup unexpectedly submitted compute; retain its owners')
if mode == 8:
    required += [b'M5_KSM quiescent admission snapshot;', b'id=0x41 args=[b, 1, 2, 1]']
    if b'M5_COMPUTE firmware requested GPU restart' in captured:
        raise RuntimeError('Recovery preceded the admission snapshot; retain its owners')
if any(marker not in captured for marker in required):
    if b"M5_COMPUTE firmware requested GPU restart" in captured:
        raise RuntimeError("M5 compute triggered a GPU restart; inspect the retained reports")
    raise RuntimeError(f"M5 mode-{mode} probe was not verified; inspect the retained log")
print("M5_GRAPHICS_TRIANGLE_HARDWARE_VERIFIED" if mode == 12
      else "M5_COMPUTE_TRIANGLE_HARDWARE_VERIFIED" if mode == 11
      else "M5_KSM_ADMISSION_HARDWARE_VERIFIED" if mode == 8
      else "M5_COMPUTE_SETUP_HARDWARE_VERIFIED" if mode == 7
      else "M5_COMPUTE_HARDWARE_VERIFIED" if mode in (6, 9, 10)
      else "M5_QUEUE_INSTALL_HARDWARE_VERIFIED")
