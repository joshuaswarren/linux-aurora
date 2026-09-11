#!/usr/bin/env python3
"""Run the qualified M5 macOS guest with its own GPU startup observers.

Uses the already qualified headless, single-CPU carrier. Resident Stage1 and
startup-disk policy stay intact; perform its guarded RAM chainload first.
"""
import argparse
import hashlib
import os
from pathlib import Path
import sys

root = Path(__file__).resolve().parent.parent
hvroot = root / 'hv'
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('output', type=Path)
parser.add_argument('--carrier', choices=('32', '33'), default='32',
                    help='Exact RAM carrier profile (33 tests the empty-PTE partial-update fix)')
parser.add_argument('--capture-init', action='store_true',
                    help='Capture each firmware graph before its initialization doorbell is published')
parser.add_argument('--capture-startup', action='store_true',
                    help='Also capture the first 24 native GPU work/control publications')
parser.add_argument('--trace-host-gpu-writes', action='store_true',
                    help='Observe native CPU writes across the GPU aperture, excluding ASC mailboxes')
parser.add_argument('--native-fw-trace-mask', type=lambda value: int(value, 0), default=0,
                    help='Set the primary host trace input before initialization (allowed bits: 0x203)')
args = parser.parse_args()
if args.native_fw_trace_mask < 0 or args.native_fw_trace_mask & ~0x203:
    parser.error('--native-fw-trace-mask accepts only bits 0x203')
if args.native_fw_trace_mask and not (args.capture_init or args.capture_startup):
    parser.error('--native-fw-trace-mask requires --capture-init or --capture-startup')
os.umask(0o077)
output = args.output.resolve()
output.mkdir(mode=0o700)
carriers = {
    '32': (
        '71d486a98638b238e1680200bd0d2da2748db19cd1f5a959cff4550289b098cd',
        '870732cd794cff77848122bdc28aee246e1120640be3f355ad83432f948bf69d',
        '93e7126a1ccaa35d93464a865b7f50084bbb8ed160fab514506d458477024f39'),
    '33': (
        'b60a077cf595693e5995df8b9bccdced3d4215f2f21fc80e6565c8d58d06665a',
        '572855dd68f3582a216ced435bfb3156f4992e12a3325e86eee24d4e6606d929',
        '406efb462ed05660152a5976de3f630e20f0a35d683c443c15f75342964decd1'),
}
carrier_bin, carrier_elf, carrier_raw_elf = carriers[args.carrier]
expected = {
    'kernel/25G76__Mac17,3/kernelcache.release.Mac17,2_3_4':
        '43263b4f4b7cd29cb0cc487ba944b5bf0a8f3b24b8b1cad875b796d9af026189',
    f'artifacts/carrier-{args.carrier}/m1n1.bin': carrier_bin,
    f'artifacts/carrier-{args.carrier}/m1n1.elf': carrier_elf,
    'm1n1/build/m1n1.bin': carrier_bin,
    'm1n1/build/m1n1-raw.elf': carrier_raw_elf,
}
for name, digest in expected.items():
    if hashlib.sha256((hvroot / name).read_bytes()).hexdigest() != digest:
        raise RuntimeError('Pinned M5 artifact mismatch: ' + name)
inventory = hvroot / 'logs/volume-inventory/m5-1788956350201548000.json'
policy = hvroot / 'logs/native-macos-after29'
if not inventory.is_file() or not policy.is_dir():
    raise RuntimeError('Qualified native macOS volume/policy evidence is missing')
os.environ.update({
    'M5_MACOS_VOLUME_INVENTORY': str(inventory),
    'M5_NATIVE_POLICY_CAPTURE': str(policy),
    'M5_RETAIN_RESIDENT_BOOT_POLICY': '1', 'M5_NATIVE_BOOT_OBJECTS_PATH': '1',
    'M5_ISOLATE_EXTERNAL_DISPLAY': '1', 'M5_ISOLATE_INTERNAL_DISPLAY': '1',
    'M5_ISOLATE_CAMERA': '1', 'M5_SKIP_CPU_ENERGY_RESTORE': '1',
    'M5_NVME_CQ_DEPTH_64': '1', 'J700_HV_QUIET_BOOT': '1',
    'M5_GPU_TRACE_DIR': str(output),
    'M5_GPU_CAPTURE_INIT': '1' if args.capture_init or args.capture_startup else '0',
    'M5_GPU_CAPTURE_STARTUP': '1' if args.capture_startup else '0',
    'M5_GPU_TRACE_HOST_WRITES': '1' if args.trace_host_gpu_writes else '0',
    'M5_GPU_NATIVE_FW_TRACE_MASK': hex(args.native_fw_trace_mask),
    'M5_N1_TRACE': '1', 'M5_SEP_TRACE': '1', 'M5_PMP_TRACE': '0',
    'M5_PMGR_TRACE': '0', 'M5_CLPC_SAMPLER_SHADOW': '0',
})
command = [sys.executable, '-u', str(hvroot / 'run-guest.py'),
           '--strip-node', 'error-handler', '--strip-node', 'mtp', '--strip-node', 'ane',
           '-m', str(hvroot / 'bootstrap-m5.py'),
           '-m', str(root / 'gpu/trace-m5-gpu-boot.py'),
           '-l', str(output / 'hv.log'), '--',
           str(hvroot / 'kernel/25G76__Mac17,3/kernelcache.release.Mac17,2_3_4'),
           'CAEnableMedina=0', '-vm_compressor_wk_sw', 'cpus=1', 'clpc=0',
           '-nobsdmgroot', 'wdt=-1', 'sprr_tpro=0', 'sprr_tpro_pagers=0',
           '-v', 'msgbuf=1048576']
log = os.open(output / 'console.log', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
os.dup2(log, 1)
os.dup2(log, 2)
os.close(log)
os.execv(command[0], command)
