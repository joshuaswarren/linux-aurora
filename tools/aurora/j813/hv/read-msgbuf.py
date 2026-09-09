"""Save the paused M5 guest's XNU log with a current-image provenance check."""
import os
import json
import hashlib
from pathlib import Path
import struct
import time
from m1n1 import xnutools


def save_msgbuf(hv):
    if (int(hv.u.adt['/chosen'].chip_id) != 0x8142 or hv.ctx is None or
            not getattr(hv, 'j813_kernelcache_verified', False)):
        raise RuntimeError('Requires the paused, hash-verified M5 guest')
    va = xnutools.find_msgbuf_va(hv.macho)
    if va is None:
        raise RuntimeError('No unique native msgbuf structure')
    expected = hv.guest_base + va - hv.macho.vmin
    translated = hv.p.hv_translate(va, False, False)
    previous = getattr(hv, 'm5_verified_msgbuf', None)
    physical_fallback = translated != expected
    if physical_fallback and not (
            translated == 0 and hv.ctx.spsr.M == 0 and previous and
            previous['static_buffer'] and previous['header_va'] == hex(va) and
            previous['guest_base'] == hex(hv.guest_base)):
        raise RuntimeError('Msgbuf header is not in the uploaded guest image')
    def read_uploaded(address, length):
        if not hv.macho.vmin <= address < address + length <= hv.macho.vmax:
            raise RuntimeError('Static msgbuf read exceeds the verified uploaded image')
        return hv.iface.readmem(hv.guest_base + address - hv.macho.vmin, length)
    header = read_uploaded(va, 24) if physical_fallback else hv.readmem(va, 24)
    magic, size, bufx, bufr, pointer = struct.unpack('<4IQ', header)
    if magic != 0x63061 or not 0 < size <= 0x100000 or bufx > size or bufr > size:
        raise RuntimeError('Invalid live msgbuf header')
    canonical = pointer | (0xff << 56)
    static_buffer = hv.macho.vmin <= canonical and canonical + size <= hv.macho.vmax
    if physical_fallback and (not static_buffer or size != previous['size'] or
                              pointer != int(previous['buffer_va'], 16)):
        raise RuntimeError('EL0 msgbuf differs from the previously verified static buffer')
    if static_buffer and not physical_fallback:
        points = {0, size - 1}
        points.update(range(0x4000 - (canonical & 0x3fff), size, 0x4000))
        for off in sorted(points):
            physical = hv.p.hv_translate(pointer + off, False, False)
            if physical != hv.guest_base + canonical + off - hv.macho.vmin:
                raise RuntimeError('Msgbuf buffer does not match the uploaded image')
    directory = Path(__file__).resolve().parent / 'logs/msgbuf'
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(directory, 0o700)
    if physical_fallback:
        # Kernel mappings can be absent at EL0. Reuse only this controller's
        # previously translated static image; never infer a new dynamic PA.
        raw = read_uploaded(canonical, size)
        path = directory / f'xnu_msgbuf_el0_{time.time_ns()}.txt'
        text = (raw[bufx:] + raw[:bufx]).replace(b'\0', b'').decode('utf-8', 'replace')
        for output, value in ((path, text.encode()), (path.with_suffix('.raw'), raw)):
            with open(os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as out:
                out.write(value)
    else:
        path = xnutools.dump_msgbuf(hv.u, hv.macho, dump_dir=directory, tail_lines=0)
    if path is None:
        raise RuntimeError('Msgbuf dump failed')
    os.chmod(path, 0o600)
    os.chmod(path.with_suffix('.raw'), 0o600)
    # Preserve the ring indices even if the general decoder prints only the
    # short prefix after a wrap. Raw bytes remain the primary artifact.
    raw = path.with_suffix('.raw').read_bytes()
    receipt = dict(magic=magic, size=size, bufx=bufx, bufr=bufr,
                   header_va=hex(va), buffer_va=hex(pointer),
                   guest_base=hex(hv.guest_base), static_buffer=static_buffer,
                   physical_fallback_from_verified_static=physical_fallback,
                   raw_sha256=hashlib.sha256(raw).hexdigest())
    with open(os.open(path.with_suffix('.json'),
                      os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as output:
        json.dump(receipt, output, indent=2)
        output.write('\n')
    if not physical_fallback:
        hv.m5_verified_msgbuf = receipt if static_buffer else None
    print('M5_MSGBUF_PROVENANCE', 'uploaded header and static buffer' if static_buffer
          else 'uploaded header; dynamic buffer provenance requires separate review')
    print('M5_MSGBUF_SAVED_PRIVATE', str(path))


if 'hv' in globals():
    save_msgbuf(hv)
