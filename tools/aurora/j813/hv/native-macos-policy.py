"""Bind a native M5 LocalPolicy capture to a RAM-only guest manifest.

The original signed IM4M bytes are retained. Their SHA384 must match the
native IODeviceTree's lpol measurement, and spih must match the captured
personalized system-cryptex ticket. No signature or root check is patched.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import uuid

VUID = 'AAE717A6-D9CB-4900-8695-FC62B8DD4C26'
SOURCE_SHA256 = '10618f65ad78a1bba62ce114bf08dad85d1fcf9a07ef7606481d85371b892896'
SOURCE_SHA256_AFTER29 = '2bfbc1790e06f45e8cb29c9a7b041067849b7115e570f0cc78739042cd4a9a0b'


def validate_capture(directory):
    directory = Path(directory)
    raw = (directory / 'IODeviceTree.plist').read_bytes()
    if hashlib.sha256(raw).hexdigest() not in (SOURCE_SHA256, SOURCE_SHA256_AFTER29):
        raise RuntimeError('Native M5 IODeviceTree capture changed')
    tree = plistlib.loads(raw)
    pending, found = ([tree] if isinstance(tree, dict) else list(tree)), []
    while pending:
        node = pending.pop()
        pending.extend(node.get('IORegistryEntryChildren', []))
        if node.get('IORegistryEntryName') == 'chosen':
            found.append(node)
    if len(found) != 1:
        raise RuntimeError('Native chosen node is ambiguous')
    chosen = found[0]
    children = {n['IORegistryEntryName']: n for n in chosen['IORegistryEntryChildren']}
    for key in ('boot-uuid', 'apfs-preboot-uuid'):
        if chosen[key].rstrip(b'\0').decode('ascii') != VUID:
            raise RuntimeError('Native source selects a different system volume')
    props = children['manifest-properties']
    chip, ecid = (int.from_bytes(props[k], 'little') for k in ('CHIP', 'ECID'))
    if chip != 0x8142:
        raise RuntimeError('Native source is not M5')
    manifests = list((directory / 'components').glob('*.im4m'))
    policies = [p for p in (directory / 'LocalPolicy').glob('*.img4')
                if '.recovery.' not in p.name]
    if len(manifests) != 1 or len(policies) != 1:
        raise RuntimeError('Native policy capture is ambiguous')
    manifest = manifests[0].read_bytes()
    measured = children['secure-boot-hashes']['lpol']
    if not 0 < len(manifest) <= 0x4000 or hashlib.sha384(manifest).digest() != measured:
        raise RuntimeError('Signed LocalPolicy does not match native boot measurement')
    # Parse the signed policy offline with the installed IMG4 decoder.
    result = subprocess.run(['ipsw', 'img4', 'info', '--json', str(policies[0])],
                            capture_output=True, check=True, timeout=15)
    policy = json.loads(result.stdout)['manifest']['properties']
    b64 = lambda key: base64.b64decode(policy[key], validate=True)
    if (policy['CHIP'] != chip or policy['ECID'] != ecid or
            uuid.UUID(bytes=b64('vuid')) != uuid.UUID(VUID) or
            b64('nsih') != chosen['boot-manifest-hash'] or
            b64('lpnh').hex() != policies[0].stem.lower()):
        raise RuntimeError('Native policy identity and boot chain disagree')
    if not policy.get('lobo') or policy.get('love') != '25.7.76.0.0,0':
        raise RuntimeError('Native policy is not the captured 25G76 installation')
    boot_objects_path = chosen['boot-objects-path'].rstrip(b'\0').decode('ascii')
    if boot_objects_path != f'/{VUID}/boot/{b64("nsih").hex().upper()}':
        raise RuntimeError('Native boot objects path differs from the signed volume and boot hash')
    tickets = [p for p in (directory / 'cryptex').glob('*.im4m')
               if hashlib.sha384(p.read_bytes()).digest() == b64('spih')]
    if len(b64('spih')) != 48 or len(tickets) != 1:
        raise RuntimeError('Native cryptex ticket does not match signed spih')
    # Exact 25G76 _darwin_el2_query_property_digest (0xfffffe0009593440)
    # reads property 0x19 from /chosen/asmb/lp-spih. Merely publishing the
    # original IM4M does not populate this firmware handoff field.
    asmb = children['asmb']
    if (asmb.get('lp-spih') != b64('spih') or
            len(asmb.get('lp-stng', b'')) != 8 or
            int.from_bytes(asmb['lp-stng'], 'little') != policy['stng']):
        raise RuntimeError('Native asmb cryptex hash or generation differs from signed policy')
    # Exact AppleImage4 0xfffffe00095927b4 adds lp-smb0 and lp-smb1.
    # Exact dyld 0x85850..0x858cc selects the generic ticket at levels 1/2.
    # Both measured native captures use level 0 and a personalized ticket.
    if any(k in asmb or policy.get(k[3:], False) for k in ('lp-smb0', 'lp-smb1')):
        raise RuntimeError('This handoff requires the measured full-security native policy')
    native_asmb = {}
    for key, value in asmb.items():
        if not key.startswith('lp-'):
            continue
        signed = policy.get(key[3:])
        if isinstance(signed, str):
            expected = base64.b64decode(signed, validate=True)
            typed = value
        elif isinstance(signed, int):
            expected = signed.to_bytes(len(value), 'little')
            typed = signed
        else:
            raise RuntimeError('Native asmb contains an unbound LocalPolicy field')
        if value != expected:
            raise RuntimeError('Native asmb field differs from signed LocalPolicy')
        native_asmb[key] = typed
    # Ensure the policy envelope contains the exact extracted signed manifest.
    if policies[0].read_bytes().count(manifest) != 1:
        raise RuntimeError('Extracted manifest differs from policy envelope')
    return dict(manifest=manifest, measurement=measured, chip=chip, ecid=ecid,
                volume_group=VUID, spih=b64('spih'), stng=policy['stng'],
                secure_boot_level=0, asmb=native_asmb,
                boot_manifest_hash=chosen['boot-manifest-hash'],
                boot_objects_path=boot_objects_path)


def resident_policy(hv):
    """Read and bind the original handoff; do not combine policies and tickets."""
    chosen = hv.u.adt['/chosen']
    manifests = hv.u.adt['/chosen/boot-object-manifests']
    base = int(chosen.dram_base)
    end = base + int(hv.u.ba.mem_size_actual)
    data = {}
    for key in ('lpol', 'ibot'):
        address, size = map(int, manifests._properties[key])
        if not 0 < size <= 0x4000 or not base <= address < address + size <= end:
            raise RuntimeError('Resident policy manifest is outside bounded DRAM')
        data[key] = hv.iface.readmem(address, size)
        if len(data[key]) != size:
            raise RuntimeError('Incomplete resident policy manifest read')
    measurement = hv.u.adt['/chosen/secure-boot-hashes']._properties['lpol']
    boot_hash = chosen._properties['boot-manifest-hash']
    if (hashlib.sha384(data['lpol']).digest() != measurement or
            hashlib.sha384(data['ibot']).digest() != boot_hash or
            hv.u.adt['/chosen/asmb']._properties['lp-nsih'] != boot_hash):
        raise RuntimeError('Resident LocalPolicy and iBoot handoff do not agree')
    return data['lpol'], measurement, boot_hash


def stage_policy(hv, capture, retain_resident=False, native_boot_objects=False):
    if (int(hv.u.adt['/chosen'].chip_id) != 0x8142 or
            not getattr(hv, 'j813_kernelcache_verified', False)):
        raise RuntimeError('Native policy staging requires exact M5 kernel')
    if hv.u.adt['/chosen/manifest-properties']._properties['ECID'] != capture['ecid']:
        raise RuntimeError('Native policy belongs to a different physical M5')
    if str(hv.adt['/chosen'].boot_uuid).upper() != VUID:
        raise RuntimeError('Select the full macOS volume before staging its policy')
    node = hv.adt['/chosen/boot-object-manifests']
    if 'lpol' not in node._properties or len(node._properties['lpol']) != 2:
        raise RuntimeError('Inherited LocalPolicy handoff is missing')
    asmb = hv.adt['/chosen/asmb']
    if not isinstance(asmb._properties.get('lp-stng'), int):
        raise RuntimeError('Inherited LocalPolicy generation is not an integer')
    if capture.get('secure_boot_level') != 0:
        raise RuntimeError('Native policy security level was not verified')
    if native_boot_objects:
        expected = f'/{VUID}/boot/{capture["boot_manifest_hash"].hex().upper()}'
        if capture.get('boot_objects_path') != expected:
            raise RuntimeError('Native boot objects path was not bound to the captured installation')
    manifest, measurement, boot_hash = (capture['manifest'], capture['measurement'],
                                       capture['boot_manifest_hash'])
    if retain_resident:
        # Guest33's SEP OPLA held the native LP while IPIS held the resident
        # generic iBoot ticket: the signed nsih and SHA384(IPIS) disagreed.
        # This opt-in experiment retains the physical boot's measured pair
        # and identity, while isolating the native cryptex query mirrors.
        manifest, measurement, boot_hash = resident_policy(hv)
    hv.m5_native_lpol_manifest = manifest
    hv.adt['/chosen/secure-boot-hashes']._properties['lpol'] = measurement
    # Retaining stub level2 in the Image4 query mirrors would
    # make dyld deliberately open the generic ticket, which cannot match
    # the native policy's signed personalized-ticket hash. Guest ADT only.
    for key in list(asmb._properties):
        if key.startswith('lp-'):
            del asmb._properties[key]
    asmb._properties.update(capture['asmb'])
    hv.adt['/chosen']._properties['boot-manifest-hash'] = boot_hash
    if native_boot_objects:
        # Guest36 proved centaurid cached firmware_present=0 and followed
        # its no-firmware shutdown path. Root selection changed the UUIDs
        # but left boot-objects-path pointing into the m1n1 stub. This
        # opt-in RAM-only comparison changes that filesystem path only;
        # the matched LP/iBoot blobs and measurements remain as above.
        hv.adt['/chosen']._properties['boot-objects-path'] = capture['boot_objects_path']
        print('M5_NATIVE_BOOT_OBJECTS_PATH: verified native installation path staged in guest ADT')
    if retain_resident:
        print('M5_RESIDENT_POLICY_RETAINED: measured LP/iBoot pair and boot identity; native cryptex mirrors')
    else:
        print('M5_NATIVE_POLICY_STAGED: signed manifest and personalized cryptex ticket verified')


if 'hv' in globals():
    stage_policy(hv, validate_capture(os.environ['M5_NATIVE_POLICY_CAPTURE']),
                 retain_resident=os.environ.get('M5_RETAIN_RESIDENT_BOOT_POLICY') == '1',
                 native_boot_objects=os.environ.get('M5_NATIVE_BOOT_OBJECTS_PATH') == '1')
elif __name__ == '__main__':
    capture = validate_capture(sys.argv[1])
    print('M5 native policy: chip, ECID, volume, build, boot hash, lpol measurement and spih/ticket match')
    print('Manifest SHA256', hashlib.sha256(capture['manifest']).hexdigest())
