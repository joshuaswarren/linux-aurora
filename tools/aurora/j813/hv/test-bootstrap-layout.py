"""Reproduce the guest09/26 address-layout failures without hardware."""
import ast
from pathlib import Path

source = Path(__file__).with_name('m1n1') / 'proxyclient/tools/sptm/seed.py'
tree = ast.parse(source.read_text())
names = {'PAGE_SIZE', 'L2_SPAN', 'PHYSMAP_BASE', 'M5_PHYSMAP_BASE',
         'M5_KERNEL_HEAP_BASE'}
nodes = [node for node in tree.body
         if (isinstance(node, ast.FunctionDef) and node.name in
             {'align_up', 'bootstrap_virtual_layout'}) or
         (isinstance(node, ast.Assign) and any(
             isinstance(target, ast.Name) and target.id in names
             for target in node.targets))]
namespace = {}
exec(compile(ast.Module(body=nodes, type_ignores=[]), str(source), 'exec'), namespace)
layout = namespace['bootstrap_virtual_layout']
kernel_base = 0xfffffe0000000000
heap_base = 0xfffffe1000000000
kc = (0xfffffe0007004000, 0xfffffe000c800000)
managed_start, managed_end = 0x10017bd4000, 0x103dc600000
size = managed_end - managed_start

# Exact guest09 compact-pointer encoding loses the high offset bit.
def compact_roundtrip(va):
    return kernel_base | (((va >> 6) & 0x7fffffff) << 6)

assert compact_roundtrip(0xfffffe43c749d2c0) == 0xfffffe03c749d2c0
# Exact guest26 copyout source is classified as a zone object by native XNU.
zone_min, zone_max = 0xfffffe1018000000, 0xfffffe3618000000
old_source = 0xfffffe13a5cdc000
assert zone_min <= old_source < zone_max
base, end, dynamic, extent = layout(0x8142, *kc, size)
source_pa = 0x103bd8b0000
new_source = base + source_pa - managed_start
assert not zone_min <= new_source < zone_max
assert new_source - base + managed_start == source_pa
assert kc[1] <= base < end <= dynamic < dynamic + extent <= heap_base
for va in (base, end - 64, dynamic, dynamic + extent - 64, new_source):
    assert compact_roundtrip(va) == va, hex(va)
# Supported smaller/larger RAM geometries must retain both contracts.
for gib in (8, 16, 24, 32):
    b, e, d, n = layout(0x8142, *kc, gib << 30)
    assert b < e <= d < d + n <= heap_base
    assert compact_roundtrip(d + n - 64) == d + n - 64
for bad_kc, bad_size in ((kc, 64 << 30), ((kc[0], base + 0x4000), size),
                         (kc, 0), (kc, size + 1)):
    try:
        layout(0x8142, *bad_kc, bad_size)
    except RuntimeError:
        pass
    else:
        raise AssertionError('Invalid bootstrap layout accepted')
# The Neo base and its following early arena retain the previous geometry.
b, e, d, n = layout(0x8140, *kc, size)
assert b == 0xfffffe4000000000 and e == b + size
assert d == (e + namespace['L2_SPAN'] - 1) & -namespace['L2_SPAN']
print(f'M5 layout passed: PAPT {base:#x}..{end:#x}, arena {dynamic:#x}..{dynamic+extent:#x}')
print(f'Guest26 physical buffer becomes {new_source:#x}; zone exclusion, compact pointers, rejection and Neo checks passed')
