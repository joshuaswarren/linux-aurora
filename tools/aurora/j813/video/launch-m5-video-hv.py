#!/usr/bin/env python3
"""Launch the qualified M5 carrier's macOS guest with bounded AVE observers.

Run after the carrier-32 RAM chainload has released the M5 KIS link. This
launcher neither installs a resident image nor changes the startup disk.
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
args = parser.parse_args()
os.umask(0o077)
output = args.output.resolve()
output.mkdir(mode=0o700)
expected = {
    'kernel/25G76__Mac17,3/kernelcache.release.Mac17,2_3_4':
        '43263b4f4b7cd29cb0cc487ba944b5bf0a8f3b24b8b1cad875b796d9af026189',
    'artifacts/carrier-32/m1n1.bin':
        '71d486a98638b238e1680200bd0d2da2748db19cd1f5a959cff4550289b098cd',
    'artifacts/carrier-32/m1n1.elf':
        '870732cd794cff77848122bdc28aee246e1120640be3f355ad83432f948bf69d',
    'm1n1/build/m1n1.bin':
        '71d486a98638b238e1680200bd0d2da2748db19cd1f5a959cff4550289b098cd',
    'm1n1/build/m1n1-raw.elf':
        '93e7126a1ccaa35d93464a865b7f50084bbb8ed160fab514506d458477024f39',
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
    'M5_RETAIN_RESIDENT_BOOT_POLICY': '1',
    'M5_NATIVE_BOOT_OBJECTS_PATH': '1',
    'M5_ISOLATE_EXTERNAL_DISPLAY': '1',
    'M5_ISOLATE_INTERNAL_DISPLAY': '1',
    'M5_ISOLATE_CAMERA': '1',
    # Guest38 required this exact-kernel, signature-guarded diagnostic to
    # leave the inherited CPU energy accumulator intact under the HV.
    'M5_SKIP_CPU_ENERGY_RESTORE': '1',
    'M5_NVME_CQ_DEPTH_64': '1',
    'J700_HV_QUIET_BOOT': '1',
    'M5_VIDEO_TRACE_DIR': str(output),
    'M5_N1_TRACE': '1', 'M5_SEP_TRACE': '1', 'M5_PMP_TRACE': '0',
    'M5_PMGR_TRACE': '0', 'M5_CLPC_SAMPLER_SHADOW': '0',
    'M5_AVE_TRACE_DART': '0', 'M5_AVE_CAPTURE_INPUTS': '0',
})
command = [sys.executable, '-u', str(hvroot / 'run-guest.py'),
           '--strip-node', 'error-handler', '--strip-node', 'mtp',
           '--strip-node', 'ane',
           '-m', str(hvroot / 'bootstrap-m5.py'),
           '-m', str(root / 'video/trace-j813-ave-rings.py'),
           '-m', str(root / 'video/poll-j813-video.py'),
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
