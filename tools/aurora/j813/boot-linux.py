#!/usr/bin/env python3
"""Boot the RAM-only J813 Linux payload through U-Boot; capture native proof."""
import os
from pathlib import Path
import runpy
import sys
import time
import hashlib
import plistlib
import subprocess

root = Path(__file__).resolve().parent
carrier_options = {}
for option in ('carrier-build', 'carrier-sha256'):
    values = [arg.split('=', 1)[1] for arg in sys.argv
              if arg.startswith('--' + option + '=')]
    if len(values) > 1:
        raise ValueError('Specify one ' + option)
    if values:
        carrier_options[option.replace('-', '_')] = values[0]
if carrier_options:
    if len(carrier_options) != 2 or not any(x in sys.argv for x in ('--gpu-display', '--gpu-thermal')):
        raise ValueError('Carrier build and SHA256 require each other and --gpu-display or --gpu-thermal')
    carrier_image = Path(carrier_options['carrier_build']) / 'm1n1.bin'
    if hashlib.sha256(carrier_image.read_bytes()).hexdigest() != carrier_options['carrier_sha256']:
        raise ValueError('Selected carrier SHA256 mismatch')
usb_args = [arg.split('=', 1)[1] for arg in sys.argv if arg.startswith('--j813-source-usb=')]
if len(usb_args) > 1:
    raise ValueError('Specify one M5 USB source')
source_usb = usb_args[0] if usb_args else None
if source_usb:
    devices = plistlib.loads(subprocess.check_output(
        ['ioreg', '-a', '-l', '-r', '-c', 'IOUSBHostDevice']))
    ports = set()
    def collect_ports(node):
        if 'IOCalloutDevice' in node:
            ports.add(node['IOCalloutDevice'])
        for child in node.get('IORegistryEntryChildren', []):
            collect_ports(child)
    for node in devices:
        if (node.get('locationID') == 0x01100000 and
                node.get('idVendor') == 0x1209 and node.get('idProduct') == 0x316d):
            collect_ports(node)
    if source_usb not in ports:
        raise RuntimeError('Source USB port is not the M5 RID1 m1n1 device')
os.environ['M1N1DEVICE'] = source_usb or '/dev/cu.kis-01100000-ch-0'
os.environ['M1N1TIMEOUT'] = '30'
sys.path.insert(0, str(root / 'm1n1/proxyclient'))
if not source_usb:
    runpy.run_path(str(root / 'kis-transport.py'))['install']()
from m1n1.proxy import UartInterface, M1N1Proxy
from m1n1.proxyutils import ProxyUtils, bootstrap_port

iface = UartInterface()
p = M1N1Proxy(iface)
assert p.get_chipid() == 0x8142, 'Refusing non-M5 target'
assert int(p.iodev_whoami()) in ((4, 5) if source_usb else (1,)), 'Wrong M5 transport'
bootstrap_port(iface, p)
u = ProxyUtils(p)
assert u.adt.model == 'Mac17,3'
print('TARGET_CONFIRMED J813 Mac17,3 T8142', flush=True)
if '--gpu-inventory' in sys.argv:
    runpy.run_path(str(root / 'inspect-gpu-topology.py'))['capture_gpu_topology'](u, root / 'logs')

selected = [name for name in ('cpu', 'wifi', 'thermal', 'bluetooth', 'desktop', 'input', 'video', 'video-v4l2', 'gpu', 'display', 'display-probe') if '--' + name in sys.argv]
if len(selected) > 1:
    raise ValueError('Select one RAM payload flavor')
flavor = selected[0] if selected else None
gpu = flavor == 'gpu'
gpu_display = '--gpu-display' in sys.argv
display = flavor in ('display', 'display-probe') or gpu_display
gpu_rtkit_probe = '--gpu-rtkit-probe' in sys.argv
gpu_initdata = '--gpu-initdata' in sys.argv
gpu_thermal = '--gpu-thermal' in sys.argv
if gpu_thermal and (not gpu or not gpu_initdata or gpu_display):
    raise ValueError('--gpu-thermal requires --gpu --gpu-initdata and excludes --gpu-display')
smp = '--smp' in sys.argv
idle_args = [arg.split('=', 1)[1] for arg in sys.argv if arg.startswith('--smp-idle=')]
if idle_args and (not smp or len(idle_args) != 1 or idle_args[0] not in ('nop', 'yield', 'wfi')):
    raise ValueError('--smp-idle=nop|yield|wfi requires --smp')
smp_idle = idle_args[0] if idle_args else 'wfi'
smp_boot_args = [arg.split('=', 1)[1] for arg in sys.argv
                 if arg.startswith('--smp-boot-cpus=')]
if smp_boot_args and (not smp or len(smp_boot_args) != 1 or
                      smp_boot_args[0] not in [str(n) for n in range(1, 11)]):
    raise ValueError('--smp-boot-cpus=1..10 requires --smp')
smp_boot_cpus = int(smp_boot_args[0]) if smp_boot_args else 10
smp_no_wfxt = '--smp-no-wfxt' in sys.argv
if smp_no_wfxt and not smp:
    raise ValueError('--smp-no-wfxt requires --smp')
if gpu_display and (not gpu or not gpu_initdata):
    raise ValueError('--gpu-display requires --gpu --gpu-initdata')
if gpu_initdata and (not gpu or gpu_rtkit_probe):
    raise ValueError('--gpu-initdata requires --gpu and excludes --gpu-rtkit-probe')
if gpu_rtkit_probe and not gpu:
    raise ValueError('--gpu-rtkit-probe requires --gpu')
video = flavor in ('video', 'video-v4l2', 'gpu') and not gpu_thermal
input_enabled = flavor == 'input' or video or display
wifi = flavor is not None and flavor not in ('cpu', 'thermal')
artifacts = root / 'artifacts'
if flavor:
    artifacts /= ('display/probe' if flavor == 'display-probe' else
                  'gpu/native' if gpu else 'video/v4l2' if flavor == 'video-v4l2' else
                  'video/native' if video else 'desktop' if flavor == 'input' else flavor)
if gpu_rtkit_probe or gpu_initdata:
    artifacts /= 'rtkit-probe'
candidate_args = [arg.split('=', 1)[1] for arg in sys.argv if arg.startswith('--gpu-candidate=')]
if candidate_args:
    if len(candidate_args) != 1 or not gpu_initdata:
        raise ValueError('--gpu-candidate=NAME requires --gpu --gpu-initdata')
    candidate = candidate_args[0]
    if not candidate or candidate in ('.', '..') or Path(candidate).name != candidate:
        raise ValueError('GPU candidate must name one directory under artifacts/gpu/native')
    artifacts = root / 'artifacts/gpu/native' / candidate
kernel = (artifacts / 'Image').read_bytes()
if smp_boot_cpus != 10:
    if 'CONFIG_HOTPLUG_CPU=y\n' not in (artifacts / 'kernel.config').read_text():
        raise ValueError('Deferred CPU onlining requires CONFIG_HOTPLUG_CPU=y')
initrd = (artifacts / 'initramfs.cpio.gz').read_bytes()
dtb_dir = root / 'artifacts/wifi' if flavor == 'bluetooth' else artifacts
dtb = (dtb_dir / 't8142-j813.dtb').read_bytes()
if (b'mtp@394600000\0' in dtb) != input_enabled:
    raise ValueError('The MTP device tree requires --input or --video and scoped DAPF preparation')
if (b'apple,t8142-ave\0' in dtb) != video:
    raise ValueError('The M5 encoder DT requires --video and firmware preflight')
if (b'apple,agx-t8142\0' in dtb) != gpu:
    raise ValueError('The M5 GPU DT requires --gpu and boot-local resource preflight')
if (b'apple,t8142-dcp\0' in dtb) != display:
    raise ValueError('The M5 DCP DT requires --display and live mapping preflight')
if flavor == 'thermal' or gpu_thermal:
    if not smp or smp_boot_cpus != 1 or smp_idle != 'wfi' or not smp_no_wfxt:
        raise ValueError('Thermal qualification requires --smp --smp-boot-cpus=1 --smp-idle=wfi --smp-no-wfxt')
    if b'apple,t8142-smc\0' not in dtb or b'thermal-zones\0' not in dtb:
        raise ValueError('Thermal payload must include the J813 SMC and thermal zones')
uboot = (root / 'u-boot/u-boot-nodtb.bin').read_bytes()

def prepare_smp():
    # Opt-in qualification on a freshly reset M5. Removing maxcpus alone is
    # insufficient: kboot drops CPU nodes unless m1n1 has started their cores.
    cpus = list(u.adt['/cpus'])
    cpu_ids = sorted(int(node.cpu_id) for node in cpus)
    boot_ids = [int(node.cpu_id) for node in cpus if node.state == 'running']
    if cpu_ids != list(range(10)) or len(boot_ids) != 1:
        raise RuntimeError('Unexpected J813 CPU topology')
    secondary_ids = [cpu for cpu in cpu_ids if cpu != boot_ids[0]]
    if any(p.smp_is_alive(cpu) for cpu in secondary_ids):
        raise RuntimeError('--smp requires a fresh reset, not already released cores')
    # These are command-state snapshots, not measured CPU clock rates. Do not
    # write DVFS registers while qualifying the independent SMP handoff.
    for label, base in [('E', 0x210e00000), ('P', 0x211e00000)]:
        print(f'J813_CPU_PSTATE_COMMAND {label}={p.read64(base + 0x20020):#x}', flush=True)
    print('J813_SMP_START_BEGIN', flush=True)
    p.smp_start_secondaries()
    alive = [cpu for cpu in secondary_ids if p.smp_is_alive(cpu)]
    missing = [cpu for cpu in secondary_ids if cpu not in alive]
    print(f'J813_SMP_BOOT_CPU={boot_ids[0]} SECONDARIES={alive} MISSING={missing}', flush=True)
    if not alive:
        raise RuntimeError('No M5 secondary cores started; refusing an SMP qualification boot')

if display:
    display_helpers = runpy.run_path(str(root / 'display/dcp-boot-preflight.py'))
    display_before = display_helpers['prepare_display'](u, p)

if video:
    # Keep this boot's preloaded AVE firmware and DAPF configuration intact.
    # The M5 DAPF proxy's default reg index is 1, which is a second DART on
    # AVE; it must not be used as though it were MTP's single DAPF bank.
    runpy.run_path(str(root / 'video/ave-boot-preflight.py'))['prepare_ave_firmware'](
        u, p, iface, root / 'logs' / ('m5-ave-native-firmware-' + str(time.time_ns())))

if gpu:
    gpu_helpers = runpy.run_path(str(root / 'gpu/gpu-boot-preflight.py'))
    gpu_helpers['prepare_gpu'](
        u, iface, root / 'logs' / ('m5-gpu-native-firmware-' + str(time.time_ns())),
        require_known_firmware=gpu_rtkit_probe or gpu_initdata)
    if gpu_initdata:
        p.kboot_set_chosen('aurora,m5-gpu-firmware-profile', 'M5-RTKit-3255.160.4.release')

if wifi:
    # Fail before the large RAM transfer if the resident carrier cannot
    # initialize N1 or a previous proxy session still owns its SMC endpoint.
    runpy.run_path(str(root / 'prepare-n1-pcie.py'), init_globals={
        'p': p, 'u': u, 'iface': iface, 'quiesce_smc_for_linux': gpu_thermal})

kernel_addr = u.memalign(2 * 1024 * 1024, 64 * 1024 * 1024)
initrd_addr = u.memalign(65536, len(initrd))
uboot_addr = u.memalign(2 * 1024 * 1024, len(uboot))
dtb_addr = u.memalign(16384, len(dtb))
for label, address, data in [('Image', kernel_addr, kernel),
                             ('initramfs', initrd_addr, initrd),
                             ('U-Boot', uboot_addr, uboot),
                             ('DTB', dtb_addr, dtb)]:
    print(f'LOAD {label} {address:#x} {len(data)} bytes', flush=True)
    transfer_start = time.monotonic()
    u.compressed_writemem(address, data, True)
    if source_usb:
        digest = hashlib.sha256(iface.readmem(address, len(data))).hexdigest()
        if digest != hashlib.sha256(data).hexdigest():
            raise RuntimeError(f'M5 USB readback mismatch: {label}')
        print(f'USB_READBACK_OK {label} sha256={digest}', flush=True)
    print(f'LOAD_SECONDS {label} {time.monotonic() - transfer_start:.3f}', flush=True)
    p.dc_cvau(address, len(data))
    p.ic_ivau(address, len(data))

bootargs = ('earlycon=dockchannel,0x38812c000 console=tty0 console=ttyDC0 '
            'keep_bootcon ignore_loglevel loglevel=8 initcall_debug '
            'nokaslr rdinit=/init panic=0')
if not smp:
    bootargs += ' maxcpus=1'
else:
    # The SMP kernel preserves M5 registers across WFI/WFIT. YIELD remains
    # available only for diagnosing wait-related failures; it is not sleep.
    bootargs += f' idle={smp_idle}'
    if smp_boot_cpus != 10:
        # Retain all ten DT CPU nodes for first-time onlining from userspace.
        # The selected kernel must provide CONFIG_HOTPLUG_CPU for that test.
        bootargs += f' maxcpus={smp_boot_cpus}'
    if smp_no_wfxt:
        # J700's qualified native SMP path also disables timed waits. Keeping
        # this separate permits comparison with M5's protected WFIT path.
        bootargs += ' arm64.nowfxt'
if '--no-simpledrm' in sys.argv:
    if not gpu_display:
        raise ValueError('--no-simpledrm requires the native GPU/DCP display payload')
    bootargs += ' initcall_blacklist=simpledrm_platform_driver_init'
if '--smp-diagnostics' in sys.argv:
    if not smp:
        raise ValueError('--smp-diagnostics requires --smp')
    bootargs += ' printk.time=1 oops=panic watchdog_thresh=5 workqueue.watchdog_thresh=10'
if flavor == 'desktop' or input_enabled:
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
# Without --smp, m1n1 prunes cold CPUs and keeps the boot CPU selected by MPIDR.
# With --smp, kboot verifies each live core's MPIDR and supplies its release slot.
if input_enabled:
    # Match the scoped preparation in the hardware-qualified J813 MTP
    # handoff. Global DAPF initialization touches unrelated gated blocks.
    assert u.adt['/arm-io/mtp'].get_reg(0) == (0x394600000, 0x88000)
    assert u.adt['/arm-io/dart-mtp'].get_reg(0) == (0x394800000, 0xc000)
    assert u.adt['/arm-io/dart-mtp'].get_reg(1) == (0x394810000, 0x4000)
    result = p.dapf_init('/arm-io/dart-mtp')
    if result:
        raise RuntimeError(f'Scoped MTP DAPF setup failed: {result}')
    print('J813_MTP_DAPF_PREPARED', flush=True)
if smp:
    # Finish device preparation and uploads before releasing CPUs. In
    # particular, do not park M5 secondaries throughout a long KIS transfer.
    prepare_smp()
result = p.kboot_prepare_dt(dtb_addr)
if result:
    raise RuntimeError(f'Device-tree preparation failed: {result}')
print('DEVICE_TREE_PREPARED', flush=True)
if display:
    display_helpers['verify_display_fdt'](u, p, display_before,
                                          integrated_gpu=gpu_display, **carrier_options)
if gpu:
    gpu_helpers['verify_gpu_fdt'](u, p, combined_display=gpu_display, **carrier_options)
if '--prepare-only' in sys.argv:
    sys.exit(0)
if source_usb:
    # Keep the fast upload transport separate from the console, which survives
    # the U-Boot/Linux handoff only on KIS. The prepared RAM and kboot state
    # remain owned by m1n1 while the scoped USB debug connection changes.
    iface.dev.close()
    runpy.run_path(str(root / 'hv/host_auth.py'))['run_scoped_vdm'](['debugusb'], check=True)
    os.environ['M1N1DEVICE'] = '/dev/cu.kis-01100000-ch-0'
    runpy.run_path(str(root / 'kis-transport.py'))['install']()
    for attempt in range(50):
        if Path(os.environ['M1N1DEVICE']).exists():
            break
        time.sleep(0.1)
    iface = UartInterface()
    p = M1N1Proxy(iface)
    assert p.get_chipid() == 0x8142 and int(p.iodev_whoami()) == 1
    bootstrap_port(iface, p)
    u = ProxyUtils(p)
    assert u.adt.model == 'Mac17,3'
    print('M5_FAST_UPLOAD_KIS_HANDOFF_READY', flush=True)
u.msr('DAIF', 0xc0)
print('HANDOFF_TO_UBOOT', flush=True)
p.kboot_boot(uboot_addr)
iface.dev.timeout = 0.2
deadline = time.monotonic() + 150
captured = bytearray()
verified = False
sent = False
panicked = False
while time.monotonic() < deadline:
    chunk = iface.dev.read(65536)
    if not chunk:
        continue
    sys.stdout.buffer.write(chunk)
    sys.stdout.flush()
    captured.extend(chunk)
    if not panicked and b'Kernel panic - not syncing:' in captured:
        panicked = True
        # Retain the panic tail, then release the console promptly so a known
        # failed qualification does not consume the entire shell timeout.
        deadline = min(deadline, time.monotonic() + 3)
    if not sent and b'AURORA_J813_LINUX_INITRAMFS_READY' in captured and b'# ' in captured[-256:]:
        time.sleep(0.5)
        if flavor == 'desktop' or input_enabled:
            now = time.strftime('%Y-%m-%d %H:%M:%S', time.gmtime())
            iface.dev.write(f"date -u -s '{now}'\n".encode())
        iface.dev.write(b"echo AURORA_NATIVE_VERIFY_BEGIN; uname -a; cat /sys/devices/system/cpu/online; cat /proc/uptime; sleep 2; cat /proc/uptime; cat /proc/meminfo | head -5; echo AURORA_NATIVE_VERIFY_DONE\n")
        sent = True
    if sent and b'\nAURORA_NATIVE_VERIFY_DONE\r\n' in captured:
        verified = True
        break
iface.dev.close()
if not verified:
    if panicked:
        raise RuntimeError('Linux kernel panic; inspect captured console')
    raise RuntimeError('No verified Linux initramfs shell within 150 seconds; inspect captured console')
print('\nNATIVE_LINUX_SHELL_VERIFIED', flush=True)
