#!/usr/bin/env python3
"""Open both J813 input interfaces before Xorg, without reading input events."""
import os
from pathlib import Path
import time


def main():
    if b"apple,j813\0" not in Path("/proc/device-tree/compatible").read_bytes():
        raise SystemExit("Expected J813")
    names = {"Apple MTP keyboard", "Apple MTP multi-touch"}
    devices = {}
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        for entry in Path("/sys/class/input").glob("event*"):
            name = (entry / "device/name").read_text().strip()
            if name in names:
                devices[name] = Path("/dev/input") / entry.name
        if devices.keys() == names:
            break
        time.sleep(0.1)
    if devices.keys() != names:
        raise SystemExit("J813 input interfaces did not register")

    for name in sorted(names):
        for attempt in range(1, 4):
            try:
                fd = os.open(devices[name], os.O_RDONLY | os.O_NONBLOCK)
            except OSError as error:
                print(f"{name}: open attempt {attempt} failed, errno={error.errno}", flush=True)
                if attempt == 3:
                    raise SystemExit("J813 input firmware did not become ready")
                time.sleep(1)
            else:
                os.close(fd)
                print(f"{name}: ready", flush=True)
                break
    print("J813_INPUT_READY", flush=True)


if __name__ == "__main__":
    main()
