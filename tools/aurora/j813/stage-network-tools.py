#!/usr/bin/env python3
"""Stage installed Linux networking binaries and their dynamic dependencies."""
from pathlib import Path
import re
import shutil
import subprocess
import sys

root = Path(sys.argv[1])
for source, name in [('udhcpc.script', 'n1-udhcpc'),
                     ('capture-n1-dhcp.sh', 'n1-dhcp-test')]:
    target = root / 'bin' / name
    shutil.copyfile(Path(__file__).with_name(source), target)
    target.chmod(0o755)
for tool in ['wpa_supplicant', 'wpa_cli', 'iw', 'ip', 'tcpdump', 'lspci', 'modprobe', 'modinfo']:
    binary = shutil.which(tool)
    if not binary:
        raise RuntimeError(f'Missing required tool: {tool}')
    target = root / 'bin' / tool
    target.unlink(missing_ok=True)
    shutil.copy2(binary, target, follow_symlinks=True)
    result = subprocess.run(['ldd', binary], text=True, capture_output=True, check=True)
    for library in re.findall(r'(?<!\S)(/[^\s()]+)', result.stdout):
        destination = root / library.lstrip('/')
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(library, destination, follow_symlinks=True)
    print(f'Staged {tool}')
