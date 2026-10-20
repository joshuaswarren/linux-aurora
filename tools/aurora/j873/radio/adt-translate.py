"""adt-translate.py ADT.bin PROXYCLIENT_DIR: translate every address the APCIe device tree uses through the live ADT /arm-io ranges (m1n1 adt.py get_reg).

Receipt for the arm-io translation: the first /arm-io range maps child bus 0..0x400000000 to parent 0x200000000, so child addresses below 0x400000000
gain +0x200000000; ECAM (0x1cb0000000) and the PCI windows (0xbc0000000, 0xb80000000) are identity ranges and do not change.
The aic and smc rows are nodes the existing t8152-j873.dts already uses at their physical addresses: they check the method.
"""
import sys

sys.path.insert(0, sys.argv[2])
from m1n1.adt import load_adt

adt = load_adt(open(sys.argv[1], "rb").read())
print("/arm-io ranges (bus -> parent, size):")
for r in adt["/arm-io"].ranges:
    print("  %#x -> %#x  %#x%s" % (r.bus_addr, r.parent_addr, r.size, "" if r.bus_addr != r.parent_addr else "  (identity)"))


def rows(path, only=None):
    node = adt[path]
    for i, r in enumerate(node.reg):
        if only is not None and i not in only:
            continue
        phys, size = node.get_reg(i)
        print("  %-26s reg[%d] bus %#x -> phys %#x size %#x" % (path, i, r.addr, phys, size))


print("method check against t8152-j873.dts:")
rows("/arm-io/aic", only=[0])
rows("/arm-io/smc", only=[0])
print("APCIe:")
rows("/arm-io/gpio0")
rows("/arm-io/dart-apcie0")
rows("/arm-io/dart-apcie2")
rows("/arm-io/apcie0-piodma")
rows("/arm-io/apcie2-piodma")
rows("/arm-io/apcie")
