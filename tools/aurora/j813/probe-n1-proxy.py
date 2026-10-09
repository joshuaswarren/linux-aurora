#!/usr/bin/env python3
"""Validate the N1 PCIe handoff independently before uploading Linux."""
import os
from pathlib import Path
import runpy
import sys

root = Path(__file__).resolve().parent
os.environ['M1N1DEVICE'] = '/dev/cu.kis-01100000-ch-0'
os.environ['M1N1TIMEOUT'] = '20'
sys.path.insert(0, str(root / 'm1n1/proxyclient'))
from m1n1.proxy import UartInterface, M1N1Proxy
from m1n1.proxyutils import ProxyUtils
iface = UartInterface()
p = M1N1Proxy(iface)
assert p.get_chipid() == 0x8142
u = ProxyUtils(p)
runpy.run_path(str(root / 'prepare-n1-pcie.py'), init_globals={'p': p, 'u': u, 'iface': iface})
iface.dev.close()
