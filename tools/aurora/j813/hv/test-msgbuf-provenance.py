"""EL0 log reads require a prior static mapping from the same controller."""
import json
from pathlib import Path
import runpy
import struct
import sys
import tempfile
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / 'm1n1/proxyclient'))
helper = runpy.run_path(str(ROOT / 'read-msgbuf.py'))
save = helper['save_msgbuf']
kernel, physical = 0xfffffe0007000000, 0x10010000000
header_va, buffer_va = kernel + 0x1000, kernel + 0x2000
size = 64
raw = b'new\n' + bytes(size - 8) + b'old\n'
helper['xnutools'].find_msgbuf_va = lambda macho: header_va
prior = dict(static_buffer=True, header_va=hex(header_va), buffer_va=hex(buffer_va),
             guest_base=hex(physical), size=size)

for case in ('valid', 'no-prior', 'other-guest', 'dynamic', 'el1', 'moved-header',
             'moved-buffer', 'changed-size'):
    calls = []
    proof = dict(prior)
    if case == 'other-guest': proof['guest_base'] = hex(physical + 0x4000)
    if case == 'dynamic': proof['static_buffer'] = False
    header = struct.pack('<4IQ', 0x63061, size + (case == 'changed-size'), 4, 0,
                         buffer_va + (0x4000 if case == 'moved-buffer' else 0))
    def readmem(address, length):
        calls.append((address, length))
        if address == physical + 0x1000 and length == 24:
            return header
        assert address == physical + 0x2000 and length == size
        return raw
    hv = SimpleNamespace(
        u=SimpleNamespace(adt={'/chosen': SimpleNamespace(chip_id=0x8142)}),
        ctx=SimpleNamespace(spsr=SimpleNamespace(M=5 if case == 'el1' else 0)),
        j813_kernelcache_verified=True, guest_base=physical,
        macho=SimpleNamespace(vmin=kernel, vmax=kernel + 0x100000),
        p=SimpleNamespace(hv_translate=lambda *args: 0x10020000000 if case == 'moved-header' else 0),
        iface=SimpleNamespace(readmem=readmem),
        m5_verified_msgbuf=None if case == 'no-prior' else proof)
    with tempfile.TemporaryDirectory() as directory:
        save.__globals__['__file__'] = str(Path(directory) / 'read-msgbuf.py')
        try:
            save(hv)
        except RuntimeError:
            assert case != 'valid'
            assert len(calls) <= (1 if case in ('moved-buffer', 'changed-size') else 0)
        else:
            assert case == 'valid'
            path = next((Path(directory) / 'logs/msgbuf').glob('*.json'))
            assert json.loads(path.read_text())['physical_fallback_from_verified_static']
            assert path.with_suffix('.raw').read_bytes() == raw
            assert path.with_suffix('.txt').read_text() == 'old\nnew\n'
print('Msgbuf provenance checks passed: verified static EL0 read and stale/dynamic/moved mapping rejection')
