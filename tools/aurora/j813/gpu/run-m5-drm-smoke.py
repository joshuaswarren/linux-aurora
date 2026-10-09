#!/usr/bin/env python3
"""Load the M5 application adapter and verify its ABI on a fresh RAM boot.

This is separate from the diagnostic runner: mode14 retains live firmware,
so a diagnostic stop marker is neither required nor accepted as success.
"""
from pathlib import Path
import argparse
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / "m1n1/proxyclient"))
from m1n1.proxy import Serial

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--existing-session", action="store_true",
                    help="Repeat the non-submitting ABI test on an already loaded mode14 session")
args = parser.parse_args()

device = "/dev/cu.kis-01100000-ch-0"  # M5 RID1 only; Neo is 00100000/RID0.
owners = subprocess.run(["lsof", "-t", device], capture_output=True, text=True)
if owners.stdout.strip():
    raise RuntimeError("M5 console already has an owner; do not compete with it")

session_gate = (
    "[ -d /sys/module/asahi ] "
    "&& [ -c /dev/dri/renderD128 ] "
    "&& dmesg | grep -q 'M5_DRM registered experimental first-job render adapter'"
    if args.existing_session else "[ ! -d /sys/module/asahi ]"
)
module_command = (
    "true" if args.existing_session else "modprobe asahi m5_initdata_test=14 m5_fw_trace=0"
)
command = (
    "echo M5_DRM_SMOKE_BEGIN; "
    "if grep -q apple,j813 /proc/device-tree/compatible "
    "&& [ $(uname -r) = 7.1.6-aurora-j813-gpu-v9 ] "
    f"&& {session_gate}; then "
    f"{module_command}; "
    "rc=$?; echo M5_DRM_MODULE_RETURN=$rc; "
    "if [ $rc = 0 ]; then "
    "/usr/local/bin/m5-drm-smoke /dev/dri/renderD128; "
    "echo M5_DRM_SMOKE_RETURN=$?; fi; "
    "dmesg | tail -100; "
    "else echo M5_DRM_SMOKE_WRONG_TARGET_OR_ALREADY_LOADED; fi; "
    "echo M5_DRM_SMOKE_DONE\n"
)
captured = bytearray()
with Serial(device, 115200, timeout=0.2) as port:
    port.write(command.encode())
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        chunk = port.read(65536)
        if not chunk:
            continue
        captured.extend(chunk)
        sys.stdout.buffer.write(chunk)
        sys.stdout.flush()
        if b"\nM5_DRM_SMOKE_DONE\r\n" in captured:
            break
    else:
        raise RuntimeError("M5 DRM test did not return; preserve the live session and log")

for marker in [
    b"\nM5_DRM_MODULE_RETURN=0\r\n", b"\nM5_DRM_SMOKE_RETURN=0\r\n",
    b"M5_APPLICATION session ready; both firmware CPUs retained",
    b"M5_DRM registered experimental first-job render adapter",
    b"M5_DRM_SMOKE_VERIFIED metadata=1 lifecycles=3 mappings=9 rejected_submits=9 error_fences=18 gpu_jobs=0",
]:
    if marker not in captured:
        raise RuntimeError(f"Missing required M5 evidence: {marker!r}")
for marker in [b"Kernel panic", b"BUG:", b"M5_GRAPHICS installed",
               b"M5_INIT stop requests=", b"firmware requested GPU restart"]:
    if marker in captured:
        raise RuntimeError(f"Unexpected M5 state: {marker!r}")
print("M5_DRM_APPLICATION_ABI_VERIFIED_RENDERING_UNTESTED")
