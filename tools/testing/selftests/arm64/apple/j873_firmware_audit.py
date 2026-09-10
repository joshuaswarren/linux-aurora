#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Audit J873 firmware topology against this kernel's driver match tables.

Input is AuroraSilicon/tools/inventory-j873-adt.py output. This deliberately
does not generate a boot DT from logical CPU IDs or invent hardware fallbacks.
A match-table hit is only a source-wiring observation, never hardware proof.
"""
import argparse
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[5]
RAW_HASH = "3252a3f13dc025ffa8551c7a5da6a71501f76ea6c5d38aa35390f9268a6dc378"
DEVICES = {
    "uart": ("/arm-io/uart0", "uart-1,samsung", "0x331200000", "0x4000"),
    "aic": ("/arm-io/aic", "aic,3", "0x301000000", "0x1cc000"),
    "pmgr2": ("/arm-io/pmgr-child", "pmgr2,t8152", "0x300700000", "0xd0000"),
    "dart": ("/arm-io/dart-usb0", "dart,gen3", "0x403020000", "0xc000"),
    "ans": ("/arm-io/ans", "iop,ascwrap-v8", "0x321600000", "0x74000"),
}
DRIVERS = {
    "aic": "drivers/irqchip/irq-apple-aic.c",
    "dart": "drivers/iommu/apple-dart.c",
    "pmgr": "drivers/pmdomain/apple/pmgr-pwrstate.c",
}


def audit(inventory, root):
    if inventory.get("model") != "Mac18,5" or inventory.get("raw_adt_sha256") != RAW_HASH:
        raise ValueError("expected verified J873g 26A428 inventory")
    nodes = inventory["nodes"]
    if len(nodes) != 525 or inventory["node_count"] != 525 or {n["node_index"] for n in nodes} != set(range(525)):
        raise ValueError("incomplete or duplicate ADT node inventory")
    for path, compatible, base, size in DEVICES.values():
        selected = [n for n in nodes if n["path"] == path]
        if len(selected) != 1:
            raise ValueError(f"missing or ambiguous hardware node {path}")
        n = selected[0]
        reg = next(r for r in n["registers"] if r["index"] == 0)
        if compatible not in n["compatible"] or (reg["base"], reg["size"]) != (base, size):
            raise ValueError(f"firmware identity/register mismatch at {path}")
    cpus = [n for n in nodes if n["path"].startswith("/cpus/")]
    if len(cpus) != 12:
        raise ValueError("expected 12 firmware CPU descriptors")
    kinds = {kind: sum(n.get("cluster-type") == kind for n in cpus) for kind in ["E", "P", "M"]}
    if kinds != {"E": 6, "P": 2, "M": 4}:
        raise ValueError("unexpected CPU descriptor topology")
    matches = {}
    for name, path in DRIVERS.items():
        text = (root / path).read_text()
        matches[name] = sorted(set(re.findall(r'\.compatible\s*=\s*"([^"]+)"', text)))
    return dict(firmware_inventory_verified=True, hardware_tested=False,
                linux_boot_supported=False, cpu_descriptors=kinds, driver_match_tables=matches,
                blockers=[
                    "Measure boot MPIDR and every secondary affinity; ADT reg is a logical ID.",
                    "Validate AIC3 event aperture, capability registers and timer routing on T8152.",
                    "Implement PMGR2 PMS service transport; PMGR1 MMIO gates are insufficient.",
                    "Implement Gen3 DART ownership, page tables and TLBI; macOS delegates to SPTM.",
                    "Implement ASCWrap-v8 postoffice mailboxes before ANS/RTKit storage bring-up.",
                    "Capture runtime memory reservations and CPU ID registers before a boot DT.",
                ])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inventory", type=Path)
    parser.add_argument("--kernel-root", type=Path, default=ROOT)
    args = parser.parse_args()
    print(json.dumps(audit(json.loads(args.inventory.read_text()), args.kernel_root), indent=2))


if __name__ == "__main__":
    main()
