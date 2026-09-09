#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
import sys, pathlib, os, gzip, hashlib
M1N1_DIR = pathlib.Path(os.environ.get(
    "M1N1_DIR", pathlib.Path(__file__).resolve().parent / "m1n1"))
sys.path.insert(0, str(M1N1_DIR / "proxyclient"))

import argparse, time

parser = argparse.ArgumentParser(description='Mach-O loader for m1n1')
parser.add_argument('-q', '--quiet', action="store_true", help="Disable framebuffer")
parser.add_argument('-n', '--no-sepfw', action="store_true", help="Do not preserve SEPFW")
parser.add_argument('-c', '--call', action="store_true", help="Use call mode")
parser.add_argument('--vector-only', action="store_true",
                    help="Send P_VECTOR and exit after its immediate reply")
parser.add_argument('--capture-seconds', type=float, default=0,
                    help="Capture raw console for this long after --vector-only")
parser.add_argument('--capture-log', type=pathlib.Path,
                    help="Write the raw --vector-only console capture here")
parser.add_argument('-r', '--raw', action="store_true", help="Image is raw")
parser.add_argument('-E', '--entry-point', action="store", type=int, help="Entry point for the raw image", default=0x800)
parser.add_argument('-x', '--xnu', action="store_true", help="Set up for chainloading XNU")
parser.add_argument('--j813-safe-base', action="store_true",
                    help="select a RAM-relative T8142 Stage-2 base with overlap guards")
parser.add_argument('--j813-stage2-base', type=lambda value: int(value, 0),
                    help="explicit guarded T8142 Stage-2 base (raw PIE images only)")
parser.add_argument('--j813-source-usb', type=pathlib.Path,
                    help="RAM-load from the topology-verified M5 normal USB proxy, then release it")
parser.add_argument('payload', type=pathlib.Path)
parser.add_argument('boot_args', default=[], nargs="*")
args = parser.parse_args()

# Reconstructed from the pinned org loader. Only the KIS transport buffer is
# padded; the source image on disk is never changed. Patch before setup so
# every RAM write, including the ADT and boot arguments, is bounded.
from m1n1.proxy import UartInterface
from m1n1.proxyutils import ProxyUtils
_writemem = UartInterface.writemem

def kis_writemem(self, addr, data, progress=False, retry=False):
    for offset in range(0, len(data), 1024):
        _writemem(self, addr + offset, data[offset:offset + 1024], False, retry)

def kis_compressed_writemem(self, dest, data, progress=None):
    if not data:
        return
    payload = gzip.compress(data, compresslevel=9, mtime=0)
    original_length = len(payload)
    payload += bytes((-original_length) % 1024)
    print(f"KIS transport: gzip={original_length} padded={len(payload)} chunk=1024", flush=True)
    with self.heap.guarded_malloc(len(payload)) as addr:
        self.iface.writemem(addr, payload)
        old_timeout = self.iface.dev.timeout
        self.iface.dev.timeout = 30
        try:
            actual = self.proxy.gzdec(addr, original_length, dest, len(data))
        finally:
            self.iface.dev.timeout = old_timeout
        if actual != len(data):
            raise RuntimeError(f"gzdec size mismatch: {actual} != {len(data)}")

UartInterface.writemem = kis_writemem
ProxyUtils.compressed_writemem = kis_compressed_writemem

# Fail before touching target memory if the controller cannot build the stub.
from m1n1.asm import ARMAsm
_stub_preflight = ARMAsm("ret", 0x10000000000)
if _stub_preflight.data != bytes.fromhex("c0035fd6"):
    raise RuntimeError("Host assembler/linker preflight failed")
del _stub_preflight

source_device = '/dev/cu.kis-01100000-ch-0'
if args.j813_source_usb:
    import plistlib, subprocess
    if not args.vector_only:
        raise RuntimeError('USB-to-KIS carrier loading requires --vector-only')
    devices = plistlib.loads(subprocess.check_output(
        ['ioreg', '-a', '-l', '-r', '-c', 'IOUSBHostDevice']))
    ports = set()
    def collect_ports(node):
        if isinstance(node, dict):
            if 'IOCalloutDevice' in node:
                ports.add(node['IOCalloutDevice'])
            for child in node.get('IORegistryEntryChildren', []):
                collect_ports(child)
    for node in devices:
        if (node.get('locationID') == 0x01100000 and
                node.get('idVendor') == 0x1209 and node.get('idProduct') == 0x316d):
            collect_ports(node)
    source_device = str(args.j813_source_usb)
    if source_device not in ports:
        raise RuntimeError('Source USB port is not the M5 RID1 m1n1 device')
    print('M5 normal USB source topology verified; destination remains KIS carrier')
os.environ['M1N1DEVICE'] = source_device
os.environ['M1N1TIMEOUT'] = '15'
os.environ['M1N1SKIP_PANIC_COUNTER'] = '1'
from m1n1.setup import *
source_iodev = int(p.iodev_whoami())
expected_iodevs = (4, 5) if args.j813_source_usb else (1,)
if p.get_chipid() != 0x8142 or source_iodev not in expected_iodevs:
    raise RuntimeError('M5 source transport identity check failed')
if not args.j813_safe_base and args.j813_stage2_base is None:
    raise RuntimeError('M5 carrier requires guarded placement')
from m1n1.tgtypes import BootArgs_r1, BootArgs_r2, BootArgs_r3
from m1n1.macho import MachO
from m1n1 import asm
from m1n1.fw_protection import get_m4_secondary_ro_range

J813_UNSAFE_BASE = int(u.base)
# Prefer a destination 16 MiB below this boot's resident image. Low resident
# placements can put that address below RAM; move up to the first RAM page in
# that case. The full-span bounds and all occupied-range checks still decide
# whether the candidate is safe before any upload or ADT push.
J813_SAFE_BASE = max((int(u.base) - 0x1000000) & ~0x3fff,
                     (int(u.ba.phys_base) + 0x3fff) & ~0x3fff)
J813_IMAGE_END_LIMIT = int(u.base)

guarded_j813_base = args.j813_safe_base or args.j813_stage2_base is not None
if guarded_j813_base:
    if int(u.adt["/chosen"].chip_id) != 0x8142:
        raise RuntimeError("guarded J813 Stage-2 placement is T8142-only")
    if not args.raw:
        raise RuntimeError("guarded J813 Stage-2 placement requires a raw PIE m1n1 image")
    if args.j813_stage2_base is not None:
        if args.j813_stage2_base & 0x3fff:
            raise RuntimeError("--j813-stage2-base must be 16 KiB aligned")
        new_base = args.j813_stage2_base
    else:
        # Placement follows this boot's resident base; all inherited memory-map
        # and live-heap overlap guards below still run before the first upload.
        new_base = J813_SAFE_BASE
else:
    new_base = u.base

if args.raw:
    image = args.payload.read_bytes()
    image += b"\x00\x00\x00\x00"
    entry = new_base + args.entry_point
else:
    macho = MachO(args.payload.read_bytes())
    image = macho.prepare_image()
    image += b"\x00\x00\x00\x00"
    entry = macho.entry
    entry -= macho.vmin
    entry += new_base

if args.quiet:
    p.iodev_set_usage(IODEV.FB, 0)

sepfw_start, sepfw_length = 0, 0
preoslog_start, preoslog_size = 0, 0

if not args.no_sepfw:
    sepfw_start, sepfw_length = u.adt["chosen"]["memory-map"].SEPFW
    if hasattr(u.adt["chosen"]["memory-map"], "preoslog"):
        preoslog_start, preoslog_size = u.adt["chosen"]["memory-map"].preoslog

image_size = align(len(image))
sepfw_off = image_size
image_size += align(sepfw_length)
preoslog_off = image_size
image_size += align(preoslog_size)
bootargs_off = image_size
bootargs_size = 0x4000
image_size += bootargs_size

protected_range = get_m4_secondary_ro_range(
    int(u.adt["/chosen"].chip_id), u.ba, u.mrs
)
image_end = new_base + image_size
if image_end > (1 << 64) - 1:
    raise RuntimeError("Chainload image address overflows u64")
if protected_range is not None:
    protected_start, protected_end = protected_range
    if new_base < protected_end and protected_start < image_end:
        raise RuntimeError(
            f"Chainload image 0x{new_base:x}..0x{image_end:x} overlaps "
            f"secondary-RO range 0x{protected_start:x}..0x{protected_end:x}"
        )

# The running m1n1 may own live firmware objects above the stale inherited
# boot-args boundary. Snapshot the remote C heap before u.malloc() adds this
# loader's disposable staging copy, then propagate it to the next stage.
live_heap_high_water = int(p.heapblock_alloc(0))

if guarded_j813_base:
    ram_start = int(u.ba.phys_base)
    ram_end = ram_start + int(u.ba.mem_size_actual)
    if not ram_start <= new_base < image_end <= ram_end:
        raise RuntimeError(
            f"J813 relocated span 0x{new_base:x}..0x{image_end:x} "
            f"is outside RAM 0x{ram_start:x}..0x{ram_end:x}"
        )
    if new_base >= J813_UNSAFE_BASE or image_end > J813_IMAGE_END_LIMIT:
        raise RuntimeError(
            f"J813 relocated span violates safe bounds: "
            f"0x{new_base:x}..0x{image_end:x}"
        )

    occupied = []
    memory_map = u.adt["/chosen"]["memory-map"]
    for name, value in memory_map._properties.items():
        try:
            start, length = map(int, value[:2])
        except (TypeError, ValueError, IndexError):
            continue
        if length:
            occupied.append((start, start + length, str(name)))

    # The active Stage-1 image and every live heap allocation remain valid
    # until the vector handoff.  They must not be overwritten by Stage 2.
    occupied.append((int(u.base), live_heap_high_water, "live-stage1-heap"))
    if protected_range is not None:
        occupied.append((protected_start, protected_end, "secondary-RO"))

    overlaps = [
        (start, end, name)
        for start, end, name in occupied
        if new_base < end and start < image_end
    ]
    if overlaps:
        detail = ", ".join(
            f"{name}=0x{start:x}..0x{end:x}"
            for start, end, name in overlaps
        )
        raise RuntimeError(
            f"J813 relocated span 0x{new_base:x}..0x{image_end:x} "
            f"overlaps live state: {detail}"
        )
    print(
        f"J813 safe Stage-2 span: 0x{new_base:x}..0x{image_end:x} "
        f"(Stage-1 0x{int(u.base):x}, live heap 0x{live_heap_high_water:x})"
    )

print(f"Total region size: 0x{image_size:x} bytes")
image_addr = u.malloc(image_size)

print(f"Loading kernel image (0x{len(image):x} bytes)...")
u.compressed_writemem(image_addr, image, True)
readback = iface.readmem(image_addr, len(image))
if hashlib.sha256(readback).digest() != hashlib.sha256(image).digest():
    raise RuntimeError("Decompressed Stage-2 readback does not match source image")
print("STAGE2_READBACK_SHA256=" + hashlib.sha256(readback).hexdigest(), flush=True)
p.dc_cvau(image_addr, len(image))

if not args.no_sepfw:
    print(f"Copying SEPFW (0x{sepfw_length:x} bytes)...")
    p.memcpy8(image_addr + sepfw_off, sepfw_start, sepfw_length)
    print(f"Adjusting addresses in ADT...")
    u.adt["chosen"]["memory-map"].SEPFW = (new_base + sepfw_off, sepfw_length)
    u.adt["chosen"]["memory-map"].BootArgs = (new_base + bootargs_off, bootargs_size)
    if hasattr(u.adt["chosen"]["memory-map"], "preoslog"):
        p.memcpy8(image_addr + preoslog_off, preoslog_start, preoslog_size)
        u.adt["chosen"]["memory-map"].preoslog = (new_base + preoslog_off, preoslog_size)

if args.xnu:
    def remove_oslog(node):
        names = node.segment_names.split(";")
        try:
            idx = names.index("__OS_LOG")
        except ValueError:
            return
        print(f"Removing __OS_LOG from {node.name}")
        names = names[:idx] + names[idx + 1:]
        node.segment_names = ";".join(names)
        node.segment_ranges = node.segment_ranges[:idx * 32] + node.segment_ranges[32 + idx * 32: ]

    for node in u.adt["/arm-io"]:
        if hasattr(node, "segment_names"):
            remove_oslog(node)

        for nub in node:
            if hasattr(nub, "segment_names"):
                remove_oslog(nub)

# T8142 host RVBAR writes disturb other cores; preserve every inherited RVBAR.
# The initial carrier test leaves all secondary CPUs asleep.
print("T8142: preserving all secondary CPU RVBARs", flush=True)

print("ADT_PUSH_BOUNDARY: any subsequent failure requires a fresh reboot", flush=True)
u.push_adt()

print("Setting up bootargs...")
tba = u.ba.copy()

handoff_high_water = max(
    image_end, live_heap_high_water, int(u.ba.top_of_kernel_data)
)
if protected_range is not None:
    handoff_high_water = max(handoff_high_water, protected_end)
    tba.top_of_kernel_data = handoff_high_water
    print(
        f"Preserving firmware high-water 0x{tba.top_of_kernel_data:x} "
        f"(live-heap 0x{live_heap_high_water:x}, "
        f"inherited 0x{int(u.ba.top_of_kernel_data):x}, "
        f"image-end 0x{image_end:x}, "
        f"secondary-RO 0x{protected_start:x}..0x{protected_end:x})"
    )
else:
    tba.top_of_kernel_data = handoff_high_water
    print(
        f"Preserving firmware high-water 0x{tba.top_of_kernel_data:x} "
        f"(live-heap 0x{live_heap_high_water:x}, "
        f"inherited 0x{int(u.ba.top_of_kernel_data):x}, "
        f"image-end 0x{image_end:x})"
    )

if len(args.boot_args) > 0:
    boot_args = " ".join(args.boot_args)
    if "-v" in boot_args.split():
        tba.video.display = 0
    else:
        tba.video.display = 1
    print(f"Setting boot arguments to {boot_args!r}")
    tba.cmdline = boot_args

if args.xnu:
    # Fix virt_base, since we often install m1n1 with it set to 0 which xnu does not like
    tba.virt_base = 0xfffffe0010000000 + (tba.phys_base & (32 * 1024 * 1024 - 1))
    tba.devtree = u.ba.devtree - u.ba.virt_base + tba.virt_base

if tba.revision <= 1:
    iface.writemem(image_addr + bootargs_off, BootArgs_r1.build(tba))
elif tba.revision == 2:
    iface.writemem(image_addr + bootargs_off, BootArgs_r2.build(tba))
elif tba.revision == 3:
    iface.writemem(image_addr + bootargs_off, BootArgs_r3.build(tba))

# Clean copied instructions to the Point of Coherency, not just the Point of
# Unification. Secondary cores leave reset with caches and the MMU off and may
# be in another cluster, so a PoU-only clean can leave the replacement image in
# the copying core's private/cluster cache hierarchy.
stub = asm.ARMAsm(f"""
        mov x6, x2
        mov x7, x3
1:
        ldp x4, x5, [x1], #16
        stp x4, x5, [x2]
        dc cvac, x2
        add x2, x2, #16
        sub x3, x3, #16
        cbnz x3, 1b

        dsb sy
        mov x2, x6
        mov x3, x7
2:
        ic ivau, x2
        add x2, x2, #16
        sub x3, x3, #16
        cbnz x3, 2b
        dsb sy
        isb

        ldr x1, ={entry}
        br x1
""", image_addr + image_size)

print(f"Reserving and copying stub (0x{stub.len:x} bytes)...")
stub_addr = u.malloc(stub.len)
if stub_addr != stub.addr:
    raise RuntimeError(
        f"Stub allocation is not contiguous: expected 0x{stub.addr:x}, got 0x{stub_addr:x}"
    )

iface.writemem(stub.addr, stub.data)
p.dc_cvac(stub.addr, stub.len)
p.ic_ivau(stub.addr, stub.len)

print(f"Entry point: 0x{entry:x}")

if args.xnu and p.display_is_external():
    if p.display_start_dcp() >= 0:
        p.display_shutdown(0)

if args.call:
    print(f"Shutting down MMU...")
    try:
        p.mmu_shutdown()
    except ProxyCommandError:
        pass
    print(f"Jumping to stub at 0x{stub.addr:x}")
    p.call(stub.addr, new_base + bootargs_off, image_addr, new_base, image_size, reboot=True)
else:
    print(f"Reloading into stub at 0x{stub.addr:x}")
    if args.vector_only:
        captured = bytearray()
        original_unkhandler = iface.unkhandler

        def capture_tty(data):
            captured.extend(data)
            original_unkhandler(data)

        iface.unkhandler = capture_tty
        p.request(p.P_VECTOR, stub.addr, new_base + bootargs_off,
                  image_addr, new_base, image_size)
        print("P_VECTOR accepted; raw console ownership released")
        if args.capture_seconds:
            deadline = time.monotonic() + args.capture_seconds
            old_timeout = iface.dev.timeout
            iface.dev.timeout = 0.1
            try:
                while time.monotonic() < deadline:
                    try:
                        chunk = iface.dev.read(4096)
                    except OSError:
                        break
                    if chunk:
                        captured.extend(chunk)
            finally:
                iface.dev.timeout = old_timeout
                iface.unkhandler = original_unkhandler
            if args.capture_log:
                args.capture_log.write_bytes(captured)
                print(f"Captured {len(captured)} raw console bytes to {args.capture_log}")
        sys.exit(0)
    p.reload(stub.addr, new_base + bootargs_off, image_addr, new_base, image_size)

iface.nop()
print("Proxy is alive again")
