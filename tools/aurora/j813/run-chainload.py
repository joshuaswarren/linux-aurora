#!/usr/bin/env python3
"""RAM-chainload the pinned J813 m1n1 build over the dedicated M5 KIS port."""
import os
from pathlib import Path
import runpy
import sys

root = Path(__file__).resolve().parent
os.environ['M1N1DEVICE'] = '/dev/cu.kis-01100000-ch-0'
os.environ['M1N1TIMEOUT'] = '15'
sys.path.insert(0, str(root / 'm1n1/proxyclient'))
runpy.run_path(str(root / 'kis-transport.py'))['install']()
from m1n1.proxy import UartInterface, M1N1Proxy
iface = UartInterface()
assert M1N1Proxy(iface).get_chipid() == 0x8142, 'Refusing non-M5 target'
iface.dev.close()
sys.argv = ['chainload.py', str(root / 'm1n1/build-j813-linux/m1n1.macho')]
runpy.run_path(str(root / 'm1n1/proxyclient/tools/chainload.py'), run_name='__main__')
