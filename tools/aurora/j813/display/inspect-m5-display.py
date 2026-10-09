#!/usr/bin/env python3
"""Capture J813 display topology and optional gated, read-only runtime state."""
import argparse
import json
import os
from pathlib import Path
import runpy
import subprocess
import struct
import sys
import time


def plain(value):
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    if isinstance(value, bytes):
        return {"hex": value.hex()} if len(value) <= 4096 else {"bytes": len(value)}
    if isinstance(value, dict):
        return {str(k): plain(v) for k, v in value.items() if not str(k).startswith("_")}
    if isinstance(value, (list, tuple)):
        return [plain(v) for v in value]
    return {"type": type(value).__name__}


def capture_display_topology(u, output, proxy=None, mappings=False):
    if u.adt.model != "Mac17,3":
        raise ValueError("Expected J813 / Mac17,3")
    tags = ("dcp", "display", "disp0", "disp1", "dptx", "dpp", "dfr", "pmgr", "pmc", "pmp")
    keys = {"compatible", "interrupts", "interrupt-parent", "interrupts-valid",
            "clock-gates", "power-gates", "clock-id", "clock-ids", "clock-frequencies",
            "iommu-parent", "iommu-mapper", "#address-cells", "#size-cells",
            "ranges", "dma-ranges", "name", "phandle", "AAPL,phandle",
            "iop-version", "role", "pre-loaded", "running", "quiesced", "idle-msg",
            "segment-names", "segment-ranges", "coredump-region", "crashlog-region",
            "devices", "ps-regs", "perf-regs"}
    result = {"model": u.adt.model, "source": "live m1n1 ADT; no MMIO accesses",
              "nodes": {}, "driver_probe_performed": False}
    for node in u.adt.walk_tree():
        path = node._path
        relevant = any(tag in path.lower() for tag in tags) or path.endswith(("/vram", "/arm-io"))
        memory_map = path.endswith("/chosen/memory-map")
        carveout_map = path.endswith("/chosen/carveout-memory-map")
        if not relevant and not memory_map and not carveout_map:
            continue
        entry = {"property_names": sorted(node._properties)}
        entry["properties"] = {
            key: plain(value) for key, value in node._properties.items()
            if (key in keys and not memory_map) or any(tag in key.lower() for tag in tags)
            or key.startswith("function-") or "agent" in key or "bwr" in key
            or (carveout_map and key in {"region-id-49", "region-id-50", "region-id-57",
                                        "region-id-94", "region-id-95"})
        }
        if relevant and "reg" in node._properties:
            entry["raw_regs"] = plain(node.reg)
            if isinstance(node.reg, int):
                entry["stream_id"] = node.reg
            else:
                try:
                    entry["translated_regs"] = [
                        {"address": hex(address), "size": hex(size)}
                        for address, size in (node.get_reg(i) for i in range(len(node.reg)))
                    ]
                except (AttributeError, TypeError, ValueError) as error:
                    entry["reg_translation_error"] = type(error).__name__
        result["nodes"][path] = entry
    u.adt.pmgr_init()
    pmgr = u.adt["/arm-io/pmgr"]
    gate_ids = {gate for node in result["nodes"].values()
                for key in ("clock-gates", "power-gates")
                for gate in node["properties"].get(key, [])}
    devices = {u.adt.pmgr_dev_get_id(d): d for d in pmgr.devices}
    pending = list(gate_ids)
    while pending:
        device = devices.get(pending.pop())
        if device is None:
            continue
        for parent in u.adt.pmgr_dev_get_parents(device):
            if parent and parent not in gate_ids:
                gate_ids.add(parent)
                pending.append(parent)
    result["power_registers"] = []
    for d in pmgr.devices:
        device_id = u.adt.pmgr_dev_get_id(d)
        if device_id not in gate_ids and not any(tag in str(d.name).lower() for tag in tags):
            continue
        entry = {"name": d.name, "id": device_id, "flags": plain(d.flags),
                 "parents": list(u.adt.pmgr_dev_get_parents(d))}
        if not d.flags.no_ps:
            entry["address"] = hex(u.adt.pmgr_dev_get_addr(d))
            entry["block"] = [hex(v) for v in u.adt.pmgr_dev_get_block(d)]
        result["power_registers"].append(entry)
    if proxy is not None:
        # Only the measured internal DCP is admitted. Confirm its physical
        # power-state register is on before reading the ASC status bank.
        dcp = u.adt["/arm-io/dcp"]
        if dcp.get_reg(0) != (0x286e00000, 0x88000):
            raise ValueError("Unexpected M5 internal DCP aperture")
        gate = next((d for d in result["power_registers"] if d["id"] == 183), None)
        if gate is None or gate.get("address") != "0x380710000":
            raise ValueError("Unexpected M5 DISP_CPU gate")
        power = proxy.read32(0x380710000)
        registers = {"power_state": hex(power), "cpu_registers_read": False}
        if power & 0xff == 0xff:
            before = [proxy.read32(0x286e00044), proxy.read32(0x286e00048)]
            after = [proxy.read32(0x286e00044), proxy.read32(0x286e00048)]
            registers.update(cpu_registers_read=True,
                             cpu_control=hex(before[0]), cpu_status=hex(before[1]),
                             stable=before == after)
        else:
            registers["reason"] = "DISP_CPU is not at the fully-on power state"
        result["register_snapshot"] = registers
        result["source"] = "live m1n1 ADT and gated read-only internal DCP status"
    if mappings:
        status = result.get("register_snapshot", {})
        if not status.get("cpu_registers_read") or not status.get("stable"):
            raise ValueError("DART inspection requires a stable powered DCP snapshot")
        from m1n1.hw.dart import DART
        chosen = u.adt["/chosen"]
        dram_base = int(chosen.getprop("dram-base"))
        dram_end = dram_base + int(chosen.getprop("dram-size"))

        class PageTableReader:
            def readmem(self, address, size):
                if size != 0x4000 or not dram_base <= address < address + size <= dram_end:
                    raise ValueError("DART page-table read outside physical RAM")
                return u.iface.readmem(address, size)

        streams = [("/arm-io/dart-dcp", 23, 0x286340000),
                   ("/arm-io/dart-disp0", 0, 0x286300000),
                   ("/arm-io/dart-disp0", 16, 0x286300000)]
        result["dart_streams"] = []
        for node_path, stream, expected_base in streams:
            if u.adt[node_path].get_reg(0)[0] != expected_base:
                raise ValueError("Unexpected M5 display DART")
            # Construction and iotranslate only read registers/page tables.
            # Never call initialize, iomap_at or invalidate_streams here.
            dart = DART.from_adt(u, node_path)
            dart.dart.iface = PageTableReader()
            item = {"path": node_path, "stream": stream,
                    "enabled": bool(dart.dart.enabled_streams & (1 << stream)),
                    "tcr": hex(dart.dart.regs.TCR[stream].val),
                    "ttbr": hex(dart.dart.regs.TTBR[stream].val),
                    "reg_protect": hex(dart.dart.regs.REG_PROTECT.val),
                    "reg_protect_lock": hex(dart.dart.regs.REG_PROTECT_LOCK.val)}
            if stream == 23:
                raw = bytes(u.adt["/arm-io/dcp/iop-dcp-nub"].getprop("segment-ranges"))
                segments = list(struct.iter_unpack("<QQQII", raw))
                if not segments or sum(s[3] for s in segments) > 128 * 1024 * 1024:
                    raise ValueError("Unexpected DCP segment inventory size")
                item["segments"] = []
                for physical, virtual, remap, size, flags in segments:
                    if not size or not dram_base <= physical < physical + size <= dram_end:
                        raise ValueError("DCP firmware descriptor outside physical RAM")
                    ranges = dart.iotranslate(stream, remap, size)
                    cursor, matched = physical, True
                    for address, length in ranges:
                        matched &= address == cursor
                        cursor += length
                    item["segments"].append({
                        "physical": hex(physical), "iova": hex(virtual), "remap": hex(remap),
                        "size": hex(size), "flags": flags,
                        "matches_descriptor": matched and cursor == physical + size,
                        "translations": [[None if a is None else hex(a), hex(n)] for a, n in ranges],
                    })
            result["dart_streams"].append(item)
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    name = "m5-display-topology-" + time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + ".json"
    path = output / name
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as stream:
        json.dump(result, stream, indent=2)
        stream.write("\n")
    print("M5_DISPLAY_ADT_SAVED", path, "nodes=" + str(len(result["nodes"])), flush=True)
    return path


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=root / "logs")
    parser.add_argument("--registers", action="store_true",
                        help="Also read the measured power gate and powered internal DCP CPU status")
    parser.add_argument("--mappings", action="store_true",
                        help="Also read inherited DART state and walk DCP firmware mappings")
    args = parser.parse_args()
    device = "/dev/cu.kis-01100000-ch-0"
    owners = subprocess.run(["lsof", "-t", device, device.replace("/cu.", "/tty.")],
                            capture_output=True, text=True)
    if owners.stdout.strip():
        raise RuntimeError("M5 serial connection already has an owner")
    os.environ["M1N1DEVICE"] = device
    os.environ["M1N1TIMEOUT"] = "10"
    sys.path.insert(0, str(root / "m1n1/proxyclient"))
    runpy.run_path(str(root / "kis-transport.py"))["install"]()
    from m1n1.proxy import UartInterface, M1N1Proxy
    from m1n1.proxyutils import ProxyUtils, bootstrap_port
    iface = UartInterface()
    p = M1N1Proxy(iface)
    if p.get_chipid() != 0x8142:
        raise ValueError("Refusing non-M5 target")
    bootstrap_port(iface, p)
    capture_display_topology(ProxyUtils(p), args.output,
                             p if args.registers or args.mappings else None, args.mappings)


if __name__ == "__main__":
    main()
