#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""J700 ANE_SYS power tests. These do not execute an ANE model or DMA job."""
import argparse
import errno
import gzip
import hashlib
import json
import os
import pathlib
import re
import subprocess
import time

SYS = pathlib.Path("/sys")
DRIVER = SYS / "bus/platform/drivers/apple-ane-bringup"
NODE = SYS / "firmware/devicetree/base/soc/ane@400000000"
SUMMARY = SYS / "kernel/debug/pm_genpd/pm_genpd_summary"


def run(*args, check=True):
    return subprocess.run(args, text=True, capture_output=True, check=check)


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def wait_for(predicate, message, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.025)
    raise RuntimeError(message)


def domain_state():
    match = re.search(r"^\s*ane_sys\s+(\S+)", SUMMARY.read_text(), re.M)
    check(match is not None, "ANE_SYS not present in genpd summary")
    return match.group(1)


def domain_off():
    return domain_state().startswith("off")


def device():
    matches = [p for p in (SYS / "bus/platform/devices").iterdir()
               if (p / "of_node").is_symlink()
               and (p / "of_node").resolve() == NODE]
    check(len(matches) == 1, f"expected exactly one test device, found {len(matches)}")
    return matches[0]


def desktop_snapshot():
    result = {}
    for name in ("kwin_x11", "kwin_wayland", "glxgears"):
        for pid in run("pgrep", "-x", name, check=False).stdout.split():
            p = pathlib.Path("/proc") / pid / "stat"
            if p.exists():
                fields = p.read_text().rsplit(") ", 1)[1].split()
                result[f"{name}:{pid}"] = {"state": fields[0],
                                          "ticks": int(fields[11]) + int(fields[12])}
    return result


def kernel_errors(since):
    data = run("dmesg", "--raw", "--level=emerg,alert,crit,err,warn").stdout
    lines = []
    for line in data.splitlines():
        match = re.search(r"\[\s*(\d+\.\d+)\]", line)
        if match and float(match.group(1)) >= since:
            if any(expected in line for expected in (
                "unexpected T8140 ANE cluster resource",
                "unexpected T8140 ANE PWGATE resource",
                "unexpected T8140 ANE CE resource",
                "expected T8140 ANE_SYS power domain",
                "probe with driver apple-ane-bringup failed with error -22",
            )):
                continue
            lines.append(line)
    return lines


def main():
    started = time.monotonic()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--modules", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--cycles", type=int, default=20)
    parser.add_argument("--keep-loaded", action="store_true")
    parser.add_argument("--power-snapshot", action="store_true",
                        help="read ANE child states through the existing PMGR syscon")
    parser.add_argument("--engine-power", action="store_true",
                        help="test the opt-in child power and CE initialization sequence")
    args = parser.parse_args()
    if args.engine_power and not args.power_snapshot:
        parser.error("--engine-power requires --power-snapshot for verification")
    check(1 <= args.cycles <= 1000, "cycle count must be between 1 and 1000")
    check(os.geteuid() == 0, "root required")
    compat = (SYS / "firmware/devicetree/base/compatible").read_bytes().split(b"\0")
    check(b"apple,j700" in compat and b"apple,t8140" in compat,
          "refusing a target other than J700/T8140")
    check(not NODE.exists(), "ANE node already exists; do not replace another driver's node")
    for mod in ("apple_ane_bringup", "ane_test_overlay"):
        check(not (SYS / "module" / mod).exists(), f"module already loaded: {mod}")
    check(domain_off(), "ANE_SYS was already powered on; preserve existing ownership")
    manifest = json.loads((args.modules / "manifest.json").read_text())
    check(manifest["kernel_release"] == os.uname().release, "kernel release mismatch")
    live_config = gzip.decompress(pathlib.Path("/proc/config.gz").read_bytes())
    check(hashlib.sha256(live_config).hexdigest() == manifest["kernel_config_sha256"],
          "running kernel configuration differs from the module build")
    for name in ("apple-ane-bringup.ko", "ane-test-overlay.ko"):
        actual = hashlib.sha256((args.modules / name).read_bytes()).hexdigest()
        check(actual == manifest["sha256"][name], f"module checksum mismatch: {name}")
    start = float(pathlib.Path("/proc/uptime").read_text().split()[0])
    boot_id = pathlib.Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    result = {"scope": "ANE_SYS power-domain tests; no ANE inference or DMA",
              "engine_power": args.engine_power,
              "kernel": os.uname().release, "boot_id": boot_id,
              "runner_sha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
              "module_sha256": {name: manifest["sha256"][name] for name in
                                ("apple-ane-bringup.ko", "ane-test-overlay.ko")},
              "taint_before": pathlib.Path("/proc/sys/kernel/tainted").read_text().strip(),
              "desktop_before": desktop_snapshot(), "tests": [], "passed": False}
    overlay_loaded = False
    driver_loaded = False

    def load_driver():
        nonlocal driver_loaded
        run("insmod", str(args.modules / "apple-ane-bringup.ko"),
            f"enable_power_snapshot={int(args.power_snapshot)}",
            f"enable_engine_power={int(args.engine_power)}")
        driver_loaded = True

    def power_snapshot(dev, phase):
        raw = (dev / "power_state").read_text().strip()
        fields = {k: int(v, 16) for k, v in
                  re.findall(r"(sys|cpu|subblocks)=(0x[0-9a-f]+)", raw)}
        check(set(fields) == {"sys", "cpu", "subblocks"}, "invalid power-state format")
        expected = 0xff if phase == "on" else 0
        check(fields["sys"] & 0xff == expected, f"PMGR state disagrees with genpd: {raw}")
        child_expected = expected if args.engine_power else 0
        check(all(fields[k] & 0xff == child_expected for k in ("cpu", "subblocks")),
              f"unexpected child power state: {raw}")
        check(all(not fields[k] & 0x90000c00 for k in ("cpu", "subblocks")),
              f"child automatic power or reset unexpectedly enabled: {raw}")
        result.setdefault("power_snapshots", []).append({"phase": phase, **fields})
        return fields

    def passed(name, **details):
        result["tests"].append({"name": name, "passed": True, **details})
        print("PASS " + name, flush=True)

    def load_overlay(mode):
        nonlocal overlay_loaded
        run("insmod", str(args.modules / "ane-test-overlay.ko"),
            "enable_power_test=1", f"mode={mode}")
        overlay_loaded = True

    def remove_overlay():
        nonlocal overlay_loaded
        run("rmmod", "ane_test_overlay")
        overlay_loaded = False
        check(not NODE.exists(), "test DT node leaked after overlay removal")
        wait_for(domain_off, "ANE_SYS did not return to off after overlay removal")

    try:
        rejected = run("insmod", str(args.modules / "ane-test-overlay.ko"), check=False)
        check(rejected.returncode != 0 and not NODE.exists(), "missing opt-in accepted")
        passed("reject_without_power_opt_in")
        load_driver()
        for mode, name in ((1, "reject_shared_cpu_register_range"),
                           (2, "reject_missing_power_domain"),
                           (3, "reject_wrong_pwgate_range"),
                           (4, "reject_wrong_ce_range")):
            load_overlay(mode)
            dev = device()
            check(not (dev / "driver").exists(), f"invalid device bound: {name}")
            remove_overlay()
            passed(name)

        load_overlay(0)
        dev = device()
        wait_for(lambda: (dev / "bringup_status").exists(), "valid device did not bind")
        expected_phase = "engine-power-and-ce" if args.engine_power else "power-domain-only"
        check((dev / "bringup_status").read_text().strip() == expected_phase, "wrong phase")
        check((dev / "inference_available").read_text().strip() == "0", "false inference capability")
        check([int(x, 0) for x in (dev / "cluster_range").read_text().split()] ==
              [0x400000000, 0x2000000], "unexpected cluster resource")
        wait_for(domain_off, "driver did not idle ANE_SYS after probe")
        passed("bind_verified_j700_resources")
        passed("reports_inference_unavailable")

        if args.power_snapshot:
            power_snapshot(dev, "off")
        else:
            try:
                (dev / "power_state").read_text()
            except OSError as error:
                check(error.errno == errno.EOPNOTSUPP, "unexpected power snapshot error")
            else:
                raise RuntimeError("power snapshots accepted without opt-in")
            passed("reject_power_snapshot_without_opt_in")

        for iteration in range(args.cycles):
            (dev / "power/control").write_text("on\n")
            wait_for(lambda: (dev / "power/runtime_status").read_text().strip() == "active"
                     and domain_state() == "on", "ANE_SYS did not power on")
            if args.power_snapshot:
                power_snapshot(dev, "on")
            (dev / "power/control").write_text("auto\n")
            wait_for(lambda: (dev / "power/runtime_status").read_text().strip() == "suspended"
                     and domain_off(), "ANE_SYS did not power off")
            if args.power_snapshot:
                power_snapshot(dev, "off")
        transitions = (dev / "runtime_transitions").read_text().strip()
        counters = {k: int(v) for k, v in re.findall(r"(resume|suspend)=(\d+)", transitions)}
        check(counters.get("resume", 0) >= args.cycles and
              counters.get("suspend", 0) >= args.cycles, "runtime PM callback count mismatch")
        passed("runtime_power_cycles", cycles=args.cycles, transitions=counters)
        ce_count = int((dev / "ce_initializations").read_text())
        check(ce_count == (args.cycles if args.engine_power else 0),
              "CE initialization count differs from the requested cycles")
        passed("ce_initializations_match_opt_in", completed=ce_count)
        if args.power_snapshot:
            check(counters == {"resume": args.cycles, "suspend": args.cycles},
                  "reading PMGR unexpectedly changed runtime PM state")
            passed("pmgr_snapshots_match_genpd_without_extra_runtime_transitions",
                   samples=len(result["power_snapshots"]))

        for unused in range(3):
            (dev / "power/control").write_text("on\n")
            wait_for(lambda: domain_state() == "on", "domain not on before unbind")
            (DRIVER / "unbind").write_text(dev.name + "\n")
            check(not (dev / "driver").exists(), "unbind failed")
            wait_for(domain_off, "domain remained on after unbind")
            # Release the test's runtime-PM hold before rebinding the device.
            (dev / "power/control").write_text("auto\n")
            (DRIVER / "bind").write_text(dev.name + "\n")
            wait_for(lambda: (dev / "bringup_status").exists() and domain_off(), "rebind failed")
        passed("unbind_while_powered_then_rebind", cycles=3)

        (dev / "power/control").write_text("on\n")
        wait_for(lambda: domain_state() == "on", "domain not on before overlay removal")
        remove_overlay()
        run("rmmod", "apple_ane_bringup")
        driver_loaded = False
        check(not DRIVER.exists(), "driver sysfs registration leaked")
        passed("unload_while_powered_releases_device_and_power_domain")

        load_driver()
        load_overlay(0)
        dev = device()
        wait_for(lambda: (dev / "bringup_status").exists() and domain_off(), "reload failed")
        if args.power_snapshot:
            power_snapshot(dev, "off")
        passed("reload_after_complete_unload")
        result["desktop_after"] = desktop_snapshot()
        for name in result["desktop_before"]:
            check(name in result["desktop_after"] and result["desktop_after"][name]["state"] != "Z",
                  f"desktop process lost during ANE test: {name}")
        passed("desktop_processes_survive", observed=list(result["desktop_after"]))
        check(pathlib.Path("/proc/sys/kernel/random/boot_id").read_text().strip() == boot_id,
              "boot ID changed")
        result["new_kernel_warnings_or_errors"] = kernel_errors(start)
        check(not result["new_kernel_warnings_or_errors"], "new kernel warning/error; inspect result")
        passed("no_new_unexpected_kernel_warnings_or_errors")
        result["passed"] = True
    except Exception as error:
        result["error"] = str(error)
        if isinstance(error, subprocess.CalledProcessError):
            result["command_stderr"] = error.stderr
        raise
    finally:
        if not (result["passed"] and args.keep_loaded):
            if overlay_loaded:
                cleanup = run("rmmod", "ane_test_overlay", check=False)
                result["overlay_cleanup_status"] = cleanup.returncode
            if driver_loaded:
                cleanup = run("rmmod", "apple_ane_bringup", check=False)
                result["driver_cleanup_status"] = cleanup.returncode
        result["kept_loaded"] = bool(result["passed"] and args.keep_loaded)
        result["final_domain_state"] = domain_state()
        result["taint_after"] = pathlib.Path("/proc/sys/kernel/tainted").read_text().strip()
        result["elapsed_seconds"] = round(time.monotonic() - started, 3)
        args.output.write_text(json.dumps(result, indent=2) + "\n")
    print("ANE_HARDWARE_TEST_PASS inference_available=0", flush=True)


if __name__ == "__main__":
    main()
