"""Check read-only attachment after PIODMA-based native enumeration."""
from pathlib import Path
import runpy
import sys
import tempfile
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / 'm1n1/proxyclient'))
from m1n1.adt import load_adt

Journal = runpy.run_path(str(ROOT / 'n1-trace.py'))['N1PCIJournal']
read_context = runpy.run_path(str(ROOT / 'read-n1-context.py'))['read_context']
from m1n1.trace import TraceMode
source = max((ROOT / 'logs/adt-snapshots').glob('*.adt'), key=lambda p: p.stat().st_mtime)
adt = load_adt(source.read_bytes())
apcie = adt['/arm-io/apcie']
window = next(w for w in apcie.ranges if (int(w.bus_addr[0]) >> 24) & 3 in (2, 3))
bus = (int(window.bus_addr[2]) << 32) | int(window.bus_addr[1])
bus = (max(bus, 0x10000) + 0xffff) & ~0xffff
assert bus + 0x10000 <= ((int(window.bus_addr[2]) << 32) | int(window.bus_addr[1])) + int(window.size)
ecam = int(apcie.get_reg(0)[0]) + 0x100000
words = {ecam: 0x1900106b, ecam + 4: 6, ecam + 0x10: (bus & 0xffffffff) | 4,
         ecam + 0x14: bus >> 32}

for case in ('valid', 'missing', 'disabled', 'unassigned', 'misaligned', 'not-paused',
             'prearm', 'prearm-missing', 'prearm-unassigned'):
    reads, maps = [], []
    data = dict(words)
    if case in ('missing', 'prearm-missing'): data[ecam] = 0xffffffff
    if case == 'disabled' or case.startswith('prearm'): data[ecam + 4] = 0
    if case in ('unassigned', 'prearm-unassigned'): data[ecam + 0x10] = 0xffffffff
    if case == 'misaligned': data[ecam + 0x10] += 0x1000
    def read32(address):
        reads.append(address)
        return data[address]
    # No hardware write method exists: any attempt to size a BAR fails.
    hv = SimpleNamespace(adt=adt, j813_kernelcache_verified=True,
                         ctx=None if case == 'not-paused' else object(),
                         p=SimpleNamespace(read32=read32),
                         clear_tracers=lambda ident: None,
                         add_tracer=lambda *a, **k: maps.append((a, k)))
    with tempfile.TemporaryDirectory() as directory:
        journal = Journal(hv, Path(directory) / 'trace.jsonl')
        try:
            result = journal.attach_enumerated(0, 0x1900, 0x10000,
                                               prearm=case.startswith('prearm'))
        except RuntimeError:
            assert case not in ('valid', 'prearm')
            assert not maps
        else:
            assert case in ('valid', 'prearm')
            assert result[1] == 0x10000 and len(maps) == 1
            assert maps[0][0][2] == TraceMode.ASYNC
            assert reads == [ecam, ecam + 4, ecam + 0x10, ecam + 0x14, ecam]
            if case == 'prearm':
                hv.u = SimpleNamespace(adt=adt)
                hv.n1_journal = journal
                try:
                    read_context(hv, 0, 1)
                except RuntimeError as error:
                    assert 'memory decoding' in str(error)
                else:
                    raise AssertionError('Prearmed BAR accepted for active context read')
                assert reads[-2:] == [ecam, ecam + 4]
        finally:
            journal.stop()
    assert all(address in words for address in reads)
print('N1 attach checks passed: passive prearm uses configuration reads only; live context rejects disabled BARs')

# A ROM-to-runtime transition changes product identity at the same BDF.
# Native registry proves runtime control=1901, alpha=1902, beta=1903.
# Control must use 8050, alpha c, and beta must never be decoded as alpha.
for product in (0x1901, 0x1902, 0x1903):
    reads, maps = [], []
    data = {**words, ecam: (product << 16) | 0x106b}
    def read32(address):
        reads.append(address)
        return data[address]
    hv = SimpleNamespace(adt=adt, u=SimpleNamespace(adt=adt),
                         j813_kernelcache_verified=True, ctx=object(),
                         p=SimpleNamespace(read32=read32),
                         clear_tracers=lambda ident: None,
                         add_tracer=lambda *a, **k: maps.append((a, k)))
    with tempfile.TemporaryDirectory() as directory:
        journal = Journal(hv, Path(directory) / 'trace.jsonl')
        hv.n1_journal = journal
        bar, _ = journal.attach_enumerated(0, product, 0x10000)
        status_reg = 0x8050 if product == 0x1901 else 0xc
        data[bar + status_reg] = 0
        reads.clear()
        try:
            read_context(hv, 0, 1)
        except RuntimeError as error:
            if product in (0x1901, 0x1902):
                assert 'handshake' in str(error)
                assert reads[-1] == bar + status_reg
                assert bar + (0xc if product == 0x1901 else 0x8050) not in reads
            else:
                assert 'not a control or alpha' in str(error)
                assert not reads
        else:
            raise AssertionError('Invalid IPC status or product accepted')
        finally:
            journal.stop()
print('N1 runtime identity checks passed: control 1901 uses 8050; alpha 1902 uses c; beta 1903 is rejected')
