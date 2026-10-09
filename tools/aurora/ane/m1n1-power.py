#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Target-guarded J700 ANE power observations from the m1n1 proxy."""
import argparse
import json
import os
import pathlib
import runpy
import signal
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--m1n1-root", type=pathlib.Path, required=True)
    parser.add_argument("--cycle", action="store_true",
                        help="exercise the observed H17 manual child power sequence")
    parser.add_argument("--probe-ce", action="store_true",
                        help="read CE status only after all child power states are active")
    parser.add_argument("--firmware", type=pathlib.Path,
                        help="attempt a bounded bootstrap of the matching H17 firmware")
    parser.add_argument("--scratch-probe", action="store_true",
                        help="isolate one scratch-register write without changing DART")
    parser.add_argument("--init-ce", action="store_true",
                        help="run the native H17 CE initialization handshake")
    parser.add_argument("--handshake", action="store_true",
                        help="provide the firmware heap, IPC arena, and host boot arguments")
    parser.add_argument("--command", action="store_true",
                        help="send the native startup command after the firmware handshake")
    args = parser.parse_args()
    if (args.probe_ce or args.scratch_probe or args.init_ce) and not args.cycle:
        parser.error("engine register probes require --cycle")
    if args.scratch_probe and args.firmware:
        parser.error("scratch isolation and firmware bootstrap are separate experiments")
    if args.probe_ce and args.firmware:
        parser.error("firmware teardown gates the engine; use a separate CE probe")
    if args.handshake and not args.firmware:
        parser.error("--handshake requires --firmware")
    if args.command and not args.handshake:
        parser.error("--command requires --handshake")
    if args.firmware and not args.init_ce:
        parser.error("firmware register writes require --init-ce")

    def watchdog(signum, frame):
        print("WATCHDOG_ABORT hardware_state_unknown; target reset required", flush=True)
        os._exit(124)

    # Endless target exception chatter can prevent a serial read timeout.
    signal.signal(signal.SIGALRM, watchdog)
    signal.alarm(45)
    firmware_module = None
    firmware_image = None
    if args.firmware:
        if not args.cycle:
            parser.error("firmware bootstrap requires --cycle")
        firmware_module = runpy.run_path(str(pathlib.Path(__file__).with_name("m1n1-firmware.py")))
        firmware_image = firmware_module["prepare_image"](args.firmware)
    endpoint = "/dev/cu.kis-00100000-ch-0"
    if os.environ.get("M1N1DEVICE", endpoint) != endpoint:
        parser.error("this experiment requires the verified Neo KIS endpoint")
    os.environ["M1N1DEVICE"] = endpoint
    os.environ["M1N1TIMEOUT"] = "5"
    os.environ["M1N1NOBAUD"] = "1"  # KIS is not a baud-controlled UART.
    sys.path.insert(0, str(args.m1n1_root / "proxyclient"))
    from m1n1.proxy import GUARD, M1N1Proxy, UartInterface
    from m1n1.proxyutils import ProxyUtils, bootstrap_port
    if args.firmware:
        transport = args.m1n1_root.parent / "neo-update/kis-transport.py"
        runpy.run_path(str(transport))["install"]()

    iface = UartInterface()
    p = M1N1Proxy(iface)
    try:
        bootstrap_port(iface, p)
        iface.nop()
        if p.get_chipid() != 0x8140 or p.iodev_whoami() != 1:
            raise RuntimeError("refusing a target other than T8140 over KIS")
        u = ProxyUtils(p)
        if u.adt.model != "Mac17,5":
            raise RuntimeError("refusing a target other than the Neo")
        ane = u.adt["/arm-io/ane"]
        if ane.get_reg(0) != (0x400000000, 0x2000000):
            raise RuntimeError("unexpected J700 ANE cluster")
        if ane.get_reg(1) != (0x300700000, 0x18000):
            raise RuntimeError("unexpected J700 ANE PMGR range")
        if ane.get_reg(2) != (0x300724000, 0x4000):
            raise RuntimeError("unexpected J700 ANE PWGATE range")
        if ane.ane_type != 0x200:
            raise RuntimeError("unexpected J700 ANE type")
        print("TARGET_CONFIRMED J700 T8140 Mac17,5", flush=True)
        for name, value in u.adt["/chosen/memory-map"]._properties.items():
            if "ane" in name.lower():
                print(f"ANE_MEMORY {name}={value!r}", flush=True)
        registers = {"sys": 0x300700290, "cpu": 0x30070c000,
                     "subblocks": 0x30070c008, "pwgate_request": 0x30072668c,
                     "pwgate_ack": 0x3007266b0}
        p.get_exc_count()  # Clear any earlier guarded exception count.
        p.set_exc_guard(GUARD.MARK)
        try:
            writes = []

            def read(name):
                value = p.read32(registers[name])
                if p.get_exc_count():
                    raise RuntimeError(f"MMIO exception reading {name}")
                return value

            def snapshot(label):
                values = {name: read(name) for name in registers}
                print(label + " " + json.dumps({k: f"{v:#010x}" for k, v in values.items()}),
                      flush=True)
                return values

            def write(name, value):
                writes.append((name, value))
                print(f"WRITE {name}={value:#010x}", flush=True)
                p.write32(registers[name], value)
                if p.get_exc_count():
                    raise RuntimeError(f"MMIO exception writing {name}")

            def poll(name, mask, expected):
                deadline = time.monotonic() + 0.5
                while True:
                    value = read(name)
                    if value & mask == expected:
                        return value
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"{name} timeout: {value:#x} & {mask:#x} != {expected:#x}")

            states = snapshot("BEFORE")
            firmware_buffers = None
            if firmware_image is not None:
                firmware_buffers = firmware_module["upload_image"](
                    p, u, firmware_image, handshake=args.handshake)
            if args.cycle:
                if any(states[k] & 0x90000cff for k in ("cpu", "subblocks")):
                    raise RuntimeError("child domain is not quiescent and manually power-gated")
                if states["sys"] & 0xff or states["pwgate_request"] != 3 or states["pwgate_ack"]:
                    raise RuntimeError("unexpected baseline; preserving existing ownership")
                started = False
                children_quiesced = False

                def quiesce_children():
                    nonlocal children_quiesced
                    if children_quiesced:
                        return
                    write("subblocks", 0x300)
                    poll("subblocks", 0x8ff, 0)
                    write("pwgate_request", 3)
                    poll("pwgate_request", 3, 3)
                    write("pwgate_ack", 0)
                    poll("pwgate_ack", 1, 0)
                    write("cpu", 0x300)
                    poll("cpu", 0x8ff, 0)
                    children_quiesced = True
                    print("ANE_CHILDREN_QUIESCED", flush=True)

                try:
                    # ADT-derived parent recursion belongs to m1n1's PMGR driver.
                    started = True
                    if p.pmgr_power_enable(60):
                        raise RuntimeError("m1n1 could not enable ANE_SYS")
                    poll("sys", 0xff, 0xff)
                    write("cpu", 0xf)
                    poll("cpu", 0xff, 0xff)
                    write("pwgate_request", 0)
                    poll("pwgate_request", 3, 0)
                    # Keep AUTO_ENABLE clear while no firmware owns the domain.
                    write("subblocks", 0xf)
                    poll("subblocks", 0xff, 0xff)
                    active = snapshot("ACTIVE")
                    if args.init_ce:
                        ce = 0x4018a4000

                        def ce_read(offset):
                            value = p.read64(ce + offset)
                            if p.get_exc_count():
                                raise RuntimeError(f"CE read fault at {offset:#x}")
                            return value

                        wanted = (ce_read(0x10) & 3) ^ 3
                        if ce_read(0x18) & wanted:
                            raise RuntimeError("CE is in error state")
                        if wanted:
                            print(f"CE_TRIGGER {wanted:#x}", flush=True)
                            p.write64(ce, wanted)
                            if p.get_exc_count():
                                raise RuntimeError("CE trigger write fault")
                            deadline = time.monotonic() + 0.1
                            while ce_read(8) & 3 != wanted:
                                if time.monotonic() >= deadline:
                                    raise RuntimeError("CE initialization timed out")
                            if ce_read(0x18) & wanted:
                                raise RuntimeError("CE initialization reported an error")
                            p.write64(ce + 8, wanted)
                            if p.get_exc_count():
                                raise RuntimeError("CE completion acknowledgement fault")
                        print(f"CE_INITIALIZED status={ce_read(0x10):#x}", flush=True)
                    if args.scratch_probe:
                        address = 0x401880020
                        value = p.read32(address)
                        if p.get_exc_count():
                            raise RuntimeError("scratch baseline read fault")
                        print(f"SCRATCH_WRITE_SAME_VALUE {address:#x}={value:#x}", flush=True)
                        p.write32(address, value)
                        if p.get_exc_count() or p.read32(address) != value:
                            raise RuntimeError("scratch isolation write failed")
                        print("SCRATCH_WRITE_READBACK_OK", flush=True)
                    if firmware_image is not None:
                        result = firmware_module["run_bootstrap"](
                            p, u, firmware_buffers, quiesce_children,
                            handshake=args.handshake, command=args.command)
                        print("FW_RESULT " + json.dumps(result), flush=True)
                        if not result.get("wake") or (args.handshake and not result.get("handshake")):
                            raise RuntimeError("firmware did not complete the requested bootstrap")
                    if args.probe_ce:
                        for offset in (0x18a4008, 0x18a4010, 0x18a4018):
                            value = p.read64(0x400000000 + offset)
                            if p.get_exc_count():
                                raise RuntimeError(f"CE read fault at cluster offset {offset:#x}")
                            print(f"CE_READ {offset:#x}={value:#018x}", flush=True)
                        for offset, width in ((0x1050000, 64), (0x1600044, 32),
                                              (0x1880038, 32), (0x188003c, 32)):
                            value = (p.read64 if width == 64 else p.read32)(0x400000000 + offset)
                            if p.get_exc_count():
                                raise RuntimeError(f"bootstrap read fault at {offset:#x}")
                            print(f"BOOT_READ {offset:#x}={value:#018x}", flush=True)
                        dart = u.adt["/arm-io/dart-ane"]
                        if dart.get_reg(0) != (0x401800000, 0xc000):
                            raise RuntimeError("unexpected T8140 ANE DART aperture")
                        for offset in (0, 4, 8, 0xc, 0x100, 0xc00, 0x1000, 0x1400):
                            value = p.read32(0x401800000 + offset)
                            if p.get_exc_count():
                                raise RuntimeError(f"DART read fault at {offset:#x}")
                            print(f"DART_READ {offset:#x}={value:#010x}", flush=True)
                finally:
                    if started:
                        # Stop subblocks, block CPU power requests, then gate CPU.
                        quiesce_children()
                        if p.pmgr_power_disable(60):
                            raise RuntimeError("could not restore ANE_SYS off")
                        poll("sys", 0x8ff, 0)
                        snapshot("RESTORED")
                print("ANE_CHILD_POWER_CYCLE_PASS", flush=True)
            print(json.dumps({"states": states, "writes": writes,
                              "inference_available": False}))
        finally:
            p.set_exc_guard(GUARD.OFF)
    finally:
        iface.dev.close()
        signal.alarm(0)


if __name__ == "__main__":
    main()
