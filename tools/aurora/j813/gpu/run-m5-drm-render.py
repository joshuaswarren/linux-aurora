#!/usr/bin/env python3
"""Submit a relocated native triangle through the M5 DRM application ABI."""
from pathlib import Path
import argparse
import subprocess
import shlex
import sys
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / "m1n1/proxyclient"))
from m1n1.proxy import Serial

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--native-va", action="store_true",
                    help="Keep captured command-data VAs in application-owned BOs")
parser.add_argument("--all-reference-pages", action="store_true",
                    help="Include all captured resource pages; requires --native-va")
parser.add_argument("--generated-fragment",
                    help="Target path to the Mesa-generated flat colour variant")
args = parser.parse_args()
if args.all_reference_pages and not args.native_va:
    parser.error("--all-reference-pages requires --native-va")
if args.generated_fragment and not (args.native_va and args.all_reference_pages):
    parser.error("--generated-fragment requires --native-va --all-reference-pages")
layout_option = " --native-va" if args.native_va else ""
if args.all_reference_pages:
    layout_option += " --all-reference-pages"
if args.generated_fragment:
    layout_option += " " + shlex.quote("--generated-fragment=" + args.generated_fragment)

device = "/dev/cu.kis-01100000-ch-0"
owners = subprocess.run(["lsof", "-t", device, device.replace('/cu.', '/tty.')],
                        capture_output=True, text=True)
if owners.stdout.strip():
    raise RuntimeError("M5 console is owned; do not compete with the active process")
command = (
    "echo M5_DRM_RENDER_BEGIN; "
    "if grep -q apple,j813 /proc/device-tree/compatible "
    "&& [ $(uname -r) = 7.1.6-aurora-j813-gpu-v9 ] "
    "&& [ ! -d /sys/module/asahi ]; then "
    "modprobe asahi m5_initdata_test=14 m5_fw_trace=512; "
    "rc=$?; echo M5_DRM_MODULE_RETURN=$rc; "
    "if [ $rc = 0 ]; then "
    "/usr/local/bin/m5-drm-render /lib/firmware/asahi/m5-25g76-graphics.bin "
    f"/dev/dri/renderD128 /tmp/m5-drm-render.rgba{layout_option}; "
    "echo M5_DRM_APPLICATION_RENDER_RETURN=$?; "
    "sha256sum /tmp/m5-drm-render.rgba; fi; "
    "dmesg | tail -160; "
    "else echo M5_DRM_RENDER_WRONG_TARGET_OR_ALREADY_LOADED; fi; "
    "echo M5_DRM_RENDER_DONE\n"
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
        if b"\nM5_DRM_RENDER_DONE\r\n" in captured:
            break
    else:
        raise RuntimeError("M5 application draw did not return; retain its VM and firmware owners")
for marker in [
    b"\nM5_DRM_MODULE_RETURN=0\r\n",
    b"\nM5_DRM_APPLICATION_RENDER_RETURN=0\r\n",
    b"M5_APPLICATION session ready; both firmware CPUs retained",
    b"M5_DRM registered experimental first-job render adapter",
    b"M5_DRM_RELOCATION_VERIFIED ppp=14 states=1 draws=1 bytes=160",
    (f"M5_DRM_RENDER_REFERENCE_PAGES selected={334 if args.all_reference_pages else 203} "
     f"captured=334 all={int(args.all_reference_pages)}").encode(),
    b"M5_DRM_RENDER_PIXELS foreground=18432 background=47104 unexpected=0 wrong=0",
    (f"M5_DRM_RENDER_VERIFIED native_program=1 relocated_application_data="
     f"{0 if args.native_va else 1} mesa_vulkan=0").encode(),
]:
    if marker not in captured:
        raise RuntimeError(f"Missing application-render evidence: {marker!r}")
for marker in [b"Kernel panic", b"BUG:", b"firmware requested GPU restart",
               b"M5_APPLICATION submission failed"]:
    if marker in captured:
        raise RuntimeError(f"Unexpected M5 failure: {marker!r}")
if args.generated_fragment:
    if b"M5_DRM_MESA_FRAGMENT_INSTALLED bytes=60 expected_rgb=191,64,128" not in captured:
        raise RuntimeError("Missing generated fragment installation evidence")
    print("M5_DRM_MESA_FRAGMENT_TRIANGLE_HARDWARE_VERIFIED")
print("M5_DRM_APPLICATION_TRIANGLE_HARDWARE_VERIFIED")
