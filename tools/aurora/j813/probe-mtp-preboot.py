#!/usr/bin/env python3
"""Bounded J813 MTP boot comparison; SRAM evidence stays private.

Run only on a fresh RAM-chainloaded m1n1. Reboot afterward: this probe stops
the MTP CPU but deliberately leaves its DMA buffers allocated until reset.
It never consumes DockChannel input or changes resident boot images.
"""
import hashlib
import os
from pathlib import Path
import runpy
import signal
import sys
import time

os.umask(0o077)
root = Path(__file__).resolve().parent
os.environ['M1N1DEVICE'] = '/dev/cu.kis-01100000-ch-0'
os.environ['M1N1TIMEOUT'] = '10'
sys.path.insert(0, str(root / 'm1n1/proxyclient'))
runpy.run_path(str(root / 'kis-transport.py'))['install']()
from m1n1.proxy import UartInterface, M1N1Proxy
from m1n1.proxyutils import ProxyUtils, bootstrap_port
from m1n1.hw.dart import DART
from m1n1.fw.asc import StandardASC
from m1n1.fw.asc.crash import ASCCrashLogEndpoint


def private_save(name, data):
    path = root / 'logs' / name
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
    print('PRIVATE_CAPTURE', path.name, len(data), hashlib.sha256(data).hexdigest(), flush=True)


class PrivateCrash(ASCCrashLogEndpoint):
    def handle_crashed(self, msg):
        size = 0x1000 * msg.SIZE
        if not 0 < size <= 0x100000:
            raise RuntimeError('Unexpected crash buffer size')
        private_save(stamp + '-crash.bin', self.asc.ioread(msg.DVA, size))
        raise RuntimeError('MTP firmware crashed during preboot comparison')


class ProbeASC(StandardASC):
    ENDPOINTS = {1: PrivateCrash}

    def ioalloc(self, size):
        if not 0 < size <= 0x100000:
            raise RuntimeError('Unexpected RTKit buffer allocation')
        address = self.u.memalign(0x4000, size)
        self.iface.writemem(address, bytes(size))
        return address, self.iomap(address, size)


stamp = 'mtp-preboot-' + time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())
iface = UartInterface()
p = M1N1Proxy(iface)
assert p.get_chipid() == 0x8142
bootstrap_port(iface, p)
u = ProxyUtils(p)
assert u.adt.model == 'Mac17,3'
assert u.adt['/arm-io/mtp'].get_reg(0) == (0x394600000, 0x88000)
assert u.adt['/arm-io/dart-mtp'].get_reg(0) == (0x394800000, 0xc000)
assert u.adt['/arm-io/dart-mtp'].get_reg(1) == (0x394810000, 0x4000)
assert not p.read32(0x394600044) & 0x10, 'MTP already running; fresh boot required'
private_save(stamp + '-sram-before.bin', iface.readmem(0x394c00000, 0x100000))
for name in ('mtp', 'dart-mtp', 'dockchannel-mtp'):
    node = u.adt['/arm-io/' + name]
    if node._properties.get('clock-gates'):
        assert p.pmgr_adt_power_enable('/arm-io/' + name) == 0
assert p.dapf_init('/arm-io/dart-mtp') == 0
if u.adt['/arm-io/dart-mtp']._properties.get('clock-gates'):
    assert p.pmgr_adt_power_enable('/arm-io/dart-mtp') == 0
dart = DART.from_adt(u, '/arm-io/dart-mtp', iova_range=(0x02000000, 0x12000000))
dart.initialize()
staging = u.memalign(0x4000, 0x100000)
iface.writemem(staging, bytes(0x100000))
dart.iomap_at(0, 0x01800000, staging, 0x100000)
dart.invalidate_streams(1)
mtp = ProbeASC(u, 0x394600000, dart, stream=0)
mtp.allow_phys = True
mtp.verbose = 3


def expired(signum, frame):
    raise TimeoutError('MTP preboot comparison deadline expired')


signal.signal(signal.SIGALRM, expired)
signal.alarm(45)
try:
    mtp.start()
    end = time.monotonic() + 10
    while time.monotonic() < end:
        mtp.work()
        count = p.read32(0x394b3402c)
        if count:
            print('MTP_PREBOOT_READY', 'iop', hex(mtp.mgmt.iop_power_state),
                  'ap', hex(mtp.mgmt.ap_power_state), 'queued_bytes', count, flush=True)
            break
    else:
        raise TimeoutError('No DockChannel INIT data')
finally:
    signal.alarm(0)
    mtp.shutdown()
    private_save(stamp + '-sram-after.bin', iface.readmem(0x394c00000, 0x100000))
    iface.dev.close()
