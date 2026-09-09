"""Exercise the production M5 CQ patch against the exact native kernel bytes."""
import array
import ast
import hashlib
import os
from pathlib import Path
import struct
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent
source = ROOT / 'm1n1/proxyclient/m1n1/hv/__init__.py'
tree = ast.parse(source.read_text())
branch = next(n for n in ast.walk(tree) if isinstance(n, ast.If)
              and any(isinstance(c, ast.Constant) and
                      c.value == 'M5_NVME_CQ_DEPTH_64' for c in ast.walk(n))
              and isinstance(n.test, ast.Compare)
              and ast.unparse(n.test) == 'int(self.u.adt[\'/chosen\'].chip_id) == 33090')
wrapper = ast.parse('def run(self, a, seg_vmaddr, size, segname):\n    pass\n')
wrapper.body[0].body = [branch]
ast.fix_missing_locations(wrapper)
ns = {'os': os}
exec(compile(wrapper, str(source), 'exec'), ns)
run = ns['run']
kernel = (ROOT / 'kernel/25G76__Mac17,3/kernelcache.release.Mac17,2_3_4').read_bytes()
assert hashlib.sha256(kernel).hexdigest() == (
    '43263b4f4b7cd29cb0cc487ba944b5bf0a8f3b24b8b1cad875b796d9af026189')
offset = 32
segments = []
for _ in range(struct.unpack_from('<I', kernel, 16)[0]):
    command, length = struct.unpack_from('<II', kernel, offset)
    if command == 0x19 and kernel[offset + 8:offset + 24].rstrip(b'\0') == b'__TEXT_EXEC':
        va, _, fileoff, size = struct.unpack_from('<4Q', kernel, offset + 24)
        segments.append((va, kernel[fileoff:fileoff + size]))
    offset += length
assert len(segments) == 1
va, original = segments[0]
hv = SimpleNamespace(u=SimpleNamespace(adt={
    '/chosen': SimpleNamespace(chip_id=0x8142)}))
with patch.dict(os.environ, {'M5_NVME_CQ_DEPTH_64': '1'}):
    changed = run(hv, array.array('I', original), va, len(original), '__TEXT_EXEC')
    before, after = array.array('I', original), array.array('I', changed)
    differences = [i for i, (a, b) in enumerate(zip(before, after)) if a != b]
    assert differences == [(0xfffffe000a9ecc14 - va) // 4]
    assert after[differences[0]] == 0x5280080a
    for address in (0xfffffe000a9ecc14, 0xfffffe000a9ecc20):
        bad = array.array('I', original)
        bad[(address - va) // 4] ^= 1
        try:
            run(hv, bad, va, len(original), '__TEXT_EXEC')
        except RuntimeError as e:
            assert 'signature mismatch' in str(e)
        else:
            raise AssertionError('Altered CQ code was accepted')
    hv.u.adt['/chosen'].chip_id = 0x8140
    untouched = array.array('I', original)
    assert run(hv, untouched, va, len(original), '__TEXT_EXEC') is None
    assert untouched.tobytes() == original
    hv.u.adt['/chosen'].chip_id = 0x8142
with patch.dict(os.environ, {'M5_NVME_CQ_DEPTH_64': '0'}):
    assert run(hv, array.array('I', original), va, len(original), '__TEXT_EXEC') == original
with patch.dict(os.environ, {'M5_NVME_CQ_DEPTH_64': 'invalid'}):
    try:
        run(hv, array.array('I', original), va, len(original), '__TEXT_EXEC')
    except RuntimeError as e:
        assert 'must be 0 or 1' in str(e)
    else:
        raise AssertionError('Invalid control was accepted')
# The measured capability count and programmed count diverge at the boundary.
assert (0x20f0010040 & 0xffff) + 1 == 65
assert (0x003f003f & 0xffff) + 1 == 64
assert (63 + 1) % 64 == 0 and (63 + 1) % 65 == 64
print('M5 NVMe CQ: exact one-opcode patch, signature rejection, chip isolation, and A/B control PASS')
