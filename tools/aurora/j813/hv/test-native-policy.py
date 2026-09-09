"""Verify the measured native capture and reject cross-device guest staging."""
from pathlib import Path
import runpy
import sys
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / 'm1n1/proxyclient'))
from m1n1.adt import load_adt

helper = runpy.run_path(str(ROOT / 'native-macos-policy.py'))
capture = helper['validate_capture'](ROOT / 'logs/native-macos-after29')
helper['validate_capture'](ROOT / 'logs/native-macos-20260909')
source = max((ROOT / 'logs/adt-snapshots').glob('*.adt'), key=lambda p: p.stat().st_mtime)
source_bytes = source.read_bytes()
adt = load_adt(source_bytes)
assert adt['/chosen'].boot_uuid == helper['VUID']
hv = SimpleNamespace(u=SimpleNamespace(adt=load_adt(source_bytes)), adt=adt,
                     j813_kernelcache_verified=True)
before = adt.build()
helper['stage_policy'](hv, capture)
after = adt.build()
assert len(after) > 0
assert hv.m5_native_lpol_manifest == capture['manifest']
assert adt['/chosen/secure-boot-hashes']._properties['lpol'] == capture['measurement']
roundtrip = load_adt(after)
assert roundtrip['/chosen/asmb']._properties['lp-spih'] == capture['spih']
assert roundtrip['/chosen/asmb']._properties['lp-stng'] == capture['stng']
assert not any(k in roundtrip['/chosen/asmb']._properties for k in ('lp-smb0', 'lp-smb1'))
assert {k: v for k, v in roundtrip['/chosen/asmb']._properties.items()
        if k.startswith('lp-')} == capture['asmb']
assert roundtrip['/chosen']._properties['boot-manifest-hash'] == capture['boot_manifest_hash']
for change in ('chip', 'ecid', 'profile', 'volume'):
    test = SimpleNamespace(u=SimpleNamespace(adt=load_adt(source_bytes)),
                           adt=load_adt(source_bytes), j813_kernelcache_verified=True)
    if change == 'chip':
        test.u.adt['/chosen'].chip_id = 0x8140
    elif change == 'ecid':
        test.u.adt['/chosen/manifest-properties']._properties['ECID'] ^= 1
    elif change == 'profile':
        test.j813_kernelcache_verified = False
    else:
        test.adt['/chosen'].boot_uuid = '154559C3-3E4F-42C3-B06B-16A9F72CEA78'
    try:
        helper['stage_policy'](test, capture)
    except RuntimeError:
        assert not hasattr(test, 'm5_native_lpol_manifest')
    else:
        raise AssertionError(f'{change} mismatch accepted')
print('Native policy checks passed: measured signed bytes, ADT serialization, device/profile/volume isolation')

# Hardware-captured guest33 inputs prove the regression: the native LP
# was paired with an iBoot ticket whose SHA384 did not equal its nsih.
import base64
import hashlib
import json
import subprocess

evidence = ROOT / 'logs/guest-33'
resident_bytes = (evidence / 'resident.adt').read_bytes()
lpol = (evidence / 'resident-lpol.bin').read_bytes()
ibot = (evidence / 'sep-ibot.bin').read_bytes()
parsed = json.loads(subprocess.run(
    ['ipsw', 'img4', 'im4m', 'info', '--json', str(evidence / 'resident-lpol.bin')],
    capture_output=True, check=True).stdout)
assert base64.b64decode(parsed['properties']['nsih']) == hashlib.sha384(ibot).digest()
assert capture['boot_manifest_hash'] != hashlib.sha384(ibot).digest()

def resident_guest(corrupt=None):
    original = load_adt(resident_bytes)
    blobs = {'lpol': lpol, 'ibot': ibot}
    if corrupt in blobs:
        blobs[corrupt] = bytes([blobs[corrupt][0] ^ 1]) + blobs[corrupt][1:]
    addresses = {int(original['/chosen/boot-object-manifests']._properties[k][0]): value
                 for k, value in blobs.items()}
    def readmem(address, size):
        value = addresses[address]
        assert size == len(value)
        return value
    if corrupt == 'nsih':
        original['/chosen/asmb']._properties['lp-nsih'] = bytes(48)
    return SimpleNamespace(u=SimpleNamespace(adt=original, ba=SimpleNamespace(mem_size_actual=0x400000000)),
                           adt=load_adt(source_bytes), iface=SimpleNamespace(readmem=readmem),
                           j813_kernelcache_verified=True)

test = resident_guest()
helper['stage_policy'](test, capture, retain_resident=True)
assert test.m5_native_lpol_manifest == lpol
result = load_adt(test.adt.build())
assert result['/chosen']._properties['boot-manifest-hash'] == hashlib.sha384(ibot).digest()
assert result['/chosen/secure-boot-hashes']._properties['lpol'] == hashlib.sha384(lpol).digest()
assert result['/chosen/asmb']._properties['lp-spih'] == capture['spih']
for corruption in ('lpol', 'ibot', 'nsih'):
    test = resident_guest(corruption)
    before = test.adt.build()
    try:
        helper['stage_policy'](test, capture, retain_resident=True)
    except RuntimeError:
        assert test.adt.build() == before
        assert not hasattr(test, 'm5_native_lpol_manifest')
    else:
        raise AssertionError(f'Corrupt resident {corruption} accepted')
print('Resident policy checks passed: signed nsih/ticket regression, matched handoff, corrupt-input rejection')

# The native firmware path is a separate opt-in comparison. Preserve the
# measured resident LP/iBoot identity that removed the guest33 SEP panic.
test = resident_guest()
old_path = test.adt['/chosen']._properties['boot-objects-path']
helper['stage_policy'](test, capture, retain_resident=True)
assert test.adt['/chosen']._properties['boot-objects-path'] == old_path
helper['stage_policy'](test, capture, retain_resident=True, native_boot_objects=True)
result = load_adt(test.adt.build())
assert result['/chosen']._properties['boot-objects-path'] == capture['boot_objects_path']
assert result['/chosen']._properties['boot-manifest-hash'] == hashlib.sha384(ibot).digest()
assert result['/chosen/secure-boot-hashes']._properties['lpol'] == hashlib.sha384(lpol).digest()
assert test.m5_native_lpol_manifest == lpol
# A later guest snapshot can already contain the native path. Use the
# captured resident stub for rejection, not the newest guest's valid path.
resident_path = load_adt(resident_bytes)['/chosen']._properties['boot-objects-path']
assert resident_path != capture['boot_objects_path']
for bad_path in (resident_path, '/another-volume/boot/invalid', capture['boot_objects_path'] + '/..'):
    test = resident_guest()
    before = test.adt.build()
    try:
        helper['stage_policy'](test, {**capture, 'boot_objects_path': bad_path},
                               retain_resident=True, native_boot_objects=True)
    except RuntimeError:
        assert test.adt.build() == before
        assert not hasattr(test, 'm5_native_lpol_manifest')
    else:
        raise AssertionError('Unbound native boot objects path accepted')
print('Boot objects path checks passed: native path binding, ADT roundtrip, unchanged SEP pair, rejection before mutation')
