#!/usr/bin/env python3
"""Run the isolated M5 macOS qualification guest through the scoped KIS link."""
import hashlib
import os
from pathlib import Path
import runpy
import signal
import sys
import traceback

root = Path(__file__).resolve().parent
source = root / 'm1n1'
kernel = root / 'kernel/25G76__Mac17,3/kernelcache.release.Mac17,2_3_4'
expected = '43263b4f4b7cd29cb0cc487ba944b5bf0a8f3b24b8b1cad875b796d9af026189'
if hashlib.sha256(kernel.read_bytes()).hexdigest() != expected:
    raise RuntimeError('M5 kernel hash mismatch')
os.environ['M1N1DEVICE'] = '/dev/cu.kis-01100000-ch-0'
os.environ['M1N1TIMEOUT'] = '15'
os.environ['M1N1SKIP_PANIC_COUNTER'] = '1'
os.environ['M1N1_DIR'] = str(source)
os.environ['USE_CLANG'] = '0'
os.environ['TOOLCHAIN'] = '/opt/homebrew/bin/'
os.environ['ARCH'] = 'aarch64-elf-'
sys.path.insert(0, str(source / 'proxyclient'))
runpy.run_path(str(root.parent / 'kis-transport.py'))['install']()

import m1n1.hv as hv_module
from m1n1.proxy import UartInterface, M1N1Proxy
from m1n1.proxyutils import bootstrap_port

# Verify the target before any privileged debug-USB operation.
iface = UartInterface()
proxy = M1N1Proxy(iface)
bootstrap_port(iface, proxy)
try:
    if proxy.get_chipid() != 0x8142 or proxy.iodev_whoami() != 1:
        raise RuntimeError('M5 KIS identity mismatch')
finally:
    iface.dev.close()

from host_auth import run_scoped_vdm


def run_vdm(args, check=False):
    if args != ['debugusb']:
        raise RuntimeError('Guest requested an unexpected VDM operation')
    return run_scoped_vdm(args, check=check)


def install_stackdump():
    signal.signal(signal.SIGQUIT, lambda signum, frame: traceback.print_stack(frame))


install_stackdump()
hv_module._run_macvdmtool = run_vdm
hv_module.set_sigquit_stackdump_handler = install_stackdump
os.chdir(source)
print('M5_GUEST_QUALIFICATION: exact kernel, single CPU, scoped KIS', flush=True)
runpy.run_path(str(source / 'proxyclient/tools/run_guest.py'), run_name='__main__')
