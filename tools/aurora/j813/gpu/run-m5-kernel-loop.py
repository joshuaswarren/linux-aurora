#!/usr/bin/env python3
"""Reboot the M5 and RAM-boot a candidate over normal USB, then restore Vulkan.

Run with the workbench's Python environment (pyserial required). This uses the
existing scoped RID1 VDM/auth and carrier helpers; resident boot files stay intact.
The optional runtime server serves only the prepared transfer-artifact directory.
"""
import argparse
import fcntl
import json
from pathlib import Path
import plistlib
import re
import shlex
import subprocess
import sys
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--workbench', required=True, type=Path)
    parser.add_argument('--carrier', required=True, type=Path)
    parser.add_argument('--candidate', required=True)
    parser.add_argument('--with-dcp', action='store_true',
                        help='require the combined GPU/DCP DT and normally load both drivers')
    parser.add_argument('--defer-dcp', action='store_true',
                        help='keep the combined DT but defer DCP driver loading for offscreen GPU tests')
    parser.add_argument('--from-m1n1', action='store_true',
                        help='start at the resident m1n1 proxy instead of rebooting Linux')
    parser.add_argument('--server', help='optional HTTP URL of the runtime transfer directory')
    parser.add_argument('--runtime', help='runtime archive filename on the server')
    parser.add_argument('--runtime-sha256')
    args = parser.parse_args()
    if args.defer_dcp and not args.with_dcp:
        parser.error('--defer-dcp requires --with-dcp and its combined DT checks')
    root = args.workbench.resolve()
    carrier = args.carrier.resolve()
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', args.candidate):
        parser.error('candidate must be one directory name')
    runtime = any((args.server, args.runtime, args.runtime_sha256))
    if runtime and not all((args.server, args.runtime, args.runtime_sha256)):
        parser.error('provide all three runtime arguments together')
    if runtime and (not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', args.runtime)
                    or not re.fullmatch(r'[a-f0-9]{64}', args.runtime_sha256)):
        parser.error('invalid runtime filename or SHA256')
    for path in [carrier, root / 'boot-linux.py', root / 'hv/chainload-carrier.py',
                 root / 'hv/host_auth.py', root / 'linux-command.py']:
        if not path.is_file():
            parser.error(f'missing {path}')
    for name in ['Image', 'initramfs.cpio.gz', 't8142-j813.dtb']:
        if not (root / 'artifacts/gpu/native' / args.candidate / name).is_file():
            parser.error(f'candidate missing {name}')
    if runtime:
        args.server = args.server.rstrip('/')
        for name in ['prepare-m5-vulkan-test-root.sh', args.runtime]:
            request = urllib.request.Request(args.server + '/' + name, method='HEAD')
            with urllib.request.urlopen(request, timeout=10) as response:
                if response.status != 200:
                    parser.error(f'runtime server cannot serve {name}')

    logs = root / 'logs'
    logs.mkdir(exist_ok=True)
    lock = (logs / '.m5-kernel-loop.lock').open('a')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    output = logs / time.strftime('m5-fast-loop-%Y%m%d-%H%M%S')
    output.mkdir()
    timings = {}
    started = time.monotonic()

    def run(label, command, timeout=60):
        before = time.monotonic()
        path = output / (label + '.log')
        with path.open('wb') as log:
            result = subprocess.run(command, cwd=root, stdout=log,
                                    stderr=subprocess.STDOUT, timeout=timeout)
        timings[label] = round(time.monotonic() - before, 3)
        print(f'{label}: {timings[label]:.3f}s (rc={result.returncode})', flush=True)
        if result.returncode:
            raise RuntimeError(f'{label} failed; inspect {path}; no automatic retry')

    def linux(label, command, timeout=30):
        run(label, [sys.executable, 'linux-command.py', command,
                    '--timeout', str(timeout)], timeout + 5)

    def usb_port():
        before = time.monotonic()
        while time.monotonic() - before < 30:
            nodes = plistlib.loads(subprocess.check_output(
                ['ioreg', '-a', '-l', '-r', '-c', 'IOUSBHostDevice']))
            ports = set()

            def collect(node):
                if 'IOCalloutDevice' in node:
                    ports.add(node['IOCalloutDevice'])
                for child in node.get('IORegistryEntryChildren', []):
                    collect(child)

            for node in nodes:
                if (node.get('locationID') == 0x01100000 and
                        node.get('idVendor') == 0x1209 and node.get('idProduct') == 0x316d):
                    collect(node)
            if ports:
                # The first CDC interface is the main proxy. Both downstream
                # loaders recheck topology, chip 0x8142 and the source iodev.
                return sorted(ports)[0]
            time.sleep(0.2)
        raise RuntimeError('M5 normal USB did not appear; no other target will be used')

    try:
        if not args.from_m1n1:
            linux('identity', 'set -e; grep -aq apple,j813 /proc/device-tree/compatible; uname -a')
            run('reboot', [sys.executable, '-c',
                "import runpy; h=runpy.run_path('hv/host_auth.py'); "
                "print(h['run_scoped_vdm'](['reboot'], check=True))"])
        source = usb_port()
        run('carrier', [sys.executable, 'hv/chainload-carrier.py', str(carrier),
                       '--raw', '--j813-safe-base', '--vector-only', '--j813-source-usb', source])
        source = usb_port()
        run('boot', [sys.executable, 'boot-linux.py', '--gpu', '--gpu-initdata',
                     '--gpu-inventory', '--gpu-candidate=' + args.candidate,
                     '--j813-source-usb=' + source] + (['--gpu-display'] if args.with_dcp else []))
        if args.with_dcp and not args.defer_dcp:
            linux('dcp', 'set -e; modprobe appledrm live_discovery=1; '
                        'test -d /sys/module/appledrm; '
                        'grep -q "^2560x1664$" /sys/class/drm/card*-*/modes; '
                        'echo M5_DCP_NATIVE_MODE_READY')
        elif args.defer_dcp:
            print('M5_DCP_LOAD_DEFERRED: offscreen GPU tests only', flush=True)
        linux('gpu', 'set -e; modprobe asahi m5_initdata_test=14 m5_fw_trace=512; '
                     'test -c /dev/dri/renderD128; echo M5_GPU_DEVICE_READY')
        if runtime:
            linux('wifi-init', 'set -e; modprobe apple_centauri station_link_addresses=1; '
                  'd=/sys/kernel/debug/apple_centauri/0000:01:00.0; '
                  'for n in 1 2 3; do echo $n > $d/rom_boot; done; ip link set wlan0 up; '
                  'for n in 76 24 15 12; do echo $n > $d/data_probe; done; '
                  'iw dev wlan0 set power_save off; iw reg set US; echo 1 > $d/host_wakeup; '
                  'echo 1 > $d/tx_config; echo 2 > $d/tx_config')
            run('wifi-credentials', [sys.executable, 'configure-wpa.py'])
            linux('wifi-connect', 'set -e; wpa_supplicant -B -i wlan0 -c /run/n1-wpa.conf '
                  '>/tmp/wpa.log 2>&1; sleep 3; '
                  'echo 25 > /sys/kernel/debug/apple_centauri/0000:01:00.0/data_probe; '
                  'udhcpc -i wlan0 -s /bin/n1-udhcpc -n -q -t 5 -T 3 >/tmp/dhcp.log 2>&1')
            url = shlex.quote(args.server + '/prepare-m5-vulkan-test-root.sh')
            parameters = shlex.join([args.server, args.runtime, args.runtime_sha256])
            linux('runtime', f'set -e; wget -q {url} -O /run/prepare-fast.sh; '
                  f'sh /run/prepare-fast.sh {parameters}', timeout=40)
        print('M5_KERNEL_LOOP_READY', flush=True)
    finally:
        timings['total'] = round(time.monotonic() - started, 3)
        (output / 'timings.json').write_text(json.dumps(timings, indent=2) + '\n')
        print(f'Total: {timings["total"]:.3f}s; evidence: {output}', flush=True)


if __name__ == '__main__':
    main()
