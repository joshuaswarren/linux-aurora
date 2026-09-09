"""Read resident M5 counters after a host controller exits; never resume it."""
import os
from pathlib import Path
import struct
import sys

ROOT = Path(__file__).resolve().parent
os.environ['M1N1DEVICE'] = '/dev/cu.kis-01100000-ch-0'
os.environ['M1N1TIMEOUT'] = '15'
os.environ['M1N1_DIR'] = str(ROOT / 'm1n1')
os.environ['USE_CLANG'] = '0'
os.environ['TOOLCHAIN'] = '/opt/homebrew/bin/'
os.environ['ARCH'] = 'aarch64-elf-'
sys.path.insert(0, str(ROOT / 'm1n1/proxyclient'))
from m1n1.proxy import UartInterface, M1N1Proxy
from m1n1.proxyutils import bootstrap_port
from tools.sptm.seed import _m1n1_symbol_offset

iface = UartInterface()
p = M1N1Proxy(iface)
try:
    bootstrap_port(iface, p)
    if p.get_chipid() != 0x8142 or p.iodev_whoami() != 1:
        raise RuntimeError('Wrong target or transport')
    base = p.get_base()
    sk = base + _m1n1_symbol_offset('g_sk')
    count = p.read64(sk + 2728)
    print('M5_RESIDENT_SPTM_CALLS', count)
    mte = struct.unpack('<7Q', iface.readmem(
        base + _m1n1_symbol_offset('g_sptm_mte'), 56))
    print('M5_RESIDENT_MTE', dict(zip(
        ('dram_base', 'storage_base', 'storage_end', 'tag_calls',
         'untag_calls', 'tagged_pages', 'rejected'), mte)))
    ring = struct.unpack('<320Q', iface.readmem(sk + 168, 2560))
    print('M5_RESIDENT_RECENT_CALLS', [
        [hex(v) for v in ring[(i % 64) * 5:(i % 64 + 1) * 5]]
        for i in range(max(0, count - 8), count)])
finally:
    iface.dev.close()
