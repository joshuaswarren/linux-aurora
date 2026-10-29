#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0
"""Tests for tools/asahi/mtp_dt_from_adt.py and the MTP nodes it feeds.

Always run: the committed T6050 MTP nodes follow the J813 pattern (same relations between the windows).
Run when AURORA_ADT_DIR (DeviceTree.<board>.adt files) and M1N1_PY_DIR (directory holding the m1n1 python
package) are set: the derivation on the J813 ADT gives the numbers committed in the J813 file, and the
derivation on the j714c ADTs of macOS 27.0 and 27.2 regenerates t6050-j714c-peripherals.dtsi byte for byte.  Without the ADTs those
two tests are reported as skipped; the ADTs are Apple data and are not stored in the tree.
"""
import importlib.util
import os
import re
import sys
import unittest
from pathlib import Path

TREE = Path(__file__).resolve().parent.parent.parent
DTS = TREE / "arch/arm64/boot/dts/apple"
spec = importlib.util.spec_from_file_location("mtp_dt_from_adt", Path(__file__).with_name("mtp_dt_from_adt.py"))
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)


def mtp_block(text):
    """Window numbers of the MTP nodes in one dtsi: address cells joined into 64-bit numbers."""
    def cells(s):
        return [int(x, 16) for x in re.findall(r"0x[0-9a-fA-F]+|\b\d+\b", s)]

    def reg(label):
        m = re.search(label + r":[^{]*\{[^}]*?reg = <([^;]*?)>;", text, re.S)
        c = cells(m.group(1))
        return [((c[i] << 32) | c[i + 1], c[i + 3]) for i in range(0, len(c), 4)]

    m = re.search(r"mtp: mtp@[0-9a-f]+ \{[^}]*?reg = <([^;]*?)>;", text, re.S)
    c = cells(m.group(1))
    asc = [((c[i] << 32) | c[i + 1], c[i + 3]) for i in range(0, len(c), 4)]
    ranges = cells(re.search(r"mtp_dockchannel:.*?ranges = <([^>]*)>", text, re.S).group(1))
    return {
        "asc": asc[0], "sram": asc[1], "mailbox": reg("mtp_mbox")[0], "dart": reg("mtp_dart")[0],
        "dock_irq": reg("mtp_dockchannel")[0], "dock_range": ((ranges[1] << 32) | ranges[2], ranges[3]),
    }


class Pattern(unittest.TestCase):
    def test_t6050_follows_the_j813_window_relations(self):
        for name in ("t8142-j813-peripherals.dtsi", "t6050-j714c-peripherals.dtsi"):
            b = mtp_block((DTS / name).read_text())
            asc = b["asc"][0]
            self.assertEqual(b["sram"], (asc + 0x600000, 0x100000), name)
            self.assertEqual(b["mailbox"], (asc + 0x8000, 0x4000), name)
            self.assertEqual(b["dart"], (asc + 0x200000, 0xC000), name)
            self.assertEqual(b["dock_irq"], (asc + 0x514000, 0x1000), name)
            self.assertEqual(b["dock_range"], (asc + 0x528000, 0x20000), name)


ADT_DIR = os.environ.get("AURORA_ADT_DIR")
M1N1 = os.environ.get("M1N1_PY_DIR")


@unittest.skipUnless(ADT_DIR and M1N1, "set AURORA_ADT_DIR and M1N1_PY_DIR to run against real ADTs")
class AgainstAdt(unittest.TestCase):
    def values(self, board):
        return gen.derive(gen.load(os.path.join(ADT_DIR, "%s.adt" % board), M1N1))

    def test_j813_derivation_matches_the_committed_j813_nodes(self):
        v = self.values("DeviceTree.j813ap")
        b = mtp_block((DTS / "t8142-j813-peripherals.dtsi").read_text())
        self.assertEqual(tuple(v["asc"]), b["asc"])
        self.assertEqual(tuple(v["sram"]), b["sram"])
        self.assertEqual(tuple(v["mailbox"]), b["mailbox"])
        self.assertEqual(tuple(v["dart"]), b["dart"])
        self.assertEqual(tuple(v["dock_irq_reg"]), b["dock_irq"])
        self.assertEqual(tuple(v["dock_ranges"]), b["dock_range"])
        text = (DTS / "t8142-j813-peripherals.dtsi").read_text()
        text = text[text.index("mtp: mtp@"):]
        irqs = [int(x) for x in re.findall(r"AIC_IRQ (\d+) IRQ_TYPE", text)]
        i = v["mailbox_irqs"]
        self.assertEqual(irqs[:4], [i["send-empty"], i["send-not-empty"], i["recv-empty"], i["recv-not-empty"]])
        self.assertEqual(irqs[4:6], [v["dart_irq"], v["dock_irq"]])

    def test_t6050_dtsi_is_what_the_generator_renders_from_both_adts(self):
        want = (DTS / "t6050-j714c-peripherals.dtsi").read_text()
        for adt in ("DeviceTree.j714cap", "v272-DeviceTree.j714cap"):
            self.assertEqual(gen.render(self.values(adt), "j714c"), want, adt)


if __name__ == "__main__":
    unittest.main(verbosity=2)
