#!/usr/bin/env python3
"""Restart the Neo X11 compositor after an explicit GPU device-loss error."""
import argparse
from collections import deque
import json
from pathlib import Path
import subprocess
import time


def main_pid(unit):
    result = subprocess.run(
        ["systemctl", "--user", "show", unit, "-p", "MainPID", "--value"],
        capture_output=True, text=True, timeout=5, check=True,
    )
    return int(result.stdout.strip() or "0")


def reports_device_loss(event):
    message = event.get("MESSAGE", "")
    return isinstance(message, str) and (
        "VK_ERROR_DEVICE_LOST" in message or "zink: DEVICE LOST!" in message
    )


def watch(unit):
    # Replay briefly so a loss during service startup cannot be missed. Old
    # compositor messages are rejected by PID, including after each restart.
    journal = subprocess.Popen(
        ["journalctl", "--user", "--unit=" + unit, "--follow", "--since=-30sec",
         "--output=json", "--no-pager"], stdout=subprocess.PIPE, text=True,
    )
    restarts = deque()
    try:
        for line in journal.stdout:
            try:
                event = json.loads(line)
                pid = int(event.get("_PID", "0"))
            except (ValueError, TypeError):
                continue
            if not reports_device_loss(event) or pid <= 1 or pid != main_pid(unit):
                continue

            # Bound repeated recovery if a persistent driver fault is present.
            now = time.monotonic()
            while restarts and now - restarts[0] >= 60:
                restarts.popleft()
            if len(restarts) >= 3:
                delay = max(0, 60 - (now - restarts[0]))
                print(f"Repeated GPU loss; delaying KWin recovery {delay:.1f}s", flush=True)
                time.sleep(delay)
            if pid != main_pid(unit):
                continue

            print(f"GPU device lost in KWin PID {pid}; restarting {unit}", flush=True)
            subprocess.run(["systemctl", "--user", "restart", unit],
                           check=True, timeout=20)
            restarts.append(time.monotonic())
            print(f"KWin recovery completed: {pid} -> {main_pid(unit)}", flush=True)
    finally:
        journal.terminate()
        try:
            journal.wait(timeout=5)
        except subprocess.TimeoutExpired:
            journal.kill()
            journal.wait()
    raise RuntimeError("KWin journal stream ended")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--unit", default="plasma-kwin_x11.service")
    args = parser.parse_args()
    compatible = Path("/proc/device-tree/compatible").read_bytes().split(b"\0")
    if b"apple,j700" not in compatible or b"apple,t8140" not in compatible:
        raise SystemExit("This recovery service is for Neo J700/T8140 only")
    watch(args.unit)


if __name__ == "__main__":
    main()
