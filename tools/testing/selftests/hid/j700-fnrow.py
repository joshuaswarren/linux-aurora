#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""Manual J700 evtest/hid-tools capture. Run over SSH, not on the tested keyboard."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import time


MEDIA = (
    "KEY_BRIGHTNESSDOWN", "KEY_BRIGHTNESSUP", "KEY_SCALE", "KEY_SEARCH",
    "KEY_MICMUTE", "KEY_SLEEP", "KEY_PREVIOUSSONG", "KEY_PLAYPAUSE",
    "KEY_NEXTSONG", "KEY_MUTE", "KEY_VOLUMEDOWN", "KEY_VOLUMEUP",
)
PARAM = Path("/sys/module/hid_apple/parameters/fnmode")
EVENT = re.compile(r"type 1 \(EV_KEY\), code \d+ \((\w+)\), value ([012])")


def key_events(text):
    return [(key, int(value)) for key, value in EVENT.findall(text)
            if value != "2"]


def expected(mode, number, held):
    media = (mode in (1, 3) and not held) or (mode == 2 and held)
    key = MEDIA[number - 1] if media else f"KEY_F{number}"
    pairs = [(key, 1), (key, 0)]
    return [("KEY_FN", 1), *pairs, ("KEY_FN", 0)] if held else pairs


def read(path):
    try:
        return path.read_text().strip()
    except OSError:
        return "<absent>"


def identify(event):
    device = (Path("/sys/class/input") / event.name / "device").resolve()
    if read(device / "name") != "Apple MTP keyboard":
        raise RuntimeError("Select the internal Apple MTP keyboard event node")
    hid = next((p for p in device.parents
                if (p / "subsystem").resolve() == Path("/sys/bus/hid")), None)
    if hid is None or (hid / "driver").resolve().name != "apple":
        raise RuntimeError("Keyboard is not bound to hid-apple")
    raws = [p for p in Path("/sys/class/hidraw").glob("hidraw*")
            if (p / "device").resolve() == hid]
    if len(raws) != 1:
        raise RuntimeError(f"Expected one matching hidraw node, found {raws}")
    return device, hid, Path("/dev") / raws[0].name


def inventory(out, device, hid, raw):
    info = {"kernel": os.uname().release, "hidraw": str(raw),
            "driver": str((hid / "driver").resolve()),
            "input": {p: read(device / p) for p in
                      ("name", "id/bustype", "id/vendor", "id/product", "id/version")},
            "hid": {p: read(hid / p) for p in ("country", "apple_layout_id")},
            "parameters": {p.name: read(p) for p in PARAM.parent.iterdir()},
            "leds": {p.name: {v: read(p / v) for v in
                              ("brightness", "max_brightness")}
                     for p in Path("/sys/class/leds").glob("*")}}
    # Save only layout-related properties, never a whole DT or device serials.
    dt = Path("/sys/firmware/devicetree/base")
    info["dt"] = {}
    for prop in ("apple,keyboard-layout-id", "hid-country-code",
                 "asahi,kblang-code", "apple,no-stm"):
        for p in dt.rglob(prop):
            data = p.read_bytes()
            info["dt"][str(p.relative_to(dt))] = {
                "hex": data.hex(),
                "u32_be": int.from_bytes(data, "big") if len(data) == 4 else None,
            }
    (out / "inventory.json").write_text(json.dumps(info, indent=2) + "\n")
    (out / "report-descriptor.bin").write_bytes((hid / "report_descriptor").read_bytes())
    with (out / "report-descriptor.txt").open("w") as f:
        subprocess.run(["hid-decode", str(out / "report-descriptor.bin")],
                       stdout=f, stderr=subprocess.STDOUT, check=True)


def start_capture(command, path):
    with path.open("w") as output:
        return subprocess.Popen(command, stdout=output, stderr=subprocess.STDOUT,
                                env={**os.environ, "LC_ALL": "C", "PYTHONUNBUFFERED": "1"})


def stop_capture(proc):
    if proc.poll() is None:
        proc.send_signal(signal.SIGINT)
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def capture(args, event, raw, out):
    original = PARAM.read_text()
    results = []
    processes = []
    evpath = out / "evtest.txt"
    hidpath = out / "reports.hid"
    try:
        processes.append(start_capture(["stdbuf", "-oL", "evtest", "--grab", str(event)], evpath))
        processes.append(start_capture(["hid-recorder", str(raw)], hidpath))
        deadline = time.monotonic() + 10
        while "Testing ..." not in read(evpath) or "R:" not in read(hidpath):
            if any(p.poll() is not None for p in processes) or time.monotonic() > deadline:
                raise RuntimeError("Capture did not start; inspect evtest.txt and reports.hid")
            time.sleep(0.1)
        time.sleep(0.5)
        if "grabbed by another process" in read(evpath):
            raise RuntimeError("evtest could not grab keyboard; stop the other capture")

        def step(label, instruction, want):
            input(f"\n{label}: {instruction}\nRelease ALL keys. Press Enter on SSH host to arm: ")
            evstart = evpath.stat().st_size
            hidstart = hidpath.stat().st_size
            started = time.time()
            print(f"GO: {instruction} ({args.seconds:g} seconds)", flush=True)
            time.sleep(args.seconds)
            if any(p.poll() is not None for p in processes):
                raise RuntimeError("Capture exited unexpectedly")
            with evpath.open("rb") as f:
                f.seek(evstart)
                evtext = f.read().decode(errors="replace")
            with hidpath.open("rb") as f:
                f.seek(hidstart)
                rawtext = f.read().decode(errors="replace")
            (out / f"{label}.evtest").write_text(evtext)
            (out / f"{label}.hid-events").write_text(rawtext)
            got = key_events(evtext)
            ok = (got == want and "SYN_DROPPED" not in evtext
                  and bool(re.search(r"^E:", rawtext, re.MULTILINE)))
            result = {"step": label, "start_unix": started, "expected": want,
                      "actual": got, "pass": ok}
            results.append(result)
            (out / "results.json").write_text(json.dumps(results, indent=2) + "\n")
            print(f"{'PASS' if ok else 'FAIL'}: {got}", flush=True)
            if got != want:
                print(f"Expected: {want}; retain capture and continue for diagnosis.")

        for mode in args.modes:
            input(f"\nRelease ALL keys before changing fnmode to {mode}. Enter on SSH host: ")
            PARAM.write_text(f"{mode}\n")
            if read(PARAM) != str(mode):
                raise RuntimeError("fnmode write did not take effect")
            step(f"mode{mode}-fn", "Tap and release Globe/Fn only",
                 [("KEY_FN", 1), ("KEY_FN", 0)])
            for held in (False, True):
                for number in range(1, 13):
                    instruction = (f"Hold Fn, tap F{number}, release F{number}, then release Fn"
                                   if held else f"Tap and release F{number} without Fn")
                    step(f"mode{mode}-{'fn' if held else 'plain'}-F{number}",
                         instruction, expected(mode, number, held))
            want = expected(mode, 2, True)
            step(f"mode{mode}-release-fn-first",
                 "Hold Fn, hold F2, release Fn, then release F2",
                 [want[0], want[1], want[3], want[2]])
    finally:
        # Keep the grab until the original mode has been restored.
        try:
            PARAM.write_text(original)
        finally:
            for proc in reversed(processes):
                stop_capture(proc)
    return all(r["pass"] for r in results) and bool(results)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", type=Path, help="default: unique Apple MTP keyboard")
    parser.add_argument("--out", type=Path, required=True, help="new output directory")
    parser.add_argument("--seconds", type=float, default=5, help="seconds per key (default: 5)")
    parser.add_argument("--modes", type=int, nargs="+", choices=range(6),
                        default=[0, 1, 2, 3, 4, 5])
    args = parser.parse_args()
    if args.seconds < 2 or args.seconds > 60:
        parser.error("--seconds must be between 2 and 60")
    if os.geteuid() != 0 or not os.environ.get("SSH_CONNECTION"):
        parser.error("Run as root over SSH (preserve SSH_CONNECTION with sudo)")
    if len(set(args.modes)) != len(args.modes):
        parser.error("Each mode may only be specified once")
    for command in ("evtest", "stdbuf", "hid-recorder", "hid-decode"):
        if shutil.which(command) is None:
            parser.error(f"Missing command: {command}")
    if args.event is None:
        events = [Path("/dev/input") / p.name
                  for p in Path("/sys/class/input").glob("event*")
                  if read(p / "device/name") == "Apple MTP keyboard"]
        if len(events) != 1:
            parser.error(f"Select --event from {events}")
        args.event = events[0]
    device, hid, raw = identify(args.event)
    if read(PARAM.parent / "swap_fn_leftctrl") != "0":
        parser.error("Set hid_apple.swap_fn_leftctrl=0 for this qualification")
    os.umask(0o077)
    args.out.mkdir(parents=True, exist_ok=False)
    inventory(args.out, device, hid, raw)
    print("Raw capture grabs the keyboard. Use the SSH host for prompts.\n"
          "Release all keys before each step; never press the power button.\n"
          "Keep sinks muted and stop media players before starting.")
    legends = input("Record physical F1..F12 legends and ANSI/ISO/JIS appearance: ")
    (args.out / "legends.txt").write_text(legends + "\n")
    ok = capture(args, args.event, raw, args.out)
    print(f"{'PASS' if ok else 'FAIL'} for requested modes; GNOME acceptance is separate.")
    return 0 if ok else 1


def interrupted(signum, frame):
    raise KeyboardInterrupt


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    try:
        raise SystemExit(main())
    except (RuntimeError, OSError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        raise SystemExit(f"Capture aborted: {error}")
