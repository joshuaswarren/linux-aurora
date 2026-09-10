# SPDX-License-Identifier: MIT
"""Identify the measured M5 firmware while retaining every executable byte.

Two cold-boot captures differed only in the eight-byte values of the STKG
and PACE boot TLVs. Their tags, lengths, offsets and all remaining bytes are
part of the identity. This is identity evidence, not runtime qualification.
"""
import hashlib
import struct

PROFILES = {
    'gfx-asc': (0x4c000, (0x45d09, 0x45dec),
        '89faa8be0d9a5affcaec1aeacfef8b159d04fe92da9a70a2158711b16e5059c5'),
    'gfx1-asc': (0x48000, (0x45841, 0x45924),
        '215f90712af2e4094e0c7566e5c3a29ce7a99c76ed75dffd6794a8d66fc5c9bb'),
}


def qualify_text(role, payload):
    if role not in PROFILES:
        raise ValueError('Unknown M5 firmware role')
    size, offsets, expected = PROFILES[role]
    if len(payload) != size:
        raise ValueError('M5 loaded TEXT size changed')
    canonical = bytearray(payload)
    for tag, offset in zip((b'GKTS', b'ECAP'), offsets):
        header = tag + struct.pack('<I', 8)
        if canonical[offset:offset + 8] != header:
            raise ValueError('M5 boot TLV tag or length changed')
        canonical[offset + 8:offset + 16] = bytes(8)
    digest = hashlib.sha256(canonical).hexdigest()
    if digest != expected:
        raise ValueError('Unrecognized M5 firmware TEXT identity')
    return dict(profile='M5-RTKit-3255.160.4.release', role=role,
                canonical_sha256=digest, boot_tlv_value_bytes_excluded=16,
                runtime_qualified=False)
