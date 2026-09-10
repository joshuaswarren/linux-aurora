#!/usr/bin/env python3
"""Boot the RAM-only J813 Linux payload through U-Boot; capture native proof."""
import os
from pathlib import Path
import runpy
import sys
import time

root = Path(__file__).resolve().parent
os.environ['M1N1DEVICE'] = '/dev/cu.kis-01100000-ch-0'
os.environ['M1N1TIMEOUT'] = '30'
sys.path.insert(0, str(root / 'm1n1/proxyclient'))
runpy.run_path(str(root / 'kis-transport.py'))['install']()
from m1n1.proxy import UartInterface, M1N1Proxy
from m1n1.proxyutils import ProxyUtils, bootstrap_port

iface = UartInterface()
p = M1N1Proxy(iface)
assert p.get_chipid() == 0x8142, 'Refusing non-M5 target'
bootstrap_port(iface, p)
u = ProxyUtils(p)
assert u.adt.model == 'Mac17,3'
print('TARGET_CONFIRMED J813 Mac17,3 T8142', flush=True)
if '--gpu-inventory' in sys.argv:
    runpy.run_path(str(root / 'inspect-gpu-topology.py'))['capture_gpu_topology'](u, root / 'logs')

selected = [name for name in ('wifi', 'bluetooth', 'desktop', 'input') if '--' + name in sys.argv]
if len(selected) > 1:
    raise ValueError('Select one payload: --wifi, --bluetooth, --desktop or --input')
flavor = selected[0] if selected else None
wifi = flavor is not None
artifacts = root / 'artifacts'
if flavor:
    artifacts /= 'desktop' if flavor == 'input' else flavor
kernel = (artifacts / 'Image').read_bytes()
initrd = (artifacts / 'initramfs.cpio.gz').read_bytes()
dtb_dir = root / 'artifacts/wifi' if flavor == 'bluetooth' else artifacts
dtb = (dtb_dir / 't8142-j813.dtb').read_bytes()
if (b'mtp@394600000\0' in dtb) != (flavor == 'input'):
    raise ValueError('The MTP device tree requires --input and its scoped DAPF preparation')
uboot = (root / 'u-boot/u-boot-nodtb.bin').read_bytes()

kernel_addr = u.memalign(2 * 1024 * 1024, 64 * 1024 * 1024)
initrd_addr = u.memalign(65536, len(initrd))
uboot_addr = u.memalign(2 * 1024 * 1024, len(uboot))
dtb_addr = u.memalign(16384, len(dtb))
for label, address, data in [('Image', kernel_addr, kernel),
                             ('initramfs', initrd_addr, initrd),
                             ('U-Boot', uboot_addr, uboot),
                             ('DTB', dtb_addr, dtb)]:
    print(f'LOAD {label} {address:#x} {len(data)} bytes', flush=True)
    u.compressed_writemem(address, data, True)
    p.dc_cvau(address, len(data))
    p.ic_ivau(address, len(data))

bootargs = ('earlycon=dockchannel,0x38812c000 console=tty0 console=ttyDC0 '
            'keep_bootcon ignore_loglevel loglevel=8 initcall_debug '
            'maxcpus=1 nokaslr rdinit=/init panic=0')
if flavor in ('desktop', 'input'):
    # Ask DRM fbdev for the conventional 32-bpp desktop surface. SimpleDRM
    # converts its shadow buffer into the firmware's native 10-bit RGB layout.
    bootargs += ' video=2560x1664-32'
p.kboot_set_chosen('bootargs', bootargs)
p.kboot_set_chosen('stdout-path', 'serial0:1500000n8')
p.kboot_set_initrd(initrd_addr, len(initrd))
p.kboot_set_uboot('bootcmd', f'booti {kernel_addr:#x} {initrd_addr:#x}:{len(initrd):#x} $fdtcontroladdr')
p.kboot_set_uboot('bootdelay', '0')
p.kboot_set_uboot('baudrate', '1500000')
p.kboot_set_uboot('stdin', 'serial')
# Keep the initial bring-up on the already-running boot CPU. m1n1 prunes the
# cold CPU nodes and leaves the boot CPU selected by MPIDR.
if wifi:
    runpy.run_path(str(root / 'prepare-n1-pcie.py'), init_globals={'p': p, 'u': u, 'iface': iface})
if flavor == 'input':
    # Match the scoped preparation in the hardware-qualified J813 MTP
    # handoff. Global DAPF initialization touches unrelated gated blocks.
    assert u.adt['/arm-io/mtp'].get_reg(0) == (0x394600000, 0x88000)
    assert u.adt['/arm-io/dart-mtp'].get_reg(0) == (0x394800000, 0xc000)
    assert u.adt['/arm-io/dart-mtp'].get_reg(1) == (0x394810000, 0x4000)
    result = p.dapf_init('/arm-io/dart-mtp')
    if result:
        raise RuntimeError(f'Scoped MTP DAPF setup failed: {result}')
    print('J813_MTP_DAPF_PREPARED', flush=True)
result = p.kboot_prepare_dt(dtb_addr)
if result:
    raise RuntimeError(f'Device-tree preparation failed: {result}')
print('DEVICE_TREE_PREPARED', flush=True)
if '--prepare-only' in sys.argv:
    sys.exit(0)
u.msr('DAIF', 0xc0)
print('HANDOFF_TO_UBOOT', flush=True)
p.kboot_boot(uboot_addr)
iface.dev.timeout = 0.2
deadline = time.monotonic() + 150
captured = bytearray()
verified = False
sent = False
while time.monotonic() < deadline:
    chunk = iface.dev.read(65536)
    if not chunk:
        continue
    sys.stdout.buffer.write(chunk)
    sys.stdout.flush()
    captured.extend(chunk)
    if not sent and b'AURORA_J813_LINUX_INITRAMFS_READY' in captured and b'# ' in captured[-256:]:
        time.sleep(0.5)
        if flavor in ('desktop', 'input'):
            now = time.strftime('%Y-%m-%d %H:%M:%S', time.gmtime())
            iface.dev.write(f"date -u -s '{now}'\n".encode())
        iface.dev.write(b"echo AURORA_NATIVE_VERIFY_BEGIN; uname -a; cat /sys/devices/system/cpu/online; cat /proc/uptime; sleep 2; cat /proc/uptime; cat /proc/meminfo | head -5; echo AURORA_NATIVE_VERIFY_DONE\n")
        sent = True
    if sent and b'\nAURORA_NATIVE_VERIFY_DONE\r\n' in captured:
        verified = True
        break
iface.dev.close()
if not verified:
    raise RuntimeError('No verified Linux initramfs shell within 150 seconds; inspect captured console')
print('\nNATIVE_LINUX_SHELL_VERIFIED', flush=True)
