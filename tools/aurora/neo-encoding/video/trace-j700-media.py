"""Passive ProRes/JPEG register survey on the verified J700 macOS guest.

Only logs existing guest writes to dedicated media MMIO apertures. Shared
PMGR registers and RAM are excluded. Control changes are consumed through
the active hypervisor diagnostic callback, never through a second proxy.
"""
import json
import os
from pathlib import Path
import time
import struct
import hashlib
from m1n1.hw.dart import DART
from m1n1.hv.types import TraceMode
from m1n1.utils import irange

if int(hv.u.adt['/chosen'].chip_id) != 0x8140 or hv.u.adt.model != 'Mac17,5':
    raise RuntimeError('Media survey requires J700/T8140')
_media_dir = Path(os.environ['J700_AGX_BOOT_TRACE_LOG']).parent
_media_log = (_media_dir / 'media-mmio.jsonl').open('x', buffering=1)
_media_count = 0
_media_limit = 100000
_media_ident = 'J700AdditionalMedia'
_media_phase = 'boot'
_media_active = True
_media_apr_state = {}
_media_apr_captures = 0
_media_apr_last_head = 0
_media_apr_dart = None
_media_apr_seen = set()
_media_jpeg_state = {}
_media_jpeg_seen = set()
_media_jpeg_dart = None


def _media_dma_read(iova, size, dart=None):
    if not 0 < size <= 1024 * 1024:
        raise ValueError('Media DMA snapshot exceeds its bound')
    dart = dart or _media_apr_dart
    dart.dart.invalidate_cache()
    ranges = dart.iotranslate(0, iova, size)
    for address, length in ranges:
        if address is None or not hv.ram_base <= address < address + length <= hv.ram_base + hv.ram_size:
            raise ValueError('Media DMA translation is outside physical RAM')
    return b''.join(iface.readmem(address, length) for address, length in ranges)


def _media_apr_capture(head):
    global _media_apr_captures, _media_apr_dart, _media_apr_last_head
    previous_head, _media_apr_last_head = _media_apr_last_head, head
    # A fresh macOS session resets the ring in hardware without writing a
    # zero DR_HEAD. Its first 384-byte entry still begins at offset zero.
    if head == 0x180 and previous_head >= head:
        previous_head = 0
    if not head or _media_phase in _media_apr_seen or _media_apr_captures >= 24:
        return
    _media_apr_seen.add(_media_phase)
    _media_apr_captures += 1
    prefix = f'apr-{_media_apr_captures:02d}'
    try:
        size = _media_apr_state.get(0x100, 0)
        address = (_media_apr_state.get(0x108, 0) << 32) | _media_apr_state.get(0x104, 0)
        if not address or not 0 < size <= 256 * 1024:
            raise ValueError('APR ring has an unexpected address or size')
        if _media_apr_dart is None:
            # Reading existing guest page tables; no DART initialization or writes.
            _media_apr_dart = DART.from_adt(hv.u, '/arm-io/dart-apr')
        raw = _media_dma_read(address, size)
        (_media_dir / f'{prefix}-ring.bin').write_bytes(raw)
        (_media_dir / f'{prefix}-regs.json').write_text(json.dumps(
            {hex(k): hex(v) for k, v in _media_apr_state.items()}, indent=2) + '\n')
        _media_emit('apr-ring', capture=prefix, head=hex(head), previous_head=hex(previous_head), iova=hex(address),
            size=size, sha256=hashlib.sha256(raw).hexdigest(), file=f'{prefix}-ring.bin')
        # The existing ProRes descriptor uses these plane pointer slots. Keep
        # unknown versions as raw ring evidence; failures never alter the guest.
        # DR_TAIL advances in hardware; a write-only mirror cannot track it.
        # The previous published head identifies this submission's entry.
        tail = previous_head & 0xffffff
        end = head & 0xffffff
        if 0 <= tail < end <= size and 0x180 <= end - tail <= 0x400:
            descriptor = raw[tail:end]
            height = struct.unpack_from('<H', descriptor, 0x1a)[0]
            for plane, (name, offset) in enumerate([('luma', 0x28), ('chroma', 0x38), ('alpha', 0x48)]):
                if name == 'alpha' and not struct.unpack_from('<I', descriptor)[0] & 0x100:
                    continue
                iova = struct.unpack_from('<Q', descriptor, offset)[0]
                if not iova:
                    continue
                try:
                    stride = struct.unpack_from('<H', descriptor, 0x20 + 2 * plane)[0] * 64
                    header_iova = struct.unpack_from('<Q', descriptor, offset + 8)[0]
                    base = min(iova, header_iova) if header_iova else iova
                    end = max(iova + stride * height, header_iova + 16384 if header_iova else 0)
                    # IOVA pointers need not start at a page boundary; rounding
                    # this length can cross the final mapped page unnecessarily.
                    length = end - base
                    payload = _media_dma_read(base, length)
                    filename = f'{prefix}-{name}.bin'
                    (_media_dir / filename).write_bytes(payload)
                    _media_emit('apr-plane', capture=prefix, plane=name, iova=hex(iova),
                        buffer_iova=hex(base), header_iova=hex(header_iova), stride=stride,
                        size=len(payload), file=filename, sha256=hashlib.sha256(payload).hexdigest())
                except Exception as exc:
                    _media_emit('apr-plane-unavailable', capture=prefix, plane=name, error=repr(exc))
    except Exception as exc:
        _media_emit('apr-capture-error', capture=prefix, error=repr(exc))


def _media_jpeg_capture():
    global _media_jpeg_dart
    if _media_phase in _media_jpeg_seen or len(_media_jpeg_seen) >= 8:
        return
    _media_jpeg_seen.add(_media_phase)
    prefix = f'jpeg-{len(_media_jpeg_seen):02d}'
    try:
        # T8140 observed 64-bit address pairs, unlike the old 32-bit map.
        address = _media_jpeg_state[0x90] | _media_jpeg_state[0x94] << 32
        second = _media_jpeg_state[0xa0] | _media_jpeg_state[0xa4] << 32
        end = _media_jpeg_state[0xb0] | _media_jpeg_state[0xb4] << 32
        if not address <= second <= end or not 0 < end + 1 - address <= 1024 * 1024:
            raise ValueError('Unexpected JPEG input range')
        if _media_jpeg_dart is None:
            _media_jpeg_dart = DART.from_adt(hv.u, '/arm-io/dart-jpeg0-h17p')
        payload = _media_dma_read(address, end + 1 - address, _media_jpeg_dart)
        filename = f'{prefix}-input.bin'
        (_media_dir / filename).write_bytes(payload)
        (_media_dir / f'{prefix}-regs.json').write_text(json.dumps(
            {hex(k): hex(v) for k,v in _media_jpeg_state.items()}, indent=2) + '\n')
        _media_emit('jpeg-input', capture=prefix, iova=hex(address), second_iova=hex(second),
            size=len(payload), file=filename, sha256=hashlib.sha256(payload).hexdigest())
    except Exception as exc:
        _media_emit('jpeg-capture-error', capture=prefix, error=repr(exc))


def _media_emit(kind, **values):
    _media_log.write(json.dumps(dict(kind=kind, phase=_media_phase,
        host_monotonic_ns=time.monotonic_ns(), **values)) + '\n')


def _media_poll():
    global _media_phase, _media_active
    path = _media_dir / 'media-control.json'
    if not path.exists():
        return
    value = json.loads(path.read_text())
    if 'phase' in value:
        assert isinstance(value['phase'], str) and 1 <= len(value['phase']) <= 100
        if _media_phase != value['phase']:
            _media_phase = value['phase']
            _media_emit('phase-change')
    if value.get('op') == 'capture-apr-ring':
        _media_apr_capture(_media_apr_state.get(0x10c, 0))
        path.unlink()
    if value.get('op') == 'disarm' and _media_active:
        hv.clear_tracers(_media_ident)
        _media_active = False
        _media_emit('disarmed', events=_media_count)


def _media_callback(name, base, index):
    def hit(evt):
        global _media_count, _media_active
        if not _media_active:
            return
        _media_count += 1
        _media_emit('mmio', seq=_media_count, node=name, region=index,
            cpu=int(evt.flags.CPU), pc=hex(int(evt.pc)), address=hex(int(evt.addr)),
            offset=hex(int(evt.addr) - base), width=1 << int(evt.flags.WIDTH),
            data=hex(int(evt.data)), op='W' if evt.flags.WRITE else 'R')
        if name == 'apr' and index == 0 and evt.flags.WRITE:
            offset = int(evt.addr) - base
            _media_apr_state[offset] = int(evt.data)
            if offset == 0x10c:
                _media_apr_capture(int(evt.data))
        if name == 'jpeg0' and index == 0 and evt.flags.WRITE:
            offset = int(evt.addr) - base
            _media_jpeg_state[offset] = int(evt.data)
            if offset == 0x1004 and int(evt.data) == 1:
                _media_jpeg_capture()
        if _media_count == _media_limit:
            _media_emit('limit-reached', complete=False)
            hv.clear_tracers(_media_ident)
            _media_active = False
    return hit


_media_topology = {'target': 'J700/T8140', 'arm_io_names': [], 'nodes': []}
for node in hv.adt['/arm-io']:
    _media_topology['arm_io_names'].append(node.name)
    compatible = str(node.getprop('compatible') or '')
    if not any(word in (node.name + ' ' + compatible).lower() for word in ['apr', 'prores', 'jpeg', 'scodec']):
        continue
    regions = []
    if node.getprop('reg') is not None:
        for index in range(len(node.reg)):
            base, size = map(int, node.get_reg(index))
            selected = 0x200000000 <= base and base + size <= 0x300000000 and 0 < size <= 32 * 1024**2
            regions.append(dict(index=index, base=hex(base), size=hex(size), traced=selected))
            if selected:
                mode = TraceMode.SYNC if node.name in ('apr', 'jpeg0') and index == 0 else TraceMode.ASYNC
                callback = _media_callback(node.name, base, index)
                hv.add_tracer(irange(base, size), _media_ident, mode,
                    read=callback if node.name == 'jpeg0' and index == 0 else None, write=callback)
    properties = {}
    for key in ['compatible', 'interrupts', 'clock-gates', 'power-gates', 'clock-ids', 'iommu-parent', 'AAPL,phandle', 'tunable-static', 'tunable-common']:
        value = node.getprop(key)
        if value is not None:
            properties[key] = value.hex() if isinstance(value, bytes) else str(value)
    _media_topology['nodes'].append(dict(name=node.name, regions=regions, properties=properties))
(_media_dir / 'media-topology.json').write_text(json.dumps(_media_topology, indent=2) + '\n')
hv.j700_ave_command_poll = _media_poll
_media_emit('armed', nodes=[n['name'] for n in _media_topology['nodes']], limit=_media_limit)
print('J700_MEDIA_SURVEY_ARMED', flush=True)
