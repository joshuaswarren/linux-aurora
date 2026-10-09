#!/usr/bin/env python3
"""Read a bounded firmware crash buffer from the stopped M5 Linux probe."""
import base64
import json
from pathlib import Path
import sys
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / "m1n1/proxyclient"))
from m1n1.proxy import Serial

inventory, address_text, destination = sys.argv[1:]
address = int(address_text, 0)
properties = json.loads(Path(inventory).read_text())["nodes"]["/arm-io/sgx"]["properties"]
start, size = properties["gfx-data-base"], properties["gfx-data-size"]
length = 0x8000
if address & 0xfff or not start <= address <= start + size - length:
    raise ValueError("Crash buffer must fit the measured reserved GFX DATA region")
command = (
    "stty -echo; if grep -q apple,j813 /proc/device-tree/compatible "
    "&& [ -d /sys/module/asahi ]; then echo M5_CRASH_DATA_BEGIN; "
    f"dd if=/dev/mem bs=4096 skip={address // 4096} count=8 2>/tmp/m5-crash-dd-error | base64; "
    "echo M5_CRASH_DATA_END; cat /tmp/m5-crash-dd-error; fi; stty echo\n"
)
captured = bytearray()
with Serial("/dev/cu.kis-01100000-ch-0", 115200, timeout=0.2) as port:
    port.write(command.encode())
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        captured.extend(port.read(65536))
        if b"\nM5_CRASH_DATA_END\r\n" in captured:
            break
    else:
        raise RuntimeError("Crash read timed out; leave the target unchanged")
text = bytes(captured).replace(b"\r", b"")
body = text.split(b"\nM5_CRASH_DATA_BEGIN\n", 1)[1].split(b"\nM5_CRASH_DATA_END\n", 1)[0]
raw = base64.b64decode(b"".join(body.split()), validate=True)
if len(raw) != length:
    raise RuntimeError(f"Expected {length} crash bytes, received {len(raw)}")
Path(destination).write_bytes(raw)
print(f"M5_CRASH_DATA_CAPTURED bytes={len(raw)} magic={raw[:4]!r}")
