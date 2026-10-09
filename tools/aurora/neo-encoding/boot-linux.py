#!/usr/bin/env python3
"""RAM boot J700 through the qualified KIS chainloader and capture its console.

Run after chainloading the separate Linux KIS Stage 2. This script never writes
storage. The resident Stage 1 and the saved macOS HV build are not payloads here.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import struct
import sys
import time

root = Path(__file__).resolve().parent
work = root.parent / 'neo-update'
parser = argparse.ArgumentParser()
parser.add_argument('--stage2-base', type=lambda x:int(x,0),
                    help='Exact placement reported by the completed guarded chainload')
parser.add_argument('--linux-bundle', type=Path, default=root/'artifacts/linux',
                    help='Manifest-verified Linux bundle; defaults to the qualified baseline')
parser.add_argument('--capture-seconds', type=float, default=240)
cpu_mode = parser.add_mutually_exclusive_group()
cpu_mode.add_argument('--single-cpu', action='store_true')
cpu_mode.add_argument('--keep-boot-cpu', action='store_true',
                      help='Start secondaries without switching the original boot CPU')
parser.add_argument('--direct', action='store_true', help='Use m1n1 direct Linux Image handoff')
parser.add_argument('--preflight-only', action='store_true')
args = parser.parse_args()

bundle = args.linux_bundle.resolve()
manifest = json.loads((bundle/'manifest.json').read_text())
for name,item in manifest['files'].items():
    path=bundle/name
    digest=hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda:stream.read(1024*1024),b''):
            digest.update(chunk)
    if path.stat().st_size != item['bytes'] or digest.hexdigest() != item['sha256']:
        raise SystemExit(f'Linux bundle mismatch: {name}')
stage2 = json.loads((root/'artifacts/m1n1-linux/manifest.json').read_text())
for name,item in stage2['files'].items():
    path=root/'artifacts/m1n1-linux'/name
    if path.stat().st_size != item['bytes'] or hashlib.sha256(path.read_bytes()).hexdigest()!=item['sha256']:
        raise SystemExit(f'Linux Stage 2 mismatch: {name}')
uboot_manifest = json.loads((root/'artifacts/u-boot/manifest.json').read_text())
for name,item in uboot_manifest['files'].items():
    path=root/'artifacts/u-boot'/name
    if path.stat().st_size != item['bytes'] or hashlib.sha256(path.read_bytes()).hexdigest()!=item['sha256']:
        raise SystemExit(f'U-Boot mismatch: {name}')
cfg=(root/'artifacts/m1n1-linux/build_cfg.h').read_text()
if '#define T8140_KIS_PROXY' not in cfg or '#define CHAINLOADING' not in cfg:
    raise SystemExit('Wrong Stage 2 build configuration')
if 'T8140_STAGE2_WATCHDOG' in cfg or 'T8140_NORMAL_USB_PROXY' in cfg:
    raise SystemExit('Unexpected watchdog or CDC qualification configuration')
kernel = (bundle/'Image').read_bytes()
if kernel[56:60] != b'ARM\x64':
    raise SystemExit('Not an ARM64 Linux Image')
text_offset,image_size,flags = struct.unpack_from('<QQQ',kernel,8)
if (flags >> 1) & 3 != 2:
    raise SystemExit('Expected 16 KiB kernel pages')
initrd = (bundle/'initramfs.cpio.gz').read_bytes()
dtb = (bundle/'t8140-j700.dtb').read_bytes()
uboot = (root/'artifacts/u-boot/u-boot-nodtb.bin').read_bytes()
if b'apple,j700\0' not in dtb or b'apple,t8140\0' not in dtb:
    raise SystemExit('Wrong device tree')
print(f'BUNDLE_VERIFIED release={manifest["kernel_release"]} image={len(kernel)} '
      f'runtime={image_size} initrd={len(initrd)}',flush=True)
if args.preflight_only:
    raise SystemExit(0)
if args.stage2_base is None:
    raise SystemExit('--stage2-base from the successful chainload is required')

os.environ.setdefault('M1N1DEVICE','/dev/cu.kis-00100000-ch-0')
os.environ['M1N1TIMEOUT']='30'
sys.path.insert(0,str(work/'m1n1-kis/proxyclient'))
runpy.run_path(str(work/'kis-transport.py'))['install']()
from m1n1.proxy import UartInterface, M1N1Proxy
from m1n1.proxyutils import ProxyUtils, bootstrap_port

iface=UartInterface()
p=M1N1Proxy(iface)
bootstrap_port(iface,p)
iface.nop()
if p.get_chipid()!=0x8140 or p.get_base()!=args.stage2_base or p.iodev_whoami()!=1:
    raise RuntimeError('Wrong target, Stage 2 placement, or KIS proxy')
u=ProxyUtils(p)
if u.adt.model!='Mac17,5':
    raise RuntimeError('Expected Mac17,5')
print('TARGET_CONFIRMED J700 T8140 Mac17,5',flush=True)

if b'apple,t8140-ave\0' in dtb:
    prepare_ave = runpy.run_path(str(root/'video/ave-boot-preflight.py'))['prepare_ave_firmware']
    prepare_ave(u, p, iface, root/'logs'/f'{time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())}-ave-firmware')

alignment=2*1024*1024
kernel_span=(max(len(kernel),image_size)+alignment-1)&-alignment
kernel_addr=u.memalign(alignment,kernel_span)
initrd_addr=u.memalign(65536,len(initrd))
uboot_addr=0 if args.direct else u.memalign(alignment,max(len(uboot),16*1024*1024))
dtb_addr=u.memalign(16384,len(dtb))
loads=[('Image',kernel_addr,kernel),('initramfs',initrd_addr,initrd),('DTB',dtb_addr,dtb)]
if not args.direct:
    loads.append(('U-Boot',uboot_addr,uboot))
for label,address,data in loads:
    print(f'LOAD {label} {address:#x} {len(data)} bytes',flush=True)
    u.compressed_writemem(address,data,True)
    p.dc_cvau(address,len(data))
    p.ic_ivau(address,len(data))

bootargs=('earlycon=dockchannel,0x30812c000 console=tty0 console=ttyDC0 '
          'loglevel=8 nokaslr rdinit=/init panic=0')
if args.single_cpu:
    bootargs+=' maxcpus=1'
p.kboot_set_chosen('bootargs',bootargs)
p.kboot_set_chosen('stdout-path','serial0:1500000n8')
p.kboot_set_initrd(initrd_addr,len(initrd))
p.kboot_set_uboot('bootcmd',f'booti {kernel_addr:#x} {initrd_addr:#x}:{len(initrd):#x} $fdtcontroladdr')
p.kboot_set_uboot('bootdelay','0')
p.kboot_set_uboot('baudrate','1500000')
p.kboot_set_uboot('stdin','serial')
p.cpufreq_init()
if not args.single_cpu:
    p.smp_start_secondaries()
    if not args.keep_boot_cpu and p.smp_switch_boot_cpu(0)!=0:
        raise RuntimeError('Could not switch primary CPU to core 0')
if p.kboot_prepare_dt(dtb_addr):
    raise RuntimeError('Device tree preparation failed')
print('DEVICE_TREE_PREPARED',flush=True)
u.msr('DAIF',0xc0)
print('HANDOFF_TO_LINUX' if args.direct else 'HANDOFF_TO_UBOOT',flush=True)
p.kboot_boot(kernel_addr if args.direct else uboot_addr)
iface.dev.timeout=0.2
deadline=time.monotonic()+args.capture_seconds
captured=bytearray()
sent=False
verified=False
while time.monotonic()<deadline:
    chunk=iface.dev.read(65536)
    if not chunk:
        continue
    sys.stdout.buffer.write(chunk)
    sys.stdout.flush()
    captured.extend(chunk)
    if not sent and b'AURORA_NEO_LINUX_INITRAMFS_READY' in captured and b'# ' in captured[-256:]:
        iface.dev.write(b'echo AURORA_NEO_VERIFY_BEGIN; uname -r; cat /sys/devices/system/cpu/online; cat /proc/uptime; sleep 2; cat /proc/uptime; cat /proc/mounts; echo AURORA_NEO_VERIFY_DONE\n')
        sent=True
    if sent and b'\nAURORA_NEO_VERIFY_DONE\r\n' in captured:
        verified=True
        break
iface.dev.close()
if not verified:
    raise RuntimeError('No verified native Linux shell within capture window; inspect console')
print('\nNEO_NATIVE_LINUX_SHELL_VERIFIED',flush=True)
